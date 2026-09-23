//! Coupling: qué cambia junto a una entidad dada. Paridad con `Coupling` en
//! `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// Una entidad acoplada a la consultada.
#[derive(Debug, Clone, PartialEq)]
pub struct Coupled {
    pub b: String,
    pub support: i64,
    pub confidence: f64,
}

/// Qué cambia junto a `entity`. Excluye eventos `is_bulk=1` (un commit masivo
/// contaminaría el acoplamiento). Devuelve (pares acoplados ordenados por
/// soporte desc, nº de cambios no-masivos de `entity` en la ventana).
pub fn coupling(
    conn: &Connection,
    entity: &str,
    min_support: i64,
    since_epoch: i64,
    limit: usize,
    symbols_only: bool,
) -> Result<(Vec<Coupled>, i64)> {
    // El ancla (`e.key = ?1`) se casa por clave exacta, así que no necesita
    // filtro de tipo; el alcance `symbols_only` filtra las entidades ACOPLADAS
    // devueltas (`e2`): por defecto ficheros/otras (no símbolos), con `--by
    // symbol` solo símbolos.
    let op = crate::symbol_type_op(symbols_only);
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM touches t
         JOIN entities e ON e.id = t.entity_id
         JOIN events ev ON ev.id = t.event_id
         WHERE e.key = ?1 AND ev.is_bulk = 0 AND ev.at_epoch >= ?2",
        params![entity, since_epoch],
        |r| r.get(0),
    )?;

    let mut stmt = conn.prepare(&format!(
        "SELECT e2.key, COUNT(*) AS support
         FROM touches t2
         JOIN entities e2 ON e2.id = t2.entity_id
         WHERE t2.event_id IN (
             SELECT t1.event_id FROM touches t1
             JOIN entities e1 ON e1.id = t1.entity_id
             JOIN events ev ON ev.id = t1.event_id
             WHERE e1.key = ?1 AND ev.is_bulk = 0 AND ev.at_epoch >= ?2
         ) AND e2.key != ?1 AND e2.type {op} 'symbol'
         GROUP BY e2.id
         HAVING support >= ?3
         ORDER BY support DESC
         LIMIT ?4",
    ))?;
    let rows = stmt.query_map(params![entity, since_epoch, min_support, limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (b, support) = row?;
        let confidence = if total > 0 { support as f64 / total as f64 } else { 0.0 };
        out.push(Coupled { b, support, confidence });
    }
    Ok((out, total))
}
