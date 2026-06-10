//! Enum `VoiceState` y reducción pura desde señales crudas de Discord.
//!
//! Este módulo no realiza ningún I/O: toma una foto de las señales relevantes
//! ([`RawSignals`]) y la reduce a un único [`VoiceState`] aplicando la prioridad
//! `DiscordClosed > Idle > Deafened > Muted > Unmuted`.
//!
//! La pérdida de socket (estado `DiscordClosed`) la decide el orquestador de
//! reconexión (lote posterior), no esta función.

/// Estado del icono del tray, en orden de prioridad creciente de severidad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    /// Discord no está corriendo / socket perdido. Lo establece el orquestador.
    DiscordClosed,
    /// Conectado a Discord pero sin canal de voz activo.
    Idle,
    /// En un canal de voz con el micro abierto.
    VoiceUnmuted,
    /// En un canal de voz con el micro muteado.
    VoiceMuted,
    /// En un canal de voz, ensordecido (deafen implica mute; prioridad sobre mute).
    VoiceDeafened,
}

impl VoiceState {
    /// Texto legible para tooltip y menú del tray.
    pub fn label(&self) -> &'static str {
        match self {
            VoiceState::DiscordClosed => "Discord cerrado",
            VoiceState::Idle => "Conectado a Discord (sin canal de voz)",
            VoiceState::VoiceUnmuted => "En canal de voz",
            VoiceState::VoiceMuted => "En canal de voz — muteado",
            VoiceState::VoiceDeafened => "En canal de voz — ensordecido",
        }
    }
}

/// Foto de las señales crudas que Discord reporta sobre el estado de voz.
///
/// Se va actualizando incrementalmente conforme llegan eventos/respuestas GET
/// y se reduce con [`reduce`] tras cada cambio.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawSignals {
    /// Hay un canal de voz seleccionado (`channel_id != null`).
    pub in_channel: bool,
    /// El usuario tiene el micro muteado.
    pub mute: bool,
    /// El usuario está ensordecido (implica mute en Discord).
    pub deaf: bool,
    /// La conexión de voz no está en estado `DISCONNECTED`.
    ///
    /// `VOICE_CONNECTION_STATUS state="DISCONNECTED"` lo pone a `false` y fuerza
    /// `Idle` aunque siga habiendo un canal seleccionado momentáneamente.
    pub connected: bool,
}

/// Reduce las señales crudas a un único [`VoiceState`].
///
/// No produce nunca `DiscordClosed`: ese estado depende de la vida del socket,
/// que esta función pura desconoce. Aplica la prioridad
/// `Deafened > Muted > Unmuted`, y `Idle` cuando no hay canal activo.
pub fn reduce(signals: &RawSignals) -> VoiceState {
    // Sin canal activo (o desconectado de la voz) → Idle, ignorando settings.
    if !signals.in_channel || !signals.connected {
        return VoiceState::Idle;
    }
    if signals.deaf {
        return VoiceState::VoiceDeafened;
    }
    if signals.mute {
        return VoiceState::VoiceMuted;
    }
    VoiceState::VoiceUnmuted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Señales de "en canal, conectado" con los flags de settings dados.
    fn in_channel(mute: bool, deaf: bool) -> RawSignals {
        RawSignals {
            in_channel: true,
            connected: true,
            mute,
            deaf,
        }
    }

    #[test]
    fn deafen_tiene_prioridad_sobre_mute_desde_voice_muted() {
        // Estaba muteado (VoiceMuted); llega deaf=true → VoiceDeafened.
        let antes = in_channel(true, false);
        assert_eq!(reduce(&antes), VoiceState::VoiceMuted);
        let despues = in_channel(true, true);
        assert_eq!(reduce(&despues), VoiceState::VoiceDeafened);
    }

    #[test]
    fn salir_de_canal_desde_unmuted_va_a_idle() {
        let mut s = in_channel(false, false);
        assert_eq!(reduce(&s), VoiceState::VoiceUnmuted);
        s.in_channel = false; // channel_id = null
        assert_eq!(reduce(&s), VoiceState::Idle);
    }

    #[test]
    fn salir_de_canal_desde_muted_va_a_idle() {
        let mut s = in_channel(true, false);
        assert_eq!(reduce(&s), VoiceState::VoiceMuted);
        s.in_channel = false;
        assert_eq!(reduce(&s), VoiceState::Idle);
    }

    #[test]
    fn salir_de_canal_desde_deafened_va_a_idle() {
        let mut s = in_channel(true, true);
        assert_eq!(reduce(&s), VoiceState::VoiceDeafened);
        s.in_channel = false;
        assert_eq!(reduce(&s), VoiceState::Idle);
    }

    #[test]
    fn settings_mute_en_idle_no_cambia_estado() {
        // Sin canal activo, mute=true no debe sacar de Idle.
        let s = RawSignals {
            in_channel: false,
            connected: true,
            mute: true,
            deaf: false,
        };
        assert_eq!(reduce(&s), VoiceState::Idle);
    }

    #[test]
    fn entrada_a_canal_desde_idle_va_a_unmuted() {
        let idle = RawSignals {
            in_channel: false,
            connected: true,
            mute: false,
            deaf: false,
        };
        assert_eq!(reduce(&idle), VoiceState::Idle);
        let entrando = in_channel(false, false);
        assert_eq!(reduce(&entrando), VoiceState::VoiceUnmuted);
    }

    #[test]
    fn connection_status_disconnected_fuerza_idle() {
        // Aunque hubiera canal seleccionado, connected=false → Idle.
        let s = RawSignals {
            in_channel: true,
            connected: false,
            mute: false,
            deaf: false,
        };
        assert_eq!(reduce(&s), VoiceState::Idle);
    }

    #[test]
    fn deaf_implica_deafened_aunque_mute_false() {
        let s = in_channel(false, true);
        assert_eq!(reduce(&s), VoiceState::VoiceDeafened);
    }
}
