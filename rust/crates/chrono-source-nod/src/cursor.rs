//! Cursor de streaming sobre el fichero de sobres de Nod: una línea = un sobre
//! con N eventos. Lee línea a línea (RAM acotada por el sobre más grande, ≤500
//! eventos según el contrato) y entrega los eventos de uno en uno.
//!
//! Watermark `off:<n>|p:<hex>` como `chrono-source-jsonl`, con una diferencia:
//! solo se consumen líneas COMPLETAS (terminadas en `\n`). Nod añade al final
//! del fichero mientras la sesión sigue viva; una última línea a medio escribir
//! no se lee ni avanza el offset, y se ingiere entera en el siguiente `sync`.

use crate::record::{self, EnvelopeSource};
use chrono_core::{CoreError, Cursor, Event, Result as CoreResult, Watermark};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Tope de bytes que se hashean como prefijo del fichero (ver DECISIONS §16).
const PREFIX_CAP: u64 = 4096;

pub(crate) fn source_id_for(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    format!("nod:{}", abs.to_string_lossy())
}

/// FNV-1a 64 en hex de los primeros `min(PREFIX_CAP, limit)` bytes.
pub(crate) fn hash_prefix(path: &Path, limit: u64) -> CoreResult<String> {
    let want = limit.min(PREFIX_CAP) as usize;
    let mut file = File::open(path).map_err(|e| CoreError::Other(format!("abriendo {}: {e}", path.display())))?;
    let mut buf = vec![0u8; want];
    let mut total = 0usize;
    while total < want {
        match file.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) => return Err(CoreError::Other(format!("leyendo prefijo de {}: {e}", path.display()))),
        }
    }
    Ok(format!("{:016x}", crate::simhash::fnv1a64(&buf[..total])))
}

/// `off:<n>[|p:<hex>]` → `(n, hash)`.
pub(crate) fn parse_watermark(value: &str) -> Option<(u64, Option<String>)> {
    let (off_part, prefix_part) = match value.split_once('|') {
        Some((off, rest)) => (off, Some(rest)),
        None => (value, None),
    };
    let off: u64 = off_part.strip_prefix("off:")?.parse().ok()?;
    let prefix_hex = match prefix_part {
        Some(p) => Some(p.strip_prefix("p:")?.to_string()),
        None => None,
    };
    Some((off, prefix_hex))
}

/// Primera línea no vacía (para `detect`).
pub(crate) fn peek_first_nonempty_line(path: &Path) -> Option<String> {
    let reader = BufReader::new(File::open(path).ok()?);
    for line in reader.lines() {
        let line = line.ok()?;
        if !line.trim().is_empty() {
            return Some(line);
        }
    }
    None
}

#[derive(Default)]
struct Stats {
    envelopes: u64,
    invalid_lines: u64,
    unknown_events: u64,
}

pub struct NodCursor {
    reader: BufReader<File>,
    path: PathBuf,
    source_id: String,
    /// Offset del final de la última línea COMPLETA ya leída.
    line_end: u64,
    /// Offset del principio de la línea cuyos eventos quedan en `pending`.
    line_start: u64,
    pending: VecDeque<Event>,
    stats: Stats,
}

impl NodCursor {
    pub fn open(path: &Path, start_offset: u64) -> CoreResult<Self> {
        let mut file = File::open(path).map_err(|e| CoreError::Other(format!("abriendo {}: {e}", path.display())))?;
        file.seek(SeekFrom::Start(start_offset))
            .map_err(|e| CoreError::Other(format!("buscando offset {start_offset} en {}: {e}", path.display())))?;
        Ok(Self {
            reader: BufReader::new(file),
            path: path.to_path_buf(),
            source_id: source_id_for(path),
            line_end: start_offset,
            line_start: start_offset,
            pending: VecDeque::new(),
            stats: Stats::default(),
        })
    }

    /// Siguiente línea completa, o `None` al llegar al final (una línea final
    /// sin `\n` se deja sin consumir).
    fn read_line(&mut self) -> std::io::Result<Option<String>> {
        let mut buf = Vec::new();
        let n = self.reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            return Ok(None);
        }
        self.line_start = self.line_end;
        self.line_end += n as u64;
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }

    /// Parsea un sobre y encola sus eventos conocidos.
    fn enqueue(&mut self, line: &str) {
        let Ok(Value::Object(env)) = serde_json::from_str::<Value>(line) else {
            self.stats.invalid_lines += 1;
            return;
        };
        let Some(src) = EnvelopeSource::from_envelope(&env) else {
            self.stats.invalid_lines += 1;
            return;
        };
        let Some(events) = env.get("events").and_then(Value::as_array) else {
            self.stats.invalid_lines += 1;
            return;
        };
        self.stats.envelopes += 1;
        for ev in events {
            match ev.as_object().and_then(|o| record::to_event(&src, o, &self.source_id)) {
                Some(e) => self.pending.push_back(e),
                None => self.stats.unknown_events += 1,
            }
        }
    }
}

impl Cursor for NodCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(Some(ev));
            }
            let line = self.read_line().map_err(|e| CoreError::Other(format!("leyendo sobre nod: {e}")))?;
            let Some(line) = line else {
                return Ok(None);
            };
            if line.trim().is_empty() {
                continue;
            }
            self.enqueue(&line);
        }
    }

    fn watermark(&self) -> Watermark {
        // Si la ingesta se cortó a mitad de un sobre (cap de eventos), el
        // watermark vuelve al principio de esa línea: el siguiente `sync` la
        // relee y los eventos ya guardados se descartan por id.
        let off = if self.pending.is_empty() { self.line_end } else { self.line_start };
        let value = match hash_prefix(&self.path, off) {
            Ok(h) => format!("off:{off}|p:{h}"),
            Err(_) => format!("off:{off}"),
        };
        Watermark { kind: "nod".to_string(), value }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("format".to_string(), "nod-v1".to_string());
        m.insert("nod_envelopes".to_string(), self.stats.envelopes.to_string());
        m.insert("nod_invalid_lines".to_string(), self.stats.invalid_lines.to_string());
        m.insert("nod_skipped_events".to_string(), self.stats.unknown_events.to_string());
        m
    }
}
