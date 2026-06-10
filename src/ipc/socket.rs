//! Descubrimiento del socket IPC de Discord y framing de bajo nivel.
//!
//! Este módulo solo entiende de *bytes* y *frames*: localiza el socket,
//! lee y escribe frames `[opcode u32 LE][length u32 LE][payload JSON]`.
//! No conoce semántica RPC (handshake, comandos) — eso vive en `client.rs`.

use std::path::PathBuf;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::protocol::Opcode;

/// Límite defensivo de tamaño de payload (bytes). Evita asignaciones enormes
/// ante un `length` corrupto. Los frames RPC de Discord son pequeños.
const MAX_PAYLOAD_LEN: u32 = 16 * 1024 * 1024;

/// Errores del socket y del framing.
#[derive(Debug, Error)]
pub enum SocketError {
    /// No se encontró ningún socket `discord-ipc-N` en ninguna familia de rutas.
    /// El caller lo interpreta como "Discord cerrado".
    #[error("no se encontró ningún socket discord-ipc (¿Discord cerrado?)")]
    NotFound,

    /// Error de I/O al conectar, leer o escribir.
    #[error("error de I/O en el socket: {0}")]
    Io(#[from] std::io::Error),

    /// El `length` del frame excede los bytes disponibles o el límite defensivo.
    #[error("frame inválido: length declarado {declared} excede el límite o los bytes disponibles")]
    InvalidLength { declared: u32 },

    /// Opcode no reconocido (fuera de 0..=4).
    #[error("opcode desconocido: {0}")]
    UnknownOpcode(u32),
}

/// Un frame decodificado del socket: opcode + payload crudo (bytes JSON).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub opcode: Opcode,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(opcode: Opcode, payload: Vec<u8>) -> Self {
        Self { opcode, payload }
    }

    /// Codifica el frame a su representación de cable:
    /// `[opcode u32 LE][length u32 LE][payload]`.
    pub fn encode(&self) -> Vec<u8> {
        let len = self.payload.len() as u32;
        let mut buf = Vec::with_capacity(8 + self.payload.len());
        buf.extend_from_slice(&self.opcode.as_u32().to_le_bytes());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Decodifica un frame desde un buffer completo en memoria.
    ///
    /// Devuelve `Err` (nunca panic) si los bytes no alcanzan para la cabecera,
    /// si el `length` excede los bytes disponibles, o si el opcode es desconocido.
    ///
    /// La sesión sobre el stream usa [`read_frame`] (lectura incremental); este
    /// método existe para decodificar buffers completos y es la base de los
    /// tests de round-trip del framing.
    #[allow(dead_code)]
    pub fn decode(bytes: &[u8]) -> Result<Frame, SocketError> {
        if bytes.len() < 8 {
            return Err(SocketError::InvalidLength {
                declared: bytes.len() as u32,
            });
        }
        let opcode_raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let length = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);

        if length > MAX_PAYLOAD_LEN {
            return Err(SocketError::InvalidLength { declared: length });
        }
        let available = bytes.len() - 8;
        if (length as usize) > available {
            return Err(SocketError::InvalidLength { declared: length });
        }

        let opcode = Opcode::from_u32(opcode_raw).ok_or(SocketError::UnknownOpcode(opcode_raw))?;
        let payload = bytes[8..8 + length as usize].to_vec();
        Ok(Frame::new(opcode, payload))
    }
}

/// Candidatos de ruta del socket, en orden de prioridad (PROJECT.md §1).
///
/// Para cada familia se prueban los sufijos `discord-ipc-0`..`discord-ipc-9`.
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);

    // Familia 1: $XDG_RUNTIME_DIR/discord-ipc-N
    if let Some(base) = &xdg {
        for n in 0..10 {
            paths.push(base.join(format!("discord-ipc-{n}")));
        }
    }
    // Familia 2: Flatpak.
    if let Some(base) = &xdg {
        let flatpak = base.join("app").join("com.discordapp.Discord");
        for n in 0..10 {
            paths.push(flatpak.join(format!("discord-ipc-{n}")));
        }
    }
    // Familia 3: Snap.
    if let Some(base) = &xdg {
        let snap = base.join("snap.discord");
        for n in 0..10 {
            paths.push(snap.join(format!("discord-ipc-{n}")));
        }
    }
    // Familia 4: fallback /tmp.
    for n in 0..10 {
        paths.push(PathBuf::from(format!("/tmp/discord-ipc-{n}")));
    }
    paths
}

