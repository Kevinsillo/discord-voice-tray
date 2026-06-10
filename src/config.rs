//! Carga de config TOML opcional y cache del token de acceso.
//!
//! Sin red. La config vive en `~/.config/discord-voice-tray/config.toml` (todo
//! opcional) y el token cacheado en `~/.config/discord-voice-tray/token.json`
//! con permisos EXACTAMENTE `0600`.
//!
//! Si no hay config (o le faltan `client_id`/`client_secret`) se usa la opción A
//! de auth (StreamKit, ver PROJECT.md). La opción B se detecta vía [`AuthMode`]
//! pero su ejecución queda fuera del MVP.

use std::fs;
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ipc::protocol::DEFAULT_CLIENT_ID;

/// Errores de carga/guardado de config y token.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// No se pudo resolver el directorio de config (`$HOME`/`$XDG_CONFIG_HOME`).
    #[error("no se pudo determinar el directorio de configuración (¿$HOME definido?)")]
    NoConfigDir,

    /// Error de I/O leyendo/escribiendo ficheros de config o token.
    #[error("error de I/O en config: {0}")]
    Io(#[from] io::Error),

    /// El `config.toml` existe pero no es TOML válido.
    #[error("config.toml inválido: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// El `token.json` existe pero no es JSON válido.
    #[error("token.json inválido: {0}")]
    JsonParse(#[from] serde_json::Error),
}

/// Modo de autenticación seleccionado a partir de la config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Opción A: StreamKit, sin secret. Por defecto y único soportado en el MVP.
    StreamKit,
    /// Opción B: app propia con `client_id` + `client_secret`. Detectado pero
    /// NO implementado en el MVP (ver TODO en `client.rs`).
    OwnApp,
}

/// Config opcional cargada de `config.toml`. Todos los campos son opcionales.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// `client_id` de una app propia (opción B). Ausente → opción A.
    pub client_id: Option<String>,
    /// `client_secret` de una app propia (opción B). Ausente → opción A.
    pub client_secret: Option<String>,
}

impl Config {
    /// Carga `config.toml` del directorio de config. Si el fichero no existe
    /// devuelve una `Config` vacía (opción A); otros errores se propagan.
    pub fn load() -> Result<Config, ConfigError> {
        let path = config_dir()?.join("config.toml");
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let cfg: Config = toml::from_str(&contents)?;
                tracing::debug!(path = %path.display(), "config.toml cargado");
                Ok(cfg)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "config.toml ausente; usando opción A (StreamKit)");
                Ok(Config::default())
            }
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    /// Determina el modo de auth: `OwnApp` solo si están AMBOS campos.
    pub fn auth_mode(&self) -> AuthMode {
        match (&self.client_id, &self.client_secret) {
            (Some(id), Some(secret)) if !id.is_empty() && !secret.is_empty() => AuthMode::OwnApp,
            _ => AuthMode::StreamKit,
        }
    }

    /// `client_id` efectivo para el handshake/AUTHORIZE: el de la app propia si
    /// está configurado, o el de StreamKit por defecto.
    pub fn effective_client_id(&self) -> String {
        match self.auth_mode() {
            AuthMode::OwnApp => self.client_id.clone().unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string()),
            AuthMode::StreamKit => DEFAULT_CLIENT_ID.to_string(),
        }
    }
}

/// Token de acceso cacheado en disco.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCache {
    pub access_token: String,
}

/// Lee el token cacheado. Devuelve `Ok(None)` si `token.json` no existe.
pub fn load_token() -> Result<Option<TokenCache>, ConfigError> {
    let path = token_path()?;
    match fs::read_to_string(&path) {
        Ok(contents) => {
            let cache: TokenCache = serde_json::from_str(&contents)?;
            tracing::debug!(path = %path.display(), "token cacheado cargado");
            Ok(Some(cache))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::Io(e)),
    }
}

/// Guarda el token en `token.json` con permisos EXACTAMENTE `0600`.
///
/// Crea el directorio de config si falta. Reescribe siempre con `0600`, incluso
/// si el fichero ya existía con otros permisos.
pub fn save_token(token: &TokenCache) -> Result<(), ConfigError> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("token.json");
    let json = serde_json::to_string_pretty(token)?;

    // Crear/truncar con 0600 de raíz para que nunca exista una ventana con
    // permisos más laxos.
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    {
        use std::io::Write as _;
        let mut file = opts.open(&path)?;
        file.write_all(json.as_bytes())?;
        file.flush()?;
    }
    // Forzar 0600 también si el fichero preexistía (open no cambia el modo de un
    // fichero ya existente).
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    tracing::debug!(path = %path.display(), "token cacheado guardado (0600)");
    Ok(())
}

/// Borra el token cacheado (p.ej. tras un AUTHENTICATE rechazado). No falla si
/// el fichero ya no existe.
pub fn clear_token() -> Result<(), ConfigError> {
    let path = token_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ConfigError::Io(e)),
    }
}

/// Ruta del `token.json`.
fn token_path() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join("token.json"))
}

/// Directorio de config: `$XDG_CONFIG_HOME/discord-voice-tray` o
/// `$HOME/.config/discord-voice-tray`.
fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("discord-voice-tray"));
        }
    }
    let home = std::env::var_os("HOME").ok_or(ConfigError::NoConfigDir)?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("discord-voice-tray"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_vacia_usa_streamkit() {
        let cfg = Config::default();
        assert_eq!(cfg.auth_mode(), AuthMode::StreamKit);
        assert_eq!(cfg.effective_client_id(), DEFAULT_CLIENT_ID);
    }

    #[test]
    fn config_solo_client_id_usa_streamkit() {
        let cfg = Config {
            client_id: Some("123".into()),
            client_secret: None,
        };
        assert_eq!(cfg.auth_mode(), AuthMode::StreamKit);
    }

    #[test]
    fn config_ambos_campos_usa_own_app() {
        let cfg = Config {
            client_id: Some("123".into()),
            client_secret: Some("sec".into()),
        };
        assert_eq!(cfg.auth_mode(), AuthMode::OwnApp);
        assert_eq!(cfg.effective_client_id(), "123");
    }

    #[test]
    fn config_campos_vacios_usa_streamkit() {
        let cfg = Config {
            client_id: Some(String::new()),
            client_secret: Some(String::new()),
        };
        assert_eq!(cfg.auth_mode(), AuthMode::StreamKit);
    }

    #[test]
    fn save_token_escribe_con_permisos_0600() {
        // Aísla el directorio de config vía XDG_CONFIG_HOME en un tmpdir único.
        let tmp = std::env::temp_dir().join(format!("dvt-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let token = TokenCache {
            access_token: "secreto".into(),
        };
        save_token(&token).expect("save_token debe tener éxito");

        let path = tmp.join("discord-voice-tray").join("token.json");
        let meta = fs::metadata(&path).expect("token.json debe existir");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "permisos deben ser exactamente 0600");

        // Round-trip: load_token devuelve el mismo token.
        let loaded = load_token().expect("load_token ok").expect("token presente");
        assert_eq!(loaded.access_token, "secreto");

        // clear_token lo borra y deja load_token en None.
        clear_token().expect("clear_token ok");
        assert!(load_token().expect("load_token ok").is_none());

        let _ = fs::remove_dir_all(&tmp);
    }
}
