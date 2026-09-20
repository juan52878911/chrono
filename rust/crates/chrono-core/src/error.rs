//! Error y Result del núcleo, sin dependencias externas.

use std::fmt;

/// Error de dominio del núcleo. Los adaptadores envuelven sus errores de I/O aquí.
#[derive(Debug)]
pub enum CoreError {
    /// Un watermark dejó de casar con la fuente (sha inexistente, fichero rotado…).
    Diverged(String),
    /// La fuente no reconoce la entrada; el llamador debe forzar `--as` o dar un preset.
    Unrecognized(String),
    /// Cualquier otro fallo con mensaje.
    Other(String),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::Diverged(m) => write!(f, "fuente divergente: {m}"),
            CoreError::Unrecognized(m) => write!(f, "fuente no reconocida: {m}"),
            CoreError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CoreError {}

pub type Result<T> = std::result::Result<T, CoreError>;
