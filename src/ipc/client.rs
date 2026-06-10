//! Orquestación de UNA sesión IPC con Discord.
//!
//! Una sesión completa hace, en orden:
//! 1. Descubrir el socket y handshake → esperar `READY`.
//! 2. Autenticar: si hay token cacheado, `AUTHENTICATE` directo; si Discord lo
//!    rechaza, descartar token y ejecutar el flujo `AUTHORIZE` completo (opción A
//!    StreamKit) actualizando el cache.
//! 3. `SUBSCRIBE` independiente a los 3 eventos de voz.
//! 4. `GET_VOICE_SETTINGS` + `GET_SELECTED_VOICE_CHANNEL` para el estado inicial.
//! 5. Loop de eventos: PING→PONG, y cada señal de voz reduce a `VoiceState` y se
//!    publica en el `watch::Sender`.
//!
//! Devuelve `Err` al cerrarse el socket o ante un fallo de auth. NO reintenta:
//! la reconexión con backoff es del lote 3 (en `ipc/mod.rs`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio::time::timeout;

use crate::config::{self, AuthMode, Config, ConfigError, TokenCache};
use crate::state::{self, RawSignals, VoiceState};

use super::protocol::{
    AuthorizeData, Handshake, Opcode, ReadyData, RpcMessage, StreamKitTokenResponse,
    VoiceChannelData, VoiceConnectionStatusData, VoiceSettingsData, CMD_AUTHENTICATE,
    CMD_AUTHORIZE, CMD_GET_SELECTED_VOICE_CHANNEL, CMD_GET_VOICE_SETTINGS, CMD_SUBSCRIBE,
    EVT_VOICE_CHANNEL_SELECT, EVT_VOICE_CONNECTION_STATUS, EVT_VOICE_SETTINGS_UPDATE,
};
use super::socket::{self, Frame, SocketError};

/// Tiempo máximo de espera del evento `READY` tras enviar el handshake.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Tiempo máximo de espera de la respuesta a un comando (AUTHORIZE/AUTHENTICATE/
/// SUBSCRIBE/GET). AUTHORIZE incluye la interacción del usuario con el popup, por
/// eso es generoso.
const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Endpoint de intercambio code→token de StreamKit (opción A).
const STREAMKIT_TOKEN_URL: &str = "https://streamkit.discord.com/overlay/token";

