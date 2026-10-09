//! Cursor de streaming sobre un fichero de texto casado con una plantilla:
//! RAM constante (una línea a la vez), con offset en bytes para el watermark
//! incremental (`off:<bytes>|p:<hash del prefijo>`) y reanudación en `open`.
//! Mismo modelo que `chrono-source-jsonl` / `-csv` / `-textlog`.

use crate::matcher::Plantilla;
use crate::record::{self, Roles};
use chrono_core::{CoreError, Cursor, Event, Result as CoreResult, Watermark};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// `source_id` = "template:<ruta absoluta>" (ruta tal cual si no canonicaliza).
fn source_id_for(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    format!("template:{}", abs.to_string_lossy())
}

/// Tope de bytes que se hashean como "prefijo" del fichero.
const PREFIX_CAP: u64 = 4096;

/// FNV-1a 64 (16 dígitos hex) de los primeros `min(PREFIX_CAP, limit)` bytes
/// de `path`. `limit` es el offset del watermark (región YA consumida): un
/// append no cambia ese prefijo; una reescritura de lo consumido, sí.
pub(crate) fn hash_prefix(path: &Path, limit: u64) -> CoreResult<String> {
    let want = limit.min(PREFIX_CAP) as usize;
    let mut file =
        File::open(path).map_err(|e| CoreError::Other(format!("abriendo {} para hash de prefijo: {e}", path.display())))?;
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

/// Parsea un watermark: `off:<n>` o `off:<n>|p:<hex>`. `None` si es inválido.
/// Retrocompatible: sin `|p:` devuelve `(n, None)`.
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

pub struct TemplateCursor {
    reader: BufReader<File>,
    source_id: String,
    plantilla: Plantilla,
    roles: Roles,
    /// Bytes consumidos desde el principio del fichero.
    bytes_read: u64,
    path: PathBuf,
}

impl TemplateCursor {
    /// Abre el cursor sobre `path` leyendo desde `start_offset` bytes.
    pub fn open(path: &Path, start_offset: u64, plantilla: Plantilla, roles: Roles) -> CoreResult<Self> {
        let mut file = File::open(path).map_err(|e| CoreError::Other(format!("abriendo {}: {e}", path.display())))?;
        file.seek(SeekFrom::Start(start_offset))
            .map_err(|e| CoreError::Other(format!("buscando offset {start_offset} en {}: {e}", path.display())))?;
        Ok(Self {
            reader: BufReader::new(file),
            source_id: source_id_for(path),
            plantilla,
            roles,
            bytes_read: start_offset,
            path: path.to_path_buf(),
        })
    }

    /// Siguiente línea cruda sin salto final; avanza `bytes_read` con los
    /// bytes crudos (incluido el salto).
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

impl Cursor for TemplateCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        loop {
            let line_offset = self.bytes_read;
            let raw = self
                .read_raw_line()
                .map_err(|e| CoreError::Other(format!("leyendo línea: {e}")))?;
            let Some(raw) = raw else {
                return Ok(None);
            };
            if raw.trim().is_empty() {
                continue; // en blanco: no es evento, pero cuenta para el offset.
            }
            // Línea que no casa la plantilla: se descarta, no aborta.
            let Some(caps) = self.plantilla.casar(&raw) else {
                continue;
            };
            return Ok(Some(record::line_to_event(&caps, &self.roles, &raw, line_offset, &self.source_id)));
        }
    }

    fn watermark(&self) -> Watermark {
        let value = match hash_prefix(&self.path, self.bytes_read) {
            Ok(h) => format!("off:{}|p:{}", self.bytes_read, h),
            Err(_) => format!("off:{}", self.bytes_read),
        };
        Watermark { kind: "template".to_string(), value }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("format".to_string(), "template".to_string());
        m.insert("template".to_string(), self.plantilla.texto().to_string());
        m
    }
}
