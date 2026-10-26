//! Índices de otra versión de esquema: el error que lo dice en lenguaje de
//! usuario y la lectura de sus fuentes, que es lo único que hace falta para
//! reconstruirlo (`chrono reindex`).

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::{Result, SourceRow, SCHEMA_VERSION};

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

    /// `v5`, o `?` si el índice no la guarda.
    pub fn found_label(&self) -> String {
        self.found.as_deref().map_or_else(|| "?".to_string(), |v| format!("v{v}"))
    }
}

impl std::fmt::Display for SchemaMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

/// El error de abrir un índice dice que el fichero no es una base SQLite o que
/// está dañado (y no que esté ocupado, sin permisos o inaccesible): lo único que
/// justifica apartarlo y crear otro.
pub fn is_corrupt(e: &(dyn std::error::Error + 'static)) -> bool {
    use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
    e.downcast_ref::<rusqlite::Error>().and_then(rusqlite::Error::sqlite_error_code).is_some_and(|c| matches!(c, NotADatabase | DatabaseCorrupt))
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
