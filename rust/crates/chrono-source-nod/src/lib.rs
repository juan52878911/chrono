//! `chrono-source-nod` — adaptador Source para el flujo de eventos **Nod v1**
//! (https://github.com/juan52878911/nod, `docs/nod-v1.md`): el JSONL de sobres
//! que escribe el destino de tipo fichero de Nod
//! (`~/.local/state/nod/events.jsonl`, una línea = `{"nod":1,"source":{…},"events":[…]}`).
//!
//! Nod mide el PROCESO (cómo trabajó el agente); chrono, el RESULTADO (qué se
//! entregó en git y si aguantó). Los une `vcs.commit` → link `commit` al sha.
//! El cruce lo hace la consulta `session_patterns` (`chrono-metrics`).
//!
//! - `detect`: 90 si la primera línea no vacía es un sobre `nod: 1` con
//!   `source` y `events` (por encima del 60 de `jsonl`, que también lo leería
//!   como JSON genérico); 0 si no.
//! - Dedup por (`instance`, `run`, `seq`): es el `id` del evento, y el store
//!   ignora un id repetido.
//! - Tipos desconocidos, sobres inválidos o de otra versión: se saltan (se
//!   cuentan en el manifiesto).
//! - `sync` incremental por offset de bytes + hash del prefijo (como jsonl).

mod cursor;
mod record;
mod simhash;

pub use cursor::NodCursor;
pub use record::KNOWN_TYPES;

use chrono_core::{CoreError, Result as CoreResult, Source, SourceConfig, Watermark};
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Adaptador `Source` del flujo Nod v1.
#[derive(Default)]
pub struct NodSource;

impl NodSource {
    pub fn new() -> Self {
        Self
    }
}

/// ¿Es `line` un sobre Nod v1?
fn is_envelope(line: &str) -> bool {
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    obj.get("nod").and_then(Value::as_i64) == Some(1)
        && obj.get("source").is_some_and(Value::is_object)
        && obj.get("events").is_some_and(Value::is_array)
}

impl Source for NodSource {
    fn kind(&self) -> &str {
        "nod"
    }

    fn detect(&self, path: &Path) -> i32 {
        if !fs::metadata(path).map(|m| m.is_file()).unwrap_or(false) {
            return 0;
        }
        match cursor::peek_first_nonempty_line(path) {
            Some(line) if is_envelope(&line) => 90,
            _ => 0,
        }
    }

    fn open(&self, path: &Path, watermark: Option<Watermark>, _cfg: &SourceConfig) -> CoreResult<Box<dyn chrono_core::Cursor>> {
        let meta = fs::metadata(path).map_err(|e| CoreError::Other(format!("leyendo metadata de {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(CoreError::Other(format!("{} no es un fichero regular", path.display())));
        }
        let start = match &watermark {
            None => 0,
            Some(wm) => {
                let (off, prefix) = cursor::parse_watermark(&wm.value).ok_or_else(|| CoreError::Diverged(wm.value.clone()))?;
                if meta.len() < off {
                    return Err(CoreError::Diverged(wm.value.clone()));
                }
                if let Some(expected) = prefix {
                    if cursor::hash_prefix(path, off)? != expected {
                        return Err(CoreError::Diverged(wm.value.clone()));
                    }
                }
                off
            }
        };
        Ok(Box::new(NodCursor::open(path, start)?))
    }
}

#[cfg(test)]
mod tests;
