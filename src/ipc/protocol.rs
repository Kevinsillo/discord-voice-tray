//! Tipos serde puros del protocolo RPC de Discord.
//!
//! Este módulo NO realiza ningún I/O: solo define los tipos que se
//! serializan/deserializan sobre el socket. El framing vive en `socket.rs`
//! y la orquestación de la sesión en `client.rs`.

use serde::{Deserialize, Serialize};

/// CLIENT_ID por defecto (Discord StreamKit). Ver PROJECT.md, opción A de auth.
pub const DEFAULT_CLIENT_ID: &str = "207646673902501888";

/// Opcodes del framing del protocolo IPC de Discord.
///
/// Cada frame es `[opcode: u32 LE][length: u32 LE][payload JSON UTF-8]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Opcode {
    /// Primer mensaje tras conectar.
    Handshake = 0,
    /// Comandos y eventos (todo lo demás).
    Frame = 1,
    /// Cierre de conexión.
    Close = 2,
    /// Keepalive.
    Ping = 3,
    /// Respuesta a PING.
    Pong = 4,
}

impl Opcode {
    /// Valor numérico del opcode tal como viaja por el cable.
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    /// Convierte un u32 del cable en un `Opcode`, o `None` si es desconocido.
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Opcode::Handshake),
            1 => Some(Opcode::Frame),
            2 => Some(Opcode::Close),
            3 => Some(Opcode::Ping),
            4 => Some(Opcode::Pong),
            _ => None,
        }
    }
}

/// Payload del handshake (opcode 0): `{"v":1,"client_id":"<CLIENT_ID>"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handshake {
    /// Versión del protocolo. Siempre 1.
    pub v: u32,
    pub client_id: String,
}

impl Handshake {
    /// Construye el handshake para un client_id dado, con `v = 1`.
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            v: 1,
            client_id: client_id.into(),
        }
    }
}

/// Mensaje RPC genérico (opcode 1). Cubre comandos salientes y eventos
/// entrantes; los campos no relevantes para un caso concreto quedan en `None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RpcMessage {
    /// Nombre del comando (p.ej. `AUTHORIZE`, `SUBSCRIBE`) — ausente en eventos puros.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    /// Nombre del evento (p.ej. `READY`, `VOICE_SETTINGS_UPDATE`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evt: Option<String>,
    /// Identificador único de la petición.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    /// Argumentos del comando.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    /// Datos de la respuesta o del evento.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Evento `READY` que Discord envía tras un handshake correcto (opcode 1).
///
/// Llega como un `RpcMessage` con `cmd = "DISPATCH"` y `evt = "READY"`; este
/// tipo modela solo el `data` que nos interesa.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadyData {
    /// Versión del protocolo RPC que ofrece el servidor.
    pub v: Option<u32>,
    /// Información del servidor de Discord (config), que no inspeccionamos aún.
    #[serde(default)]
    #[allow(dead_code)]
    pub config: Option<serde_json::Value>,
}

/// Nombres de los eventos de voz a los que nos suscribimos (PROJECT.md §5).
pub const EVT_VOICE_SETTINGS_UPDATE: &str = "VOICE_SETTINGS_UPDATE";
pub const EVT_VOICE_CONNECTION_STATUS: &str = "VOICE_CONNECTION_STATUS";
pub const EVT_VOICE_CHANNEL_SELECT: &str = "VOICE_CHANNEL_SELECT";

/// Comandos RPC salientes que emite el cliente.
pub const CMD_AUTHORIZE: &str = "AUTHORIZE";
pub const CMD_AUTHENTICATE: &str = "AUTHENTICATE";
pub const CMD_SUBSCRIBE: &str = "SUBSCRIBE";
pub const CMD_GET_VOICE_SETTINGS: &str = "GET_VOICE_SETTINGS";
pub const CMD_GET_SELECTED_VOICE_CHANNEL: &str = "GET_SELECTED_VOICE_CHANNEL";

/// `data` de `AUTHORIZE`: contiene el `code` que se canjea por un token.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizeData {
    pub code: String,
}

