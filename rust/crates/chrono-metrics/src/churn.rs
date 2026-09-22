//! Churn: líneas +/- por entity en la ventana. Paridad con `Churn` en
//! `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// Fila de churn (añadido/borrado) de una entity.
#[derive(Debug, Clone, PartialEq)]
pub struct ChurnRow {
    pub entity: String,
    pub added: i64,
    pub deleted: i64,
}

/// Suma de `added`/`deleted` (leídos de `touches.attrs` JSON) por entity en
/// la ventana, top `limit`. Ignora binarios: si `added`/`deleted` están
/// ausentes o son negativos, se tratan como 0 (igual que `MAX(x,0)` en el Go).
pub fn churn(conn: &Connection, since_epoch: i64, limit: usize) -> Result<Vec<ChurnRow>> {
    let mut stmt = conn.prepare(
        // Alias `a`/`d` (como el Go), NO `added`/`deleted`: en el ORDER BY,
        // `deleted` resolvería a la columna `entities.deleted` (siempre 0 aquí)
        // en vez de al alias, y el orden quedaría solo por `added`.
        "SELECT e.key,
             SUM(MAX(CAST(COALESCE(json_extract(t.attrs, '$.added'), 0) AS INTEGER), 0)) AS a,
             SUM(MAX(CAST(COALESCE(json_extract(t.attrs, '$.deleted'), 0) AS INTEGER), 0)) AS d
         FROM touches t
         JOIN events ev ON ev.id = t.event_id
         JOIN entities e ON e.id = t.entity_id
         WHERE e.excluded = 0 AND ev.at_epoch >= ?1
         GROUP BY e.id
         ORDER BY (a + d) DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_epoch, limit as i64], |r| {
        Ok(ChurnRow { entity: r.get(0)?, added: r.get(1)?, deleted: r.get(2)? })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