/// Descubre y conecta al primer socket `discord-ipc-N` que exista,
/// respetando el orden de prioridad de familias. Devuelve el stream conectado.
///
/// Si ninguna ruta acepta conexión, devuelve [`SocketError::NotFound`] para que
/// el caller programe el reintento (estado "Discord cerrado").
pub async fn discover_socket() -> Result<UnixStream, SocketError> {
    for path in candidate_paths() {
        // Conectar directamente: probar conexión es más fiable que `exists()`
        // (un socket huérfano puede existir pero rechazar la conexión).
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                tracing::debug!(path = %path.display(), "socket discord-ipc conectado");
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => continue,
            // Otros errores (permisos, etc.): seguir probando el resto.
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "ruta descartada");
                continue;
            }
        }
    }
    Err(SocketError::NotFound)
}

/// Escribe un frame completo sobre el stream.
pub async fn write_frame(stream: &mut UnixStream, frame: &Frame) -> Result<(), SocketError> {
    let encoded = frame.encode();
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    Ok(())
}

/// Lee un frame completo del stream: primero la cabecera de 8 bytes, luego
/// `length` bytes de payload.
///
/// Si la cabecera anuncia un `length` que excede el límite defensivo devuelve
/// `Err` sin leer el payload. Un EOF durante la cabecera o el payload se
/// propaga como [`SocketError::Io`] (`UnexpectedEof`), nunca como mensaje parcial.
pub async fn read_frame(stream: &mut UnixStream) -> Result<Frame, SocketError> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await?;

    let opcode_raw = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);

    if length > MAX_PAYLOAD_LEN {
        return Err(SocketError::InvalidLength { declared: length });
    }
    let opcode = Opcode::from_u32(opcode_raw).ok_or(SocketError::UnknownOpcode(opcode_raw))?;

    let mut payload = vec![0u8; length as usize];
    stream.read_exact(&mut payload).await?;

    Ok(Frame::new(opcode, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::protocol::{Handshake, DEFAULT_CLIENT_ID};

    #[test]
    fn roundtrip_frame_arbitrary_payload() {
        let payload = br#"{"hello":"world","n":42}"#.to_vec();
        let frame = Frame::new(Opcode::Frame, payload.clone());
        let encoded = frame.encode();
        let decoded = Frame::decode(&encoded).expect("decode debe tener éxito");
        assert_eq!(decoded.opcode, Opcode::Frame);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn roundtrip_handshake_opcode_zero() {
        let hs = Handshake::new(DEFAULT_CLIENT_ID);
        let payload = serde_json::to_vec(&hs).unwrap();
        let frame = Frame::new(Opcode::Handshake, payload);
        let encoded = frame.encode();
        let decoded = Frame::decode(&encoded).expect("decode debe tener éxito");
        assert_eq!(decoded.opcode, Opcode::Handshake);
        let parsed: Handshake = serde_json::from_slice(&decoded.payload).unwrap();
        assert_eq!(parsed.v, 1);
        assert_eq!(parsed.client_id, DEFAULT_CLIENT_ID);
    }

    #[test]
    fn decode_invalid_length_is_err_not_panic() {
        // Cabecera que declara 1000 bytes de payload pero solo hay 2.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&Opcode::Frame.as_u32().to_le_bytes());
        bytes.extend_from_slice(&1000u32.to_le_bytes());
        bytes.extend_from_slice(&[0xAB, 0xCD]);
        let err = Frame::decode(&bytes).unwrap_err();
        assert!(matches!(err, SocketError::InvalidLength { declared: 1000 }));
    }

    #[test]
    fn decode_too_short_for_header_is_err() {
        let bytes = [0u8; 4];
        assert!(matches!(
            Frame::decode(&bytes),
            Err(SocketError::InvalidLength { .. })
        ));
    }

    #[test]
    fn decode_excessive_length_is_err() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&Opcode::Frame.as_u32().to_le_bytes());
        bytes.extend_from_slice(&(MAX_PAYLOAD_LEN + 1).to_le_bytes());
        let err = Frame::decode(&bytes).unwrap_err();
        assert!(matches!(err, SocketError::InvalidLength { .. }));
    }
}