/// Respuesta del endpoint de StreamKit: `{"access_token": "..."}`.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamKitTokenResponse {
    pub access_token: String,
}

/// `data` de `VOICE_SETTINGS_UPDATE` y de la respuesta de `GET_VOICE_SETTINGS`.
///
/// Solo modelamos los campos de interés (`mute`, `deaf`); el resto se ignora.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VoiceSettingsData {
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub deaf: bool,
}

/// `data` de `VOICE_CHANNEL_SELECT` y de `GET_SELECTED_VOICE_CHANNEL`.
///
/// `channel_id = null` (o `data` nulo en el GET) significa "sin canal de voz".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VoiceChannelData {
    #[serde(default)]
    pub channel_id: Option<String>,
}

/// `data` de `VOICE_CONNECTION_STATUS`: `state` ∈ {DISCONNECTED, CONNECTING,
/// VOICE_CONNECTED, ...}. Tratamos `DISCONNECTED` como "no conectado".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VoiceConnectionStatusData {
    #[serde(default)]
    pub state: Option<String>,
}

impl VoiceConnectionStatusData {
    /// `true` salvo que el estado sea exactamente `DISCONNECTED`.
    pub fn is_connected(&self) -> bool {
        self.state.as_deref() != Some("DISCONNECTED")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_roundtrip() {
        for op in [
            Opcode::Handshake,
            Opcode::Frame,
            Opcode::Close,
            Opcode::Ping,
            Opcode::Pong,
        ] {
            assert_eq!(Opcode::from_u32(op.as_u32()), Some(op));
        }
        assert_eq!(Opcode::from_u32(99), None);
    }

    #[test]
    fn handshake_serializes_as_expected() {
        let hs = Handshake::new(DEFAULT_CLIENT_ID);
        let json = serde_json::to_value(&hs).unwrap();
        assert_eq!(json["v"], 1);
        assert_eq!(json["client_id"], DEFAULT_CLIENT_ID);
    }

    #[test]
    fn parse_voice_settings_update_mute_deaf() {
        let data: VoiceSettingsData =
            serde_json::from_str(r#"{"mute":true,"deaf":true}"#).unwrap();
        assert!(data.mute);
        assert!(data.deaf);
    }

    #[test]
    fn parse_voice_settings_update_campos_extra_ignorados() {
        // Discord manda muchos más campos; solo nos quedamos con mute/deaf.
        let data: VoiceSettingsData =
            serde_json::from_str(r#"{"mute":false,"deaf":false,"input":{"volume":100}}"#)
                .unwrap();
        assert!(!data.mute);
        assert!(!data.deaf);
    }

    #[test]
    fn parse_voice_channel_select_channel_id_null() {
        let data: VoiceChannelData = serde_json::from_str(r#"{"channel_id":null}"#).unwrap();
        assert!(data.channel_id.is_none());
    }

    #[test]
    fn parse_voice_channel_select_channel_id_presente() {
        let data: VoiceChannelData =
            serde_json::from_str(r#"{"channel_id":"123456789"}"#).unwrap();
        assert_eq!(data.channel_id.as_deref(), Some("123456789"));
    }

    #[test]
    fn parse_voice_connection_status_disconnected() {
        let data: VoiceConnectionStatusData =
            serde_json::from_str(r#"{"state":"DISCONNECTED"}"#).unwrap();
        assert_eq!(data.state.as_deref(), Some("DISCONNECTED"));
        assert!(!data.is_connected());
    }

    #[test]
    fn parse_voice_connection_status_voice_connected() {
        let data: VoiceConnectionStatusData =
            serde_json::from_str(r#"{"state":"VOICE_CONNECTED"}"#).unwrap();
        assert!(data.is_connected());
    }

    #[test]
    fn parse_authorize_data_code() {
        let data: AuthorizeData = serde_json::from_str(r#"{"code":"abc123"}"#).unwrap();
        assert_eq!(data.code, "abc123");
    }

    #[test]
    fn parse_streamkit_token_response() {
        let resp: StreamKitTokenResponse =
            serde_json::from_str(r#"{"access_token":"tok"}"#).unwrap();
        assert_eq!(resp.access_token, "tok");
    }
}
