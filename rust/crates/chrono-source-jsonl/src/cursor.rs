//! Cursor de streaming sobre un fichero JSONL/NDJSON: RAM constante (una
//! línea a la vez), con seguimiento de offset en bytes para el watermark
//! incremental (`off:<bytes leídos>`) y reanudación en `open`.

use crate::config::{self, JsonlConfig, JsonlOverrides};
use crate::record;
use chrono_core::{CoreError, Cursor, Event, Result as CoreResult, SourceConfig, Watermark};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Lee la primera línea no vacía del fichero (para autodetectar la
/// configuración de campos, ver `config::resolve_config`), o `None` si el
/// fichero no se puede leer o no tiene ninguna línea no vacía.
pub(crate) fn peek_first_nonempty_line(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.ok()?;
        if !line.trim().is_empty() {
            return Some(line);
        }
    }
    None
}

/// `source_id` = "jsonl:<ruta absoluta>". Usa la ruta tal cual si no se puede
/// canonicalizar (fichero borrado entre el `detect` y el `open`, symlink
/// roto…), igual que hace `chrono-source-git`.
fn source_id_for(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    format!("jsonl:{}", abs.to_string_lossy())
}

pub struct JsonlCursor {
    reader: BufReader<File>,
    source_id: String,
    /// Overrides globales (de `SourceConfig.options`): se aplican a cada
    /// línea. Sin override, cada línea se resuelve por su cuenta en
    /// `record::line_to_event` (logs heterogéneos: una línea puede traer
    /// `ts` epoch y otra `@timestamp` ISO).
    overrides: JsonlOverrides,
    /// Resolución "de muestra" (primera línea no vacía del fichero), solo
    /// para reportar en `manifest()`: estable entre un `init` y un `sync`
    /// incremental que arranca a mitad de fichero.
    reported: JsonlConfig,
    /// Bytes consumidos desde el principio del fichero (posición actual);
    /// arranca en el offset del watermark de entrada, si lo hay.
    bytes_read: u64,
}

impl JsonlCursor {
    /// Abre el cursor sobre `path`, arrancando la lectura en `start_offset`
    /// bytes (0 si no hay watermark previo).
    pub fn open(path: &Path, start_offset: u64, cfg: &SourceConfig) -> CoreResult<Self> {
        let sample: Map<String, Value> = peek_first_nonempty_line(path)
            .and_then(|line| serde_json::from_str::<Value>(&line).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let reported = config::resolve_config(cfg, &sample);
        let overrides = config::overrides_from(cfg);

        let mut file = File::open(path).map_err(|e| CoreError::Other(format!("abriendo {}: {e}", path.display())))?;
        file.seek(SeekFrom::Start(start_offset))
            .map_err(|e| CoreError::Other(format!("buscando offset {start_offset} en {}: {e}", path.display())))?;

        Ok(Self {
            reader: BufReader::new(file),
            source_id: source_id_for(path),
            overrides,
            reported,
            bytes_read: start_offset,
        })
    }

    /// Lee la siguiente línea cruda (sin salto de línea final), o `None` al
    /// agotar el fichero. Avanza `bytes_read` con los bytes crudos leídos
    /// (incluido el salto de línea), para que el watermark refleje la
    /// posición real en el fichero.
    fn read_raw_line(&mut self) -> std::io::Result<Option<String>> {
        let mut buf: Vec<u8> = Vec::new();
        let n = self.reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        self.bytes_read += n as u64;
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }
}

impl Cursor for JsonlCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        loop {
            let line_offset = self.bytes_read;
            let raw = self
                .read_raw_line()
                .map_err(|e| CoreError::Other(format!("leyendo línea jsonl: {e}")))?;
            let Some(raw) = raw else {
                return Ok(None);
            };
            if raw.trim().is_empty() {
                continue; // línea en blanco: no es un evento, pero cuenta para el offset.
            }
            // Línea no parseable como objeto JSON: se descarta, igual que
            // `chrono-source-git` descarta registros corruptos de `git log`.
            let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            return Ok(Some(record::line_to_event(&obj, &raw, line_offset, &self.overrides, &self.source_id)));
        }
    }

    fn watermark(&self) -> Watermark {
        Watermark { kind: "jsonl".to_string(), value: format!("off:{}", self.bytes_read) }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("format".to_string(), "jsonl".to_string());
        m.insert("time_field".to_string(), self.reported.time_field.clone());
        m.insert("message_field".to_string(), self.reported.message_field.clone());
        m.insert("entity_field".to_string(), self.reported.entity_field.clone());
        m
    }
}
