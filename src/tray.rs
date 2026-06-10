//! Implementación del trait [`ksni::Tray`]: mapeo estado → icono/tooltip/menú.
//!
//! El mapeo `VoiceState` → icono vive SOLO aquí (decisión de arquitectura). El
//! resto del programa habla en términos de `VoiceState`; este módulo es el único
//! que conoce los bytes de los iconos.
//!
//! ## Formato de `icon_pixmap` (ksni 0.3 / StatusNotifierItem)
//!
//! `ksni::Icon { width, height, data }` espera `data` en **ARGB32, network byte
//! order** (big-endian por píxel): cada píxel son 4 bytes en orden A, R, G, B.
//! El crate `image` decodifica PNG a RGBA8 (bytes R, G, B, A). La conversión es
//! rotar cada grupo de 4 bytes una posición a la derecha (`rotate_right(1)`):
//! `[R,G,B,A] → [A,R,G,B]`. Lo confirma la doc del propio `ksni::Icon`
//! (ver ~/.cargo/registry/.../ksni-0.3.4/src/tray.rs, ejemplo con `image`).
//!
//! ## Iconos embebidos
//!
//! Los 8 PNG (4 estados × {22,24}px) se embeben con `include_bytes!` y se
//! decodifican una sola vez al arrancar (`LazyLock`). El binario es
//! autocontenido: no instala temas de iconos. `DiscordClosed` no tiene icono:
//! el ítem está `Passive` (oculto por el panel) y publica un pixmap vacío.

use std::sync::LazyLock;

use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, ToolTip, TrayMethods};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::state::VoiceState;

/// PNG fuente embebidos: (22px, 24px) por estado.
macro_rules! png_pair {
    ($name:literal) => {
        (
            include_bytes!(concat!("../assets/", $name, "-22.png")).as_slice(),
            include_bytes!(concat!("../assets/", $name, "-24.png")).as_slice(),
        )
    };
}

/// Bytes PNG por estado con icono (todos salvo `DiscordClosed`).
const PNG_IDLE: (&[u8], &[u8]) = png_pair!("idle");
const PNG_VOICE_ON: (&[u8], &[u8]) = png_pair!("voice-on");
const PNG_VOICE_MUTED: (&[u8], &[u8]) = png_pair!("voice-muted");
const PNG_VOICE_DEAFENED: (&[u8], &[u8]) = png_pair!("voice-deafened");

/// Conjunto de iconos ARGB32 ya decodificados para un estado (ambos tamaños).
struct DecodedIcon {
    /// 22px y 24px como `ksni::Icon`; el panel elige el que mejor le venga.
    pixmaps: Vec<Icon>,
}

/// Decodifica un PNG RGBA y lo convierte a `ksni::Icon` en ARGB32 big-endian.
fn decode_argb(png: &[u8]) -> Icon {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("PNG de icono embebido inválido (bug de build)");
    let width = img.width() as i32;
    let height = img.height() as i32;
    let mut data = img.into_rgba8().into_vec();
    debug_assert_eq!(data.len() % 4, 0);
    // RGBA (orden del crate image) → ARGB32 network byte order que exige SNI.
    for pixel in data.chunks_exact_mut(4) {
        pixel.rotate_right(1); // [R,G,B,A] → [A,R,G,B]
    }
    Icon {
        width,
        height,
        data,
    }
}

/// Tabla de iconos decodificados, una entrada por estado. Se construye una sola
/// vez (perezosamente) al primer acceso.
static ICONS: LazyLock<[DecodedIcon; 4]> = LazyLock::new(|| {
    let make = |pair: (&[u8], &[u8])| DecodedIcon {
        pixmaps: vec![decode_argb(pair.0), decode_argb(pair.1)],
    };
    [
        make(PNG_IDLE),
        make(PNG_VOICE_ON),
        make(PNG_VOICE_MUTED),
        make(PNG_VOICE_DEAFENED),
    ]
});

/// Índice en [`ICONS`] para cada estado, o `None` si el estado no tiene icono
/// (`DiscordClosed`: el ítem está `Passive` y el panel lo oculta).
fn icon_index(state: VoiceState) -> Option<usize> {
    match state {
        VoiceState::DiscordClosed => None,
        VoiceState::Idle => Some(0),
        VoiceState::VoiceUnmuted => Some(1),
        VoiceState::VoiceMuted => Some(2),
        VoiceState::VoiceDeafened => Some(3),
    }
}

/// El tray observable por ksni. Mantiene el estado actual y un token para
/// señalar la salida cuando el usuario pulsa "Salir".
pub struct VoiceTray {
    state: VoiceState,
    cancel: CancellationToken,
}

impl VoiceTray {
    /// Crea el tray con el estado inicial dado y el token de cancelación que se
    /// dispara desde el ítem "Salir" del menú.
    pub fn new(initial: VoiceState, cancel: CancellationToken) -> Self {
        Self {
            state: initial,
            cancel,
        }
    }

    /// Actualiza el estado mostrado. Usado desde `tray_task` vía `Handle::update`.
    pub fn set_state(&mut self, state: VoiceState) {
        self.state = state;
    }
}

impl ksni::Tray for VoiceTray {
    fn id(&self) -> String {
        "discord-voice-tray".into()
    }

    fn title(&self) -> String {
        "Discord Voice Tray".into()
    }

