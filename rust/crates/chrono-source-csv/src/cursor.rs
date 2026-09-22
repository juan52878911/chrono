//! Cursor de streaming sobre un fichero CSV: RAM constante (una fila a la
//! vez), con seguimiento de offset en bytes para el watermark incremental
//! (`off:<bytes leídos>`) y reanudación en `open`.
//!
//! La cabecera (nombres de columna) se resuelve SIEMPRE leyendo el principio
//! del fichero, en una pasada aparte (`peek_header`), nunca desde la línea de
//! arranque de un watermark incremental: así `manifest()` y la resolución de
//! columnas son estables entre un `init` (offset 0) y un `sync` (offset>0).
//! Ver `chrono-source-jsonl::cursor::peek_first_nonempty_line`, mismo patrón.

use crate::config::{self, CsvOverrides, ResolvedColumns};
use crate::parse::parse_csv_line;
use crate::record;
use chrono_core::{CoreError, Cursor, Event, Result as CoreResult, SourceConfig, Watermark};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Lee la primera línea no vacía del fichero (cabecera CSV), o `None` si el
/// fichero no se puede leer o no tiene ninguna línea no vacía. Usada tanto
/// para `detect` (chequeo de forma) como para resolver la cabecera real.
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

/// Cabecera (nombres de columna) partida con `delimiter`, o `None` si el
/// fichero no tiene ninguna línea no vacía.
fn peek_header(path: &Path, delimiter: char) -> Option<Vec<String>> {
    let line = peek_first_nonempty_line(path)?;
    Some(parse_csv_line(&line, delimiter))
}

/// `source_id` = "csv:<ruta absoluta>". Usa la ruta tal cual si no se puede
/// canonicalizar (fichero borrado entre el `detect` y el `open`, symlink
/// roto…), igual que `chrono-source-jsonl`.
fn source_id_for(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    format!("csv:{}", abs.to_string_lossy())
}

/// Tope de bytes que se hashean como "prefijo" del fichero.
const PREFIX_CAP: u64 = 4096;

/// Calcula el hash FNV-1a 64 (16 dígitos hex) de los primeros
/// `min(PREFIX_CAP, limit)` bytes de `path`. `limit` es el offset del
/// watermark (la región YA consumida): así un append (que solo añade bytes
/// DESPUÉS del offset) no cambia el prefijo hasheado y NO dispara divergencia;
/// una reescritura de los bytes ya consumidos SÍ. Se lee el fichero aparte
/// (fichero nuevo, no el buffer del cursor), tanto en `watermark()` como al
/// comprobar divergencia en `CsvSource::open` (ver `docs/DESIGN-GENERAL-CORE.md §3`).
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

pub struct CsvCursor {
    reader: BufReader<File>,
    source_id: String,
    /// Nombres de columna de la cabecera, en el orden del fichero.
    header: Vec<String>,
    /// Índices resueltos UNA VEZ contra `header` (ver módulo `config`).
    columns: ResolvedColumns,
    delimiter: char,
    /// Bytes consumidos desde el principio del fichero (posición actual);
    /// arranca en el offset del watermark de entrada, si lo hay (la
    /// cabecera ya fue descontada en una ejecución anterior).
    bytes_read: u64,
    /// Ruta del fichero, para recalcular el hash del prefijo en `watermark()`
    /// sobre `min(4096, bytes_read)` (la región consumida hasta ese momento).
    path: PathBuf,
}

impl CsvCursor {
    /// Abre el cursor sobre `path`. Si `start_offset` es 0 (primera
    /// ingesta), la cabecera se consume de la lectura de datos (no se
    /// entrega como fila); si `start_offset` > 0 (sync incremental), se
    /// asume que ya apunta después de la cabecera y de cualquier fila ya
    /// leída, y NO se vuelve a tratar la primera línea leída como cabecera.
    pub fn open(path: &Path, start_offset: u64, cfg: &SourceConfig) -> CoreResult<Self> {
        let delimiter = config::resolve_delimiter(cfg);
        let header = peek_header(path, delimiter)
            .ok_or_else(|| CoreError::Other(format!("{}: fichero CSV sin cabecera (vacío)", path.display())))?;
        let overrides: CsvOverrides = config::overrides_from(cfg);
        let columns = config::resolve_columns(&header, &overrides);

        let file = File::open(path).map_err(|e| CoreError::Other(format!("abriendo {}: {e}", path.display())))?;
        let mut reader = BufReader::new(file);

        // OJO: usamos el MISMO `reader` para saltar la cabecera y para leer
        // datos después. Si en su lugar envolviéramos el fichero en un
        // `BufReader` temporal aparte solo para saltar la cabecera, su
        // buffering interno podría leer del fichero más allá de la línea de
        // cabecera; al descartar ese lector y crear uno nuevo sobre el mismo
        // `File`, la posición física ya habría avanzado de más y perderíamos
        // datos. `BufReader::seek` sí es seguro: descarta su buffer interno
        // y reposiciona el fichero subyacente correctamente.
        let bytes_read = if start_offset == 0 {
            skip_header_line(&mut reader)
                .map_err(|e| CoreError::Other(format!("leyendo cabecera de {}: {e}", path.display())))?
        } else {
            reader
                .seek(SeekFrom::Start(start_offset))
                .map_err(|e| CoreError::Other(format!("buscando offset {start_offset} en {}: {e}", path.display())))?;
            start_offset
        };

        Ok(Self { reader, source_id: source_id_for(path), header, columns, delimiter, bytes_read, path: path.to_path_buf() })
    }

