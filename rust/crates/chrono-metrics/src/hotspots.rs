//! Hotspots: ficheros que más cambian ponderados por su tamaño actual.
//! Paridad con `Hotspots` en `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// Un fichero caliente: cambia mucho y/o pesa mucho.
#[derive(Debug, Clone, PartialEq)]
pub struct Hotspot {
    pub entity: String,
    pub changes: i64,
    pub size: i64,
    /// `raw = changes * size` normalizado por el máximo DENTRO del top
    /// devuelto (igual que el Go: el `LIMIT` se aplica en SQL antes de
    /// normalizar, así que el primer resultado siempre vale 1.0).
    pub score: f64,
}

/// Ficheros más cambiados ponderados por `entities.size`. Filtra
/// `entities.type='file'`, `deleted=0`, `excluded=0` (igual que el Go).
pub fn hotspots(conn: &Connection, since_epoch: i64, limit: usize) -> Result<Vec<Hotspot>> {
    let mut stmt = conn.prepare(
        "SELECT e.key, COUNT(*) AS changes, e.size
         FROM touches t
         JOIN events ev ON ev.id = t.event_id
         JOIN entities e ON e.id = t.entity_id
         WHERE e.type = 'file' AND e.deleted = 0 AND e.excluded = 0 AND ev.at_epoch >= ?1
         GROUP BY e.id
         ORDER BY (CAST(COUNT(*) AS REAL) * e.size) DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_epoch, limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;

    let mut out = Vec::new();
    let mut max_raw = 0f64;
    for row in rows {
        let (entity, changes, size) = row?;
        let raw = changes as f64 * size as f64;
        if raw > max_raw {
            max_raw = raw;
        }
        out.push(Hotspot { entity, changes, size, score: raw });
    }
    if max_raw > 0.0 {
        for h in &mut out {
            h.score /= max_raw;
        }
    }
    Ok(out)
}
