//! Cursor de streaming sobre un fichero de texto plano: RAM constante (una
//! línea a la vez), con seguimiento de offset en bytes para el watermark
//! incremental (`off:<bytes>`) y reanudación en `open`. Mismo modelo que
//! `chrono-source-jsonl::cursor`.

use crate::preset::{self, Preset};
use crate::record;
use chrono_core::{CoreError, Cursor, Event, Result as CoreResult, Watermark};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Lee la primera línea no vacía del fichero (para autodetectar el preset en
/// `detect`/`open`), o `None` si el fichero no se puede leer o no tiene
/// ninguna línea no vacía.
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

/// `source_id` = "textlog:<ruta absoluta>". Ruta tal cual si no se puede
/// canonicalizar (fichero borrado entre `detect`/`open`, symlink roto…).
fn source_id_for(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    format!("textlog:{}", abs.to_string_lossy())
}

/// Tope de bytes que se hashean como "prefijo" del fichero.
const PREFIX_CAP: u64 = 4096;

/// Calcula el hash FNV-1a 64 (16 dígitos hex) de los primeros
/// `min(PREFIX_CAP, limit)` bytes de `path`. `limit` es el offset del
/// watermark (la región YA consumida): así un append (que solo añade bytes
/// DESPUÉS del offset) no cambia el prefijo hasheado y NO dispara divergencia;
/// una reescritura de los bytes ya consumidos SÍ. Se lee el fichero aparte
/// (fichero nuevo, no el buffer del cursor), tanto en `watermark()` como al
/// comprobar divergencia en `TextlogSource::open` (ver `docs/DESIGN-GENERAL-CORE.md §3`).
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

/// Parsea el valor de un watermark de este adaptador: separa la parte
/// `off:<n>` de la opcional `|p:<hex>` (hash del prefijo, formato nuevo).
/// Devuelve `None` si no tiene el prefijo `off:` esperado o el número no es
/// válido. Retrocompatible: un watermark viejo `off:<n>` a secas devuelve
/// `(n, None)` — sin hash que comprobar.
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

pub struct TextlogCursor {
    reader: BufReader<File>,
    source_id: String,
    preset: Preset,
    /// Año de referencia para syslog RFC3164 (ver `preset::fallback_year_from`).
    fallback_year: i64,
    /// Bytes consumidos desde el principio del fichero (posición actual);
    /// arranca en el offset del watermark de entrada, si lo hay.
    bytes_read: u64,
    /// Ruta del fichero, para recalcular el hash del prefijo en `watermark()`
    /// sobre `min(4096, bytes_read)` (la región consumida hasta ese momento).
    path: PathBuf,
}

impl TextlogCursor {
    /// Abre el cursor sobre `path` con el preset ya resuelto, arrancando la
    /// lectura en `start_offset` bytes (0 si no hay watermark previo).
    pub fn open(path: &Path, start_offset: u64, preset: Preset, fallback_year: i64) -> CoreResult<Self> {
        let mut file = File::open(path).map_err(|e| CoreError::Other(format!("abriendo {}: {e}", path.display())))?;
        file.seek(SeekFrom::Start(start_offset))
            .map_err(|e| CoreError::Other(format!("buscando offset {start_offset} en {}: {e}", path.display())))?;

        Ok(Self {
            reader: BufReader::new(file),
            source_id: source_id_for(path),
            preset,
            fallback_year,
            bytes_read: start_offset,
            path: path.to_path_buf(),
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

impl Cursor for TextlogCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        loop {
            let line_offset = self.bytes_read;
            let raw = self
                .read_raw_line()
                .map_err(|e| CoreError::Other(format!("leyendo línea textlog: {e}")))?;
            let Some(raw) = raw else {
                return Ok(None);
            };
            if raw.trim().is_empty() {
                continue; // línea en blanco: no es un evento, pero cuenta para el offset.
            }
            // Línea que no casa el preset activo: se descarta, igual que
            // `chrono-source-jsonl` descarta líneas no-JSON.
            let Some(parsed) = preset::parse_line(self.preset, &raw, self.fallback_year) else {
                continue;
            };
            return Ok(Some(record::line_to_event(&parsed, &raw, line_offset, &self.source_id)));
        }
    }

    fn watermark(&self) -> Watermark {
        // Hash del prefijo sobre la región consumida (`min(4096, bytes_read)`).
        // Si no se puede leer el fichero, se cae al formato antiguo `off:<n>`.
        let value = match hash_prefix(&self.path, self.bytes_read) {
            Ok(h) => format!("off:{}|p:{}", self.bytes_read, h),
            Err(_) => format!("off:{}", self.bytes_read),
        };
        Watermark { kind: "textlog".to_string(), value }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("format".to_string(), "textlog".to_string());
        m.insert("preset".to_string(), self.preset.as_str().to_string());
        if self.preset == Preset::Syslog {
            m.insert("year".to_string(), self.fallback_year.to_string());
        }
        m
    }
}
