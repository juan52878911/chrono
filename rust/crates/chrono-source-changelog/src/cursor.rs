//! Cursor sobre un fichero CHANGELOG. A diferencia de `chrono-source-jsonl`
//! (streaming línea a línea con watermark de offset), un changelog es
//! pequeño y se edita por ARRIBA (las releases nuevas se anteponen): un
//! offset de bytes no sirve de watermark porque el contenido "antiguo" se
//! desplaza. Este cursor:
//!
//! - Lee el fichero ENTERO de una vez y lo parsea completo en `open` (no hay
//!   streaming real; no hace falta, el formato es pequeño por naturaleza).
//! - El watermark es el hash FNV-1a del contenido completo del fichero
//!   (`sha:<hex>`), no un offset. Si al reabrir el hash coincide, el fichero
//!   no cambió: el cursor no entrega nada. Si no coincide, el fichero
//!   cambió de cualquier forma (prepend, edición,...) y no hay manera barata
//!   de saber qué es "nuevo": se devuelve `CoreError::Diverged` para que el
//!   llamador reingiera la fuente entera (barato y correcto para un fichero
//!   de este tamaño).

use crate::parser;
use crate::record::releases_to_events;
use crate::simhash::fnv1a64;
use chrono_core::{CoreError, Cursor, Event, Result as CoreResult, SourceConfig, Watermark};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// `source_id` = "changelog:<ruta absoluta>". Ruta tal cual si no se puede
/// canonicalizar (fichero borrado entre el `detect` y el `open`, symlink
/// roto…), igual que `chrono-source-jsonl`.
fn source_id_for(path: &Path) -> String {
    let abs = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    format!("changelog:{}", abs.to_string_lossy())
}

/// Watermark de contenido: `sha:<fnv1a64 en hex del fichero completo>`.
fn content_watermark(content: &str) -> String {
    format!("sha:{:016x}", fnv1a64(content.as_bytes()))
}

pub struct ChangelogCursor {
    events: std::vec::IntoIter<Event>,
    watermark_value: String,
    releases_count: usize,
}

impl ChangelogCursor {
    /// Abre el cursor sobre `path`. `_cfg` se acepta por simetría con el
    /// resto de adaptadores (contrato `Source::open`), pero el formato Keep
    /// a Changelog no tiene columnas ni campos configurables: no se usa.
    pub fn open(path: &Path, watermark: Option<Watermark>, _cfg: &SourceConfig) -> CoreResult<Self> {
        let content = fs::read_to_string(path).map_err(|e| CoreError::Other(format!("leyendo {}: {e}", path.display())))?;
        let current_wm = content_watermark(&content);

        if let Some(wm) = watermark {
            if wm.value == current_wm {
                // Mismo contenido: nada nuevo que entregar, pero el
                // manifiesto sigue informando cuántas releases tiene el
                // fichero (información sobre la fuente, no sobre lo emitido).
                let releases_count = parser::parse(&content).len();
                return Ok(Self { events: Vec::new().into_iter(), watermark_value: current_wm, releases_count });
            }
            // Contenido distinto (prepend de una release nueva, edición de
            // una existente, reordenación...): no hay offset fiable desde
            // el que continuar. El llamador (CLI `sync`) borra los eventos
            // de esta fuente y reingiere entero.
            return Err(CoreError::Diverged(wm.value));
        }

        let releases = parser::parse(&content);
        let releases_count = releases.len();
        let source_id = source_id_for(path);
        let events = releases_to_events(&releases, &source_id);
        Ok(Self { events: events.into_iter(), watermark_value: current_wm, releases_count })
    }
}

impl Cursor for ChangelogCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        Ok(self.events.next())
    }

    fn watermark(&self) -> Watermark {
        Watermark { kind: "changelog".to_string(), value: self.watermark_value.clone() }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("format".to_string(), "changelog".to_string());
        m.insert("releases".to_string(), self.releases_count.to_string());
        m
    }
}
