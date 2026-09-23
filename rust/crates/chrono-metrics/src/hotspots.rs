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

/// Entidades más cambiadas ponderadas por `entities.size`. Filtra `deleted=0`,
/// `excluded=0`. Por defecto EXCLUYE símbolos (`type != 'symbol'`); con
/// `symbols_only=true` (consultas `--by symbol`) devuelve SOLO símbolos.
///
/// No se filtra por `type='file'`: en git las entidades de fichero son idénticas
/// al Go, pero así el adaptador de logs (entity_type='log-source') u otras
/// trazas también rankean. `MAX(size,1)` = fallback a frecuencia cuando no hay
/// tamaño (logs y símbolos sin S2), sin alterar el top de git (que sí lo tiene).
pub fn hotspots(conn: &Connection, since_epoch: i64, limit: usize, symbols_only: bool) -> Result<Vec<Hotspot>> {
    let op = crate::symbol_type_op(symbols_only);
    let mut stmt = conn.prepare(&format!(
        "SELECT e.key, COUNT(*) AS changes, e.size
         FROM touches t
         JOIN events ev ON ev.id = t.event_id
         JOIN entities e ON e.id = t.entity_id
         WHERE e.deleted = 0 AND e.excluded = 0 AND e.type {op} 'symbol' AND ev.at_epoch >= ?1
         GROUP BY e.id
         ORDER BY (CAST(COUNT(*) AS REAL) * MAX(e.size, 1)) DESC
         LIMIT ?2",
    ))?;
    let rows = stmt.query_map(params![since_epoch, limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;

    let mut out = Vec::new();
    let mut max_raw = 0f64;
    for row in rows {
        let (entity, changes, size) = row?;
        let raw = changes as f64 * size.max(1) as f64;
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
