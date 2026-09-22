//! Similar: eventos casi-duplicados por distancia de Hamming del simhash.
//! Paridad con `Similar` en `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// Un evento casi-duplicado.
#[derive(Debug, Clone, PartialEq)]
pub struct SimilarRow {
    pub id: String,
    pub title: String,
    pub distance: u32,
}

/// Casi-duplicados del evento `id` por distancia de Hamming del `simhash`
/// (`(a ^ b).count_ones()`). Escaneo en memoria (igual que el Go): a esta
/// escala es aceptable. Orden asc por distancia, top `limit`.
pub fn similar(conn: &Connection, id: &str, max_dist: u32, limit: usize) -> Result<Vec<SimilarRow>> {
    // Resuelve por prefijo (igual que `sha LIKE ?||'%'` en el Go): la CLI y
    // los ejemplos de `bugs`/`branches` usan ids cortos de 8 caracteres.
    let (id, target): (String, i64) = conn.query_row(
        "SELECT id, simhash FROM events WHERE id LIKE ?1||'%' ORDER BY id LIMIT 1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let id = id.as_str();
    let target = target as u64;

    let mut stmt = conn.prepare("SELECT id, title, simhash FROM events WHERE id != ?1")?;
    let rows = stmt.query_map(params![id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (other_id, title, sh) = row?;
        let dist = (target ^ (sh as u64)).count_ones();
        if dist <= max_dist {
            out.push(SimilarRow { id: other_id, title, distance: dist });
        }
    }
    out.sort_by(|a, b| a.distance.cmp(&b.distance));
    out.truncate(limit);
    Ok(out)
}
