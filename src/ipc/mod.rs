//! Capa IPC con el cliente de Discord.
//!
//! - [`socket`]: descubrimiento del socket y framing de bajo nivel.
//! - [`protocol`]: tipos serde puros (sin I/O).
//! - [`client`]: orquestación de UNA sesión (handshake, eventos).
//! - [`run_ipc_loop`]: bucle externo de reconexión con backoff sobre `client`.

pub mod client;
pub mod protocol;
pub mod socket;

use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::state::VoiceState;
use client::Client;

/// Secuencia de backoff exponencial (segundos): 1, 2, 4, 8, 16, 30 (capped).
/// Se reinicia a 1s tras una sesión que haya llegado a publicar estado real.
const BACKOFF_SECS: [u64; 6] = [1, 2, 4, 8, 16, 30];

/// Bucle externo de reconexión: corre sesiones IPC indefinidamente, publicando
/// [`VoiceState::DiscordClosed`] entre intentos y aplicando backoff exponencial.
///
/// Garantías (spec de reconexión y ciclo de vida):
/// - Al fallar o cerrarse una sesión publica `DiscordClosed` **inmediatamente**,
///   antes de dormir el backoff.
/// - Backoff 1s → 2s → 4s → 8s → 16s → 30s (capped en 30s).
/// - El backoff se **reinicia a 1s** tras una sesión exitosa (una que llegó al
///   loop de eventos), para que un reinicio rápido de Discord reconecte rápido.
/// - Vive indefinidamente sin Discord; solo termina cuando `cancel` se dispara
///   (Quit del menú o señal del sistema). Durante el `sleep` del backoff y
///   durante la sesión responde a la cancelación de inmediato.
///
/// Mecanismo de cancelación: `tokio_util::sync::CancellationToken` (feature `rt`
/// de tokio-util). Elegido frente a un canal `oneshot`/`Notify` propio porque es
/// clonable, observable con `cancelled()` en varios `select!` y ya es la
/// convención del ecosistema tokio; se comparte tal cual con `tray_task`.
pub async fn run_ipc_loop(
    sender: watch::Sender<VoiceState>,
    config: Config,
    cancel: CancellationToken,
) {
    let mut attempt: usize = 0;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Una sesión completa. Una sesión "exitosa" es la que llegó al loop de
        // eventos: en ese punto ya publicó el estado real (algo != DiscordClosed,
        // o DiscordClosed legítimo pero tras conectar). Detectamos el éxito
        // observando si la sesión publicó algún estado durante su vida.
        let _ = sender.send(VoiceState::DiscordClosed); // base para detectar publicación.
        let session = tokio::select! {
            _ = cancel.cancelled() => break,
            res = Client::run_session(&config, &sender) => res,
        };

        // Si run_session llegó a fetch_initial_state/event_loop, habrá publicado
        // al menos una vez un estado real; lo reflejamos para reiniciar backoff.
        let connected = matches!(session, Ok(()))
            || *sender.borrow() != VoiceState::DiscordClosed;

        match session {
            Ok(()) => {
                tracing::info!("sesión IPC finalizada sin error; se reintentará");
            }
            Err(e) => {
                tracing::warn!(error = %e, "sesión IPC terminada; volviendo a DiscordClosed");
            }
        }

        if connected {
            attempt = 0; // sesión válida → backoff reiniciado a 1s.
        }

        // Publicar DiscordClosed INMEDIATAMENTE, antes de dormir el backoff.
        let _ = sender.send(VoiceState::DiscordClosed);

        if cancel.is_cancelled() {
            break;
        }

        let secs = BACKOFF_SECS[attempt.min(BACKOFF_SECS.len() - 1)];
        if attempt < BACKOFF_SECS.len() - 1 {
            attempt += 1;
        }
        tracing::info!(backoff_s = secs, "esperando antes de reintentar la conexión IPC");

        // Backoff interrumpible por cancelación.
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(secs)) => {}
        }
    }

    tracing::debug!("run_ipc_loop terminando (cancelado)");
    // Estado final coherente mientras el proceso se apaga.
    let _ = sender.send(VoiceState::DiscordClosed);
}