    /// Lee la siguiente línea cruda (sin salto de línea final), o `None` al
    /// agotar el fichero. Avanza `bytes_read` con los bytes crudos leídos
    /// (incluido el salto de línea), para que el watermark refleje la
    /// posición real en el fichero.
    fn read_raw_line(&mut self) -> std::io::Result<Option<String>> {
        read_raw_line_from(&mut self.reader, &mut self.bytes_read)
    }
}

/// Lee una línea cruda de `reader`, descontando `\n`/`\r\n` finales y sumando
/// los bytes crudos consumidos a `*bytes_read`. Compartida entre el cursor
/// (que trackea su propio contador) y `skip_header_line` (que arranca desde
/// 0 al abrir en offset 0).
fn read_raw_line_from<R: BufRead>(reader: &mut R, bytes_read: &mut u64) -> std::io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    let n = reader.read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    *bytes_read += n as u64;
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Consume del principio del fichero cualquier línea en blanco y luego la
/// cabecera (primera línea no vacía), devolviendo el total de bytes crudos
/// consumidos. Es la contraparte, sobre el propio handle de lectura de
/// datos, de lo que `peek_header` calculó en una pasada de solo lectura.
fn skip_header_line<R: BufRead>(reader: &mut R) -> std::io::Result<u64> {
    let mut bytes_read = 0u64;
    loop {
        let line_start = bytes_read;
        match read_raw_line_from(reader, &mut bytes_read)? {
            None => return Ok(bytes_read), // fichero vacío o solo líneas en blanco: nada que hacer.
            Some(line) => {
                if !line.trim().is_empty() {
                    return Ok(bytes_read);
                }
                let _ = line_start; // línea en blanco antes de la cabecera: se descarta y se sigue.
            }
        }
    }
}

impl Cursor for CsvCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        loop {
            let line_offset = self.bytes_read;
            let raw = self.read_raw_line().map_err(|e| CoreError::Other(format!("leyendo fila csv: {e}")))?;
            let Some(raw) = raw else {
                return Ok(None);
            };
            if raw.trim().is_empty() {
                continue; // línea en blanco: no es una fila, pero cuenta para el offset.
            }
            let fields = parse_csv_line(&raw, self.delimiter);
            return Ok(Some(record::row_to_event(&fields, &self.header, &self.columns, &raw, line_offset, &self.source_id)));
        }
    }

    fn watermark(&self) -> Watermark {
        // Hash del prefijo sobre la región consumida (`min(4096, bytes_read)`).
        // Si no se puede leer el fichero, se cae al formato antiguo `off:<n>`
        // (sin hash) para no perder el watermark.
        let value = match hash_prefix(&self.path, self.bytes_read) {
            Ok(h) => format!("off:{}|p:{}", self.bytes_read, h),
            Err(_) => format!("off:{}", self.bytes_read),
        };
        Watermark { kind: "csv".to_string(), value }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("format".to_string(), "csv".to_string());
        let delim_str = if self.delimiter == '\t' { "\\t".to_string() } else { self.delimiter.to_string() };
        m.insert("delimiter".to_string(), delim_str);
        m.insert("time_column".to_string(), self.columns.name(&self.header, self.columns.time));
        m.insert("message_column".to_string(), self.columns.name(&self.header, self.columns.message));
        m.insert("level_column".to_string(), self.columns.name(&self.header, self.columns.level));
        m.insert("entity_column".to_string(), self.columns.name(&self.header, self.columns.entity));
        m.insert("actor_column".to_string(), self.columns.name(&self.header, self.columns.actor));
        m.insert("id_column".to_string(), self.columns.name(&self.header, self.columns.id));
        m
    }
}
