//! discord-voice-tray — punto de entrada.
//!
//! Orquesta el daemon completo:
//! - Carga config y logging.
//! - Crea el canal `watch<VoiceState>` (inicial `DiscordClosed`) y un
//!   `CancellationToken` compartido para el apagado coordinado.
//! - Lanza dos tareas: el tray SNI ([`tray::tray_task`]) y el bucle de
//!   reconexión IPC ([`ipc::run_ipc_loop`]).
//! - Espera SIGTERM/SIGINT o la cancelación del token (que dispara el ítem
//!   "Salir" del menú). Cualquiera de los tres inicia un apagado limpio.
//!
//! Sin lógica de protocolo aquí: vive en `ipc::client`/`ipc`.

mod config;
mod ipc;
mod state;
mod tray;

use state::VoiceState;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    init_tracing();

    let cfg = match config::Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(error = %e, "no se pudo cargar la configuración; usando opción A por defecto");
            config::Config::default()
        }
    };

    // Canal de estado: arranca en DiscordClosed hasta que la sesión publique.
    let (tx, rx) = watch::channel(VoiceState::DiscordClosed);

    // Token de apagado coordinado: lo comparten tray (Quit), IPC loop y main.
    let cancel = CancellationToken::new();

    // Tarea del tray: registra el SNI y refresca el icono ante cada cambio.
    let tray_handle = tokio::spawn(tray::tray_task(rx, cancel.clone()));

    // Tarea IPC: reconexión con backoff, publica VoiceState en `tx`.
    let ipc_handle = tokio::spawn(ipc::run_ipc_loop(tx, cfg, cancel.clone()));

    // Espera la primera causa de apagado: señal del sistema o token cancelado.
    wait_for_shutdown(&cancel).await;
    tracing::info!("iniciando apagado limpio");

    // Asegura que ambas tareas vean la cancelación (p.ej. si vino por señal).
    cancel.cancel();

    // Espera ordenada de las tareas (cooperan con el token).
    let _ = tray_handle.await;
    let _ = ipc_handle.await;
    tracing::info!("discord-voice-tray finalizado");
}

/// Bloquea hasta la primera de: SIGTERM, SIGINT (Ctrl-C) o token cancelado
/// (Quit del menú del tray).
async fn wait_for_shutdown(cancel: &CancellationToken) {
    // SignalKind no es fiable de instalar en algunos entornos; degradamos a un
    // futuro que nunca completa si falla, dejando el token como única vía.
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(error = %e, "no se pudo instalar handler de SIGTERM");
            None
        }
    };
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(error = %e, "no se pudo instalar handler de SIGINT");
            None
        }
    };

    tokio::select! {
        _ = cancel.cancelled() => {
            tracing::info!("apagado solicitado desde el menú del tray (Quit)");
        }
        _ = async { match sigterm.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending().await } } => {
            tracing::info!("SIGTERM recibido");
        }
        _ = async { match sigint.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending().await } } => {
            tracing::info!("SIGINT recibido");
        }
    }
}

/// Inicializa `tracing-subscriber` respetando `RUST_LOG`, con `info` por defecto.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("discord_voice_tray=info,info"));
    fmt().with_env_filter(filter).init();
}