/// Errores de la sesión IPC.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Fallo en el socket o el framing.
    #[error(transparent)]
    Socket(#[from] SocketError),

    /// Error serializando/deserializando un payload JSON.
    #[error("error de (de)serialización JSON: {0}")]
    Json(#[from] serde_json::Error),

    /// Error de config/token cache (no debería abortar la sesión salvo I/O grave).
    #[error("error de configuración: {0}")]
    Config(#[from] ConfigError),

    /// Fallo en la llamada HTTP a StreamKit (timeout o error HTTP).
    #[error("error en el intercambio de token con StreamKit: {0}")]
    TokenExchange(String),

    /// El usuario rechazó el popup de AUTHORIZE, o Discord devolvió error.
    #[error("AUTHORIZE rechazado o fallido: {0}")]
    AuthorizeRejected(String),

    /// No llegó `READY` dentro del tiempo límite.
    #[error("timeout esperando el evento READY de Discord")]
    ReadyTimeout,

    /// Timeout esperando la respuesta a un comando.
    #[error("timeout esperando respuesta al comando {0}")]
    CommandTimeout(&'static str),

    /// Discord cerró la conexión (opcode CLOSE).
    #[error("Discord cerró la conexión")]
    Closed,

    /// Llegó un frame inesperado durante el handshake.
    #[error("frame inesperado durante el handshake: opcode={opcode:?} evt={evt:?}")]
    UnexpectedFrame {
        opcode: Opcode,
        evt: Option<String>,
    },
}

/// Resultado de intentar autenticar con el token cacheado.
enum AuthOutcome {
    /// AUTHENTICATE aceptado.
    Ok,
    /// Discord rechazó el token (cmd con `evt = "ERROR"`); hay que re-AUTHORIZE.
    Rejected,
}

/// Genera un nonce único por proceso: contador atómico + nanos de arranque.
/// Suficiente para correlacionar respuestas RPC; no necesita ser un UUID real.
fn next_nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("dvt-{nanos}-{n}")
}

/// Sesión IPC conectada con handshake completado.
///
/// Posee el `UnixStream` y mantiene las señales crudas acumuladas para reducir
/// el `VoiceState` tras cada evento.
pub struct Client {
    stream: UnixStream,
    client_id: String,
    ready: ReadyData,
    signals: RawSignals,
}

impl Client {
    /// Datos del evento READY recibido en el handshake.
    pub fn ready(&self) -> &ReadyData {
        &self.ready
    }

    /// client_id usado en este handshake.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Descubre el socket, conecta y ejecuta el handshake con el `client_id`
    /// efectivo de la config. Devuelve la sesión lista tras recibir `READY`.
    async fn connect_and_handshake_with(cfg: &Config) -> Result<Client, ClientError> {
        let client_id = cfg.effective_client_id();
        let mut stream = socket::discover_socket().await?;

        // 1. Enviar handshake (opcode 0).
        let handshake = Handshake::new(client_id.clone());
        let payload = serde_json::to_vec(&handshake)?;
        socket::write_frame(&mut stream, &Frame::new(Opcode::Handshake, payload)).await?;

        // 2. Esperar READY (opcode 1) dentro del timeout, respondiendo PINGs.
        let ready = match timeout(READY_TIMEOUT, wait_for_ready(&mut stream)).await {
            Ok(result) => result?,
            Err(_elapsed) => return Err(ClientError::ReadyTimeout),
        };

        Ok(Client {
            stream,
            client_id,
            ready,
            signals: RawSignals::default(),
        })
    }

    /// Ejecuta la sesión completa hasta que el socket se cierra o falla la auth.
    ///
    /// Hace una sola pasada de descubrimiento+handshake+auth+subscribe+GETs y
    /// luego corre el loop de eventos publicando en `tx`. Devuelve `Err` al
    /// perder el socket (el caller decide el backoff).
    pub async fn run_session(
        cfg: &Config,
        tx: &watch::Sender<VoiceState>,
    ) -> Result<(), ClientError> {
        let mut client = Self::connect_and_handshake_with(cfg).await?;
        tracing::info!(
            client_id = client.client_id(),
            rpc_version = ?client.ready().v,
            "handshake completado: READY recibido de Discord"
        );

        client.authenticate(cfg).await?;
        client.subscribe_voice_events().await?;
        client.fetch_initial_state(tx).await?;
        client.event_loop(tx).await
    }

    /// Autentica la sesión. Prefiere el token cacheado; si Discord lo rechaza,
    /// descarta el cache y ejecuta el flujo `AUTHORIZE` completo (opción A).
    async fn authenticate(&mut self, cfg: &Config) -> Result<(), ClientError> {
        if cfg.auth_mode() == AuthMode::OwnApp {
            // Opción B detectada pero NO implementada en el MVP: caemos a A.
            // TODO(lote-2): intercambio estándar con client_secret + refresh.
            tracing::warn!(
                "auth_mode=OwnApp (opción B) detectado pero no soportado en el MVP; \
                 usando flujo StreamKit (opción A)"
            );
        }

        // Intento 1: token cacheado → AUTHENTICATE directo (sin popup).
        if let Some(cache) = config::load_token()? {
            tracing::debug!("token cacheado encontrado; intentando AUTHENTICATE directo");
            match self.try_authenticate(&cache.access_token).await? {
                AuthOutcome::Ok => {
                    tracing::info!("AUTHENTICATE OK con token cacheado");
                    return Ok(());
                }
                AuthOutcome::Rejected => {
                    tracing::warn!("token cacheado rechazado por Discord; descartando y re-autorizando");
                    config::clear_token()?;
                }
            }
        }

        // Intento 2: flujo AUTHORIZE completo (opción A).
        let token = self.authorize_flow().await?;
        match self.try_authenticate(&token).await? {
            AuthOutcome::Ok => {
                config::save_token(&TokenCache {
                    access_token: token,
                })?;
                tracing::info!("AUTHENTICATE OK tras AUTHORIZE; token cacheado (0600)");
                Ok(())
            }
            AuthOutcome::Rejected => Err(ClientError::AuthorizeRejected(
                "AUTHENTICATE rechazado incluso con token recién obtenido".into(),
            )),
        }
    }

    /// Flujo AUTHORIZE → POST StreamKit → access_token.
    async fn authorize_flow(&mut self) -> Result<String, ClientError> {
        let nonce = next_nonce();
        let cmd = RpcMessage {
            cmd: Some(CMD_AUTHORIZE.into()),
            nonce: Some(nonce.clone()),
            args: Some(serde_json::json!({
                "client_id": self.client_id,
                "scopes": ["rpc"],
            })),
            ..Default::default()
        };
        self.send_command(&cmd).await?;
        tracing::info!("AUTHORIZE enviado; esperando aceptación del popup de Discord");

        let reply = self.wait_for_reply(&nonce, AUTHORIZE_TIMEOUT, CMD_AUTHORIZE).await?;
        if reply.evt.as_deref() == Some("ERROR") {
            return Err(ClientError::AuthorizeRejected(describe_error(&reply.data)));
        }
        let data: AuthorizeData = match reply.data {
            Some(v) => serde_json::from_value(v)?,
            None => {
                return Err(ClientError::AuthorizeRejected(
                    "respuesta AUTHORIZE sin data.code".into(),
                ))
            }
        };

        self.exchange_code_for_token(&data.code).await
    }

    /// POST a StreamKit canjeando el `code` por un `access_token`.
    async fn exchange_code_for_token(&self, code: &str) -> Result<String, ClientError> {
        let client = reqwest::Client::new();
        let resp = client
            .post(STREAMKIT_TOKEN_URL)
            .json(&serde_json::json!({ "code": code }))
            .send()
            .await
            .map_err(|e| ClientError::TokenExchange(e.to_string()))?;

        let resp = resp
            .error_for_status()
            .map_err(|e| ClientError::TokenExchange(e.to_string()))?;

        let body: StreamKitTokenResponse = resp
            .json()
            .await
            .map_err(|e| ClientError::TokenExchange(e.to_string()))?;

        tracing::debug!("access_token obtenido de StreamKit");
        Ok(body.access_token)
    }

    /// Envía AUTHENTICATE con el token dado y clasifica la respuesta.
    async fn try_authenticate(&mut self, access_token: &str) -> Result<AuthOutcome, ClientError> {
        let nonce = next_nonce();
        let cmd = RpcMessage {
            cmd: Some(CMD_AUTHENTICATE.into()),
            nonce: Some(nonce.clone()),
            args: Some(serde_json::json!({ "access_token": access_token })),
            ..Default::default()
        };
        self.send_command(&cmd).await?;

        let reply = self
            .wait_for_reply(&nonce, COMMAND_TIMEOUT, CMD_AUTHENTICATE)
            .await?;
        if reply.evt.as_deref() == Some("ERROR") {
            tracing::debug!(detalle = %describe_error(&reply.data), "AUTHENTICATE devolvió ERROR");
            Ok(AuthOutcome::Rejected)
        } else {
            Ok(AuthOutcome::Ok)
        }
    }

    /// Suscribe (uno por uno, con nonce propio) a los 3 eventos de voz.
    async fn subscribe_voice_events(&mut self) -> Result<(), ClientError> {
        for evt in [
            EVT_VOICE_SETTINGS_UPDATE,
            EVT_VOICE_CONNECTION_STATUS,
            EVT_VOICE_CHANNEL_SELECT,
        ] {
            let nonce = next_nonce();
            let cmd = RpcMessage {
                cmd: Some(CMD_SUBSCRIBE.into()),
                evt: Some(evt.into()),
                nonce: Some(nonce.clone()),
                ..Default::default()
            };
            self.send_command(&cmd).await?;
            let reply = self.wait_for_reply(&nonce, COMMAND_TIMEOUT, CMD_SUBSCRIBE).await?;
            if reply.evt.as_deref() == Some("ERROR") {
                tracing::warn!(evento = evt, detalle = %describe_error(&reply.data), "SUBSCRIBE falló");
            } else {
                tracing::debug!(evento = evt, "SUBSCRIBE OK");
            }
        }
        Ok(())
    }

    /// Pide el estado inicial (settings + canal), combina ambas señales y publica
    /// UNA sola vez el estado de arranque.
    async fn fetch_initial_state(
        &mut self,
        tx: &watch::Sender<VoiceState>,
    ) -> Result<(), ClientError> {
        // GET_VOICE_SETTINGS
        let nonce = next_nonce();
        self.send_command(&RpcMessage {
            cmd: Some(CMD_GET_VOICE_SETTINGS.into()),
            nonce: Some(nonce.clone()),
            ..Default::default()
        })
        .await?;
        let reply = self
            .wait_for_reply(&nonce, COMMAND_TIMEOUT, CMD_GET_VOICE_SETTINGS)
            .await?;
        if let Some(v) = reply.data {
            if let Ok(settings) = serde_json::from_value::<VoiceSettingsData>(v) {
                self.signals.mute = settings.mute;
                self.signals.deaf = settings.deaf;
            }
        }

        // GET_SELECTED_VOICE_CHANNEL
        let nonce = next_nonce();
        self.send_command(&RpcMessage {
            cmd: Some(CMD_GET_SELECTED_VOICE_CHANNEL.into()),
            nonce: Some(nonce.clone()),
            ..Default::default()
        })
        .await?;
        let reply = self
            .wait_for_reply(&nonce, COMMAND_TIMEOUT, CMD_GET_SELECTED_VOICE_CHANNEL)
            .await?;
        // data null = sin canal; data con channel_id null también = sin canal.
        let in_channel = match reply.data {
            Some(v) => serde_json::from_value::<VoiceChannelData>(v)
                .map(|c| c.channel_id.is_some())
                .unwrap_or(false),
            None => false,
        };
        self.signals.in_channel = in_channel;
        self.signals.connected = in_channel;

        self.publish(tx);
        Ok(())
    }

    /// Loop de eventos: PING→PONG y señales de voz → reduce → publish. Devuelve
    /// `Err(Closed)` cuando Discord cierra o el socket se rompe.
    async fn event_loop(&mut self, tx: &watch::Sender<VoiceState>) -> Result<(), ClientError> {
        loop {
            let frame = match socket::read_frame(&mut self.stream).await {
                Ok(f) => f,
                Err(SocketError::Io(e)) if is_eof(&e) => return Err(ClientError::Closed),
                Err(e) => return Err(ClientError::Socket(e)),
            };

            match frame.opcode {
                Opcode::Ping => {
                    socket::write_frame(&mut self.stream, &Frame::new(Opcode::Pong, frame.payload))
                        .await?;
                }
                Opcode::Pong => {}
                Opcode::Close => return Err(ClientError::Closed),
                Opcode::Handshake => {
                    tracing::warn!("frame HANDSHAKE inesperado en loop de eventos; ignorado");
                }
                Opcode::Frame => {
                    let msg: RpcMessage = serde_json::from_slice(&frame.payload)?;
                    if self.apply_event(&msg) {
                        self.publish(tx);
                    }
                }
            }
        }
    }

    /// Aplica un evento de voz a `self.signals`. Devuelve `true` si una señal
    /// relevante cambió (y por tanto hay que republicar).
    fn apply_event(&mut self, msg: &RpcMessage) -> bool {
        let evt = match msg.evt.as_deref() {
            Some(e) => e,
            None => return false,
        };
        match evt {
            EVT_VOICE_SETTINGS_UPDATE => {
                if let Some(v) = &msg.data {
                    if let Ok(s) = serde_json::from_value::<VoiceSettingsData>(v.clone()) {
                        self.signals.mute = s.mute;
                        self.signals.deaf = s.deaf;
                        return true;
                    }
                }
                false
            }
            EVT_VOICE_CHANNEL_SELECT => {
                let in_channel = msg
                    .data
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<VoiceChannelData>(v.clone()).ok())
                    .map(|c| c.channel_id.is_some())
                    .unwrap_or(false);
                self.signals.in_channel = in_channel;
                // Entrar a canal implica conexión activa; salir la resetea.
                self.signals.connected = in_channel || self.signals.connected;
                if !in_channel {
                    self.signals.connected = false;
                }
                true
            }
            EVT_VOICE_CONNECTION_STATUS => {
                if let Some(v) = &msg.data {
                    if let Ok(s) = serde_json::from_value::<VoiceConnectionStatusData>(v.clone()) {
                        self.signals.connected = s.is_connected();
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Reduce las señales actuales y publica el `VoiceState` en el watch.
    fn publish(&self, tx: &watch::Sender<VoiceState>) {
        let next = state::reduce(&self.signals);
        if *tx.borrow() != next {
            tracing::info!(estado = ?next, "transición de VoiceState publicada");
        }
        // send_replace publica siempre; los observadores deduplican por valor.
        let _ = tx.send(next);
    }

    /// Serializa y envía un comando RPC (opcode FRAME).
    async fn send_command(&mut self, msg: &RpcMessage) -> Result<(), ClientError> {
        let payload = serde_json::to_vec(msg)?;
        socket::write_frame(&mut self.stream, &Frame::new(Opcode::Frame, payload)).await?;
        Ok(())
    }

    /// Espera la respuesta correlacionada por `nonce`, respondiendo PINGs y
    /// descartando eventos no solicitados que lleguen entremedias.
    async fn wait_for_reply(
        &mut self,
        nonce: &str,
        wait: Duration,
        cmd_name: &'static str,
    ) -> Result<RpcMessage, ClientError> {
        let fut = async {
            loop {
                let frame = socket::read_frame(&mut self.stream).await?;
                match frame.opcode {
                    Opcode::Ping => {
                        socket::write_frame(
                            &mut self.stream,
                            &Frame::new(Opcode::Pong, frame.payload),
                        )
                        .await?;
                    }
                    Opcode::Pong => {}
                    Opcode::Close => return Err(ClientError::Closed),
                    Opcode::Handshake => {}
                    Opcode::Frame => {
                        let msg: RpcMessage = serde_json::from_slice(&frame.payload)?;
                        if msg.nonce.as_deref() == Some(nonce) {
                            return Ok(msg);
                        }
                        // Evento no correlacionado llegado antes de la respuesta:
                        // ignorarlo aquí (el estado inicial se fija con los GETs).
                        tracing::trace!(?msg.evt, "frame sin nonce esperado descartado");
                    }
                }
            }
        };

        match timeout(wait, fut).await {
            Ok(result) => result,
            Err(_) => Err(ClientError::CommandTimeout(cmd_name)),
        }
    }
}

/// Extrae un mensaje legible del `data` de una respuesta `evt = "ERROR"`.
fn describe_error(data: &Option<serde_json::Value>) -> String {
    data.as_ref()
        .and_then(|v| v.get("message"))
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "sin detalle".into())
}

/// `true` si el error de I/O representa un cierre del socket (EOF/reset).
fn is_eof(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        UnexpectedEof | BrokenPipe | ConnectionReset | ConnectionAborted
    )
}

/// Lee frames hasta encontrar el evento `READY`, respondiendo PING→PONG por el
/// camino. Cualquier CLOSE o frame inesperado aborta el handshake con `Err`.
async fn wait_for_ready(stream: &mut UnixStream) -> Result<ReadyData, ClientError> {
    loop {
        let frame = socket::read_frame(stream).await?;
        match frame.opcode {
            Opcode::Ping => {
                socket::write_frame(stream, &Frame::new(Opcode::Pong, frame.payload)).await?;
            }
            Opcode::Pong => {}
            Opcode::Close => return Err(ClientError::Closed),
            Opcode::Frame => {
                let msg: RpcMessage = serde_json::from_slice(&frame.payload)?;
                if msg.evt.as_deref() == Some("READY") {
                    let data = match msg.data {
                        Some(value) => serde_json::from_value(value)?,
                        None => ReadyData {
                            v: None,
                            config: None,
                        },
                    };
                    return Ok(data);
                }
                return Err(ClientError::UnexpectedFrame {
                    opcode: Opcode::Frame,
                    evt: msg.evt,
                });
            }
            Opcode::Handshake => {
                return Err(ClientError::UnexpectedFrame {
                    opcode: Opcode::Handshake,
                    evt: None,
                });
            }
        }
    }
}
