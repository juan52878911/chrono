//! Índices de otra versión de esquema: el error que lo dice en lenguaje de
//! usuario y la lectura de sus fuentes, que es lo único que hace falta para
//! reconstruirlo (`chrono reindex`).

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::{Result, SourceRow, SCHEMA_VERSION};

/// No hay índice en `path` (y quien lo abría no debe crearlo). El texto va en
/// español, como el resto de errores del store; el CLI lo traduce.
#[derive(Debug, Clone, PartialEq)]
pub struct NoIndex {
    pub path: std::path::PathBuf,
}

impl std::fmt::Display for NoIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no hay índice en {} (créalo con chrono init)", self.path.display())
    }
}

impl std::error::Error for NoIndex {}

/// El índice tiene un esquema que esta versión no lee. `found` es lo que dice
/// su `meta.schema_version` (`None` si no lo dice).
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaMismatch {
    pub found: Option<String>,
    pub want: i64,
}

impl SchemaMismatch {
    pub(crate) fn new(found: Option<String>) -> SchemaMismatch {
        SchemaMismatch { found, want: SCHEMA_VERSION }
    }

    /// El índice lo hizo un chrono más nuevo que este (no hay que reindexar,
    /// sino actualizar chrono).
    pub fn is_newer(&self) -> bool {
        self.found.as_deref().and_then(|v| v.parse::<i64>().ok()).is_some_and(|v| v > self.want)
    }

    /// El índice no dice su versión (tiene `meta` sin `schema_version`): a medio
    /// crear por otro proceso, o no es de chrono. Reindexar no es la salida.
    pub fn is_unknown(&self) -> bool {
        self.found.is_none()
    }

    /// `v5`, o `?` si el índice no la guarda.
    pub fn found_label(&self) -> String {
        self.found.as_deref().map_or_else(|| "?".to_string(), |v| format!("v{v}"))
    }
}

impl std::fmt::Display for SchemaMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_unknown() {
            return write!(f, "{}", UNKNOWN_SCHEMA);
        }
        if self.is_newer() {
            return write!(
                f,
                "el índice es de una versión más reciente de chrono (esquema {}; esta versión lee el v{}): actualiza chrono",
                self.found_label(),
                self.want
            );
        }
        write!(
            f,
            "el índice es de una versión anterior de chrono (esquema {}; esta versión necesita el v{}). \
             Reconstrúyelo con `chrono reindex`: vuelve a indexar las mismas fuentes",
            self.found_label(),
            self.want
        )
    }
}

impl std::error::Error for SchemaMismatch {}

/// Mensaje de un índice sin versión de esquema.
const UNKNOWN_SCHEMA: &str =
    "el índice no dice su versión de esquema: otro proceso lo está creando ahora mismo (repite en un momento) o no es un índice de chrono";

/// El error de abrir un índice dice que el fichero no es una base SQLite o que
/// está dañado (y no que esté ocupado, sin permisos o inaccesible): lo único que
/// justifica apartarlo y crear otro.
pub fn is_corrupt(e: &(dyn std::error::Error + 'static)) -> bool {
    use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
    e.downcast_ref::<rusqlite::Error>().and_then(rusqlite::Error::sqlite_error_code).is_some_and(|c| matches!(c, NotADatabase | DatabaseCorrupt))
}

/// Marca de un `reindex` a medias de versiones anteriores: `<db>.reindexing`,
/// con la ruta de la copia del índice viejo dentro. Aquel `reindex` apartaba el
/// índice y lo reconstruía en su sitio; si la marca sigue ahí, el índice puede
/// estar a medias y la copia es la buena. El de ahora construye aparte y no la
/// escribe: solo se lee para reanudar uno de antes.
pub fn reindex_marker(db: &Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.reindexing", db.display()))
}

/// Si hay un `reindex` sin terminar sobre `db`: la copia buena y si sigue en
/// marcha (otro proceso tiene el cerrojo de ingesta, `<db>.lock`) o se cortó.
pub fn reindex_pending(db: &Path) -> Option<(std::path::PathBuf, bool)> {
    let backup = std::fs::read_to_string(reindex_marker(db)).ok()?;
    let running = std::fs::OpenOptions::new()
        .write(true)
        .open(format!("{}.lock", db.display()))
        .is_ok_and(|f| matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    Some((std::path::PathBuf::from(backup.trim()), running))
}

/// Las fuentes registradas en un índice de cualquier versión, en el orden en
/// que se añadieron, sin comprobar el esquema ni escribir nada.
pub fn read_sources_any_version(path: &Path) -> Result<Vec<SourceRow>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT id, kind, COALESCE(path, ''), COALESCE(watermark, ''), COALESCE(manifest_json, '{}')
         FROM sources ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SourceRow { id: r.get(0)?, kind: r.get(1)?, path: r.get(2)?, watermark: r.get(3)?, manifest_json: r.get(4)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Un valor de `meta` de un índice de cualquier versión (`schema_version`,
/// `symbols`…), sin abrirlo con `Store::open`.
pub fn read_meta_any_version(path: &Path, key: &str) -> Option<String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).ok()
}

/// Eventos de un índice de cualquier versión (para comparar antes y después
/// de reindexar); `None` si no se puede leer.
pub fn count_events_any_version(path: &Path) -> Option<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).ok()
}

/// Eventos de una fuente en un índice de cualquier versión (`source_ref` desde
/// la v5, `source_id` antes); `None` si no se puede leer.
pub fn count_source_events_any_version(path: &Path, source_id: &str) -> Option<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row("SELECT COUNT(*) FROM events WHERE source_ref = (SELECT ref FROM sources WHERE id = ?1)", [source_id], |r| r.get(0))
        .or_else(|_| conn.query_row("SELECT COUNT(*) FROM events WHERE source_id = ?1", [source_id], |r| r.get(0)))
        .ok()
}

/// Vacía el WAL de `path` en el fichero principal y lo deja a 0 bytes
/// (`wal_checkpoint(TRUNCATE)`), reintentando mientras un lector lo impida
/// (hasta `wait`). Tras esto el `.db` solo, sin `-wal`, tiene todo el índice:
/// se puede enlazar como copia o sustituir por otro en la misma ruta sin que un
/// `-wal` con páginas suyas acabe aplicado al fichero nuevo.
pub fn checkpoint_truncate(path: &Path, wait: std::time::Duration) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_millis(200))?;
    let start = std::time::Instant::now();
    loop {
        let r = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0));
        match r {
            Ok(0) => return Ok(()),
            Ok(_) | Err(_) if start.elapsed() < wait => std::thread::sleep(std::time::Duration::from_millis(50)),
            Ok(_) => return Err(format!("{}: los lectores no dejan vaciar el WAL", path.display()).into()),
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mensaje_de_usuario_sin_detalles_internos() {
        let viejo = SchemaMismatch::new(Some("5".into())).to_string();
        assert!(viejo.contains("esquema v5") && viejo.contains("v7") && viejo.contains("chrono reindex"), "{viejo}");
        assert!(!viejo.contains("Some(") && !viejo.contains("Store::reset"), "{viejo}");
        let nuevo = SchemaMismatch::new(Some("99".into()));
        assert!(nuevo.is_newer() && nuevo.to_string().contains("actualiza chrono"), "{nuevo}");
        assert_eq!(SchemaMismatch::new(None).found_label(), "?");
    }
}
