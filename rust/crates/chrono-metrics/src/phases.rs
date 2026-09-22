//! Phases: fases del proyecto desde los marcadores de tipo `tag`. Paridad
//! con `Phases` en `internal/metrics/metrics.go`.

use rusqlite::Connection;

use crate::Result;

/// Una fase del proyecto (un tag/marker con su fecha).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phase {
    pub name: String,
    pub date: String,
}

/// Marcadores `kind='tag'`, ordenados por fecha (`markers.at`).
pub fn phases(conn: &Connection) -> Result<Vec<Phase>> {
    let mut stmt =
        conn.prepare("SELECT name, COALESCE(at, '') FROM markers WHERE kind = 'tag' ORDER BY at")?;
    let rows = stmt.query_map([], |r| Ok(Phase { name: r.get(0)?, date: r.get(1)? }))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