    /// Estado SNI del ítem: `Passive` cuando Discord está cerrado (los hosts
    /// suelen ocultar los ítems pasivos), `Active` en el resto de estados.
    ///
    /// En pasivo el ítem publica un `icon_pixmap` vacío (no hay icono que
    /// mostrar) pero mantiene el `tool_tip`. La transición `Active`↔`Passive`
    /// la propaga ksni (evento `NewStatus`) por el mismo camino
    /// `watch.changed()` → `Handle::update()` que ya repinta el icono.
    fn status(&self) -> ksni::Status {
        match self.state {
            VoiceState::DiscordClosed => ksni::Status::Passive,
            VoiceState::Idle
            | VoiceState::VoiceUnmuted
            | VoiceState::VoiceMuted
            | VoiceState::VoiceDeafened => ksni::Status::Active,
        }
    }

    /// Icono ARGB32 por estado (22px + 24px; el panel elige). Vacío en
    /// `DiscordClosed`: el ítem está `Passive` y no hay nada que mostrar.
    fn icon_pixmap(&self) -> Vec<Icon> {
        match icon_index(self.state) {
            Some(i) => ICONS[i].pixmaps.clone(),
            None => Vec::new(),
        }
    }

    /// Tooltip descriptivo usando el texto canónico de `state::label()`.
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Discord Voice Tray".into(),
            description: self.state.label().into(),
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    /// Menú clic derecho: estado actual (deshabilitado) + "Salir".
    ///
    /// Alcance solo lectura: ningún ítem escribe a Discord.
    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: self.state.label().into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Salir".into(),
                activate: Box::new(|tray: &mut VoiceTray| {
                    tracing::info!("\"Salir\" pulsado en el menú del tray; iniciando apagado");
                    tray.cancel.cancel();
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Arranca el servicio ksni y refresca el icono ante cada cambio de estado.
///
/// 1. Registra el `VoiceTray` en DBus (StatusNotifierItem) con el estado inicial
///    del canal `watch`. Con `assume_sni_available(true)` el arranque NO falla
///    si todavía no hay un host de tray (panel) corriendo: el icono aparecerá
///    cuando el host vuelva online.
/// 2. Loop: `rx.changed()` → `Handle::update()` para que el panel repinte el
///    icono y el tooltip en <1s.
/// 3. Termina cuando el canal `watch` se cierra (todas las puntas `tx` caídas) o
///    el token de cancelación se dispara: en ambos casos apaga el servicio ksni.
///
/// Decodificar los iconos puede fallar solo por un bug de build (PNG corrupto
/// embebido); por eso `decode_argb` hace `expect`. En operación normal no falla.
pub async fn tray_task(mut rx: watch::Receiver<VoiceState>, cancel: CancellationToken) {
    let initial = *rx.borrow();
    let tray = VoiceTray::new(initial, cancel.clone());

    // Forzar la decodificación de iconos ahora (perezosa) para fallar pronto si
    // hubiera un PNG corrupto, antes de registrar nada en DBus.
    LazyLock::force(&ICONS);

    let handle = match tray.assume_sni_available(true).spawn().await {
        Ok(h) => {
            tracing::info!("servicio StatusNotifierItem registrado en DBus");
            h
        }
        Err(e) => {
            tracing::error!(error = %e, "no se pudo registrar el tray SNI; disparando apagado");
            cancel.cancel();
            return;
        }
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("token cancelado; apagando servicio del tray");
                break;
            }
            changed = rx.changed() => {
                if changed.is_err() {
                    tracing::debug!("canal de estado cerrado; apagando servicio del tray");
                    break;
                }
                let state = *rx.borrow();
                // update() devuelve None si el servicio ya se apagó.
                if handle.update(|t| t.set_state(state)).await.is_none() {
                    tracing::debug!("servicio del tray ya apagado; terminando tray_task");
                    break;
                }
            }
        }
    }

    handle.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray;

    /// Construye un `VoiceTray` en el estado dado, sin runtime tokio ni DBus.
    /// `CancellationToken::new()` es síncrono; el token no se usa en `status()`.
    fn tray(state: VoiceState) -> VoiceTray {
        VoiceTray::new(state, CancellationToken::new())
    }

    #[test]
    fn discord_closed_es_passive() {
        assert_eq!(tray(VoiceState::DiscordClosed).status(), ksni::Status::Passive);
    }

    #[test]
    fn idle_es_active() {
        assert_eq!(tray(VoiceState::Idle).status(), ksni::Status::Active);
    }

    #[test]
    fn voice_unmuted_es_active() {
        assert_eq!(tray(VoiceState::VoiceUnmuted).status(), ksni::Status::Active);
    }

    #[test]
    fn voice_muted_es_active() {
        assert_eq!(tray(VoiceState::VoiceMuted).status(), ksni::Status::Active);
    }

    #[test]
    fn voice_deafened_es_active() {
        assert_eq!(tray(VoiceState::VoiceDeafened).status(), ksni::Status::Active);
    }

    #[test]
    fn discord_closed_sin_icono() {
        assert!(tray(VoiceState::DiscordClosed).icon_pixmap().is_empty());
    }

    #[test]
    fn estados_activos_con_icono() {
        for state in [
            VoiceState::Idle,
            VoiceState::VoiceUnmuted,
            VoiceState::VoiceMuted,
            VoiceState::VoiceDeafened,
        ] {
            assert_eq!(tray(state).icon_pixmap().len(), 2, "{state:?}");
        }
    }
}
