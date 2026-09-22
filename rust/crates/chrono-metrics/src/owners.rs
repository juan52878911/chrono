//! Owners: propiedad por autor de una ruta (prefijo) + bus factor. Paridad
//! con `Owners` en `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// Propiedad de un autor sobre el prefijo consultado.
#[derive(Debug, Clone, PartialEq)]
pub struct Owner {
    pub name: String,
    pub commits: i64,
    pub share: f64,
}

/// Propiedad por autor de las entidades cuyo `key` empieza por `prefix`.
/// Devuelve (owners ordenados por cambios desc con `share` calculado, y
/// `bus_factor`: nº mínimo de autores que acumulan más del 50% del total,
/// recorriendo la lista ya ordenada).
pub fn owners(conn: &Connection, prefix: &str, since_epoch: i64) -> Result<(Vec<Owner>, i64)> {
    let like_pattern = format!("{prefix}%");
    let mut stmt = conn.prepare(
        "SELECT COALESCE(a.display_name, a.key, '') AS name, COUNT(*) AS commits
         FROM touches t
         JOIN entities e ON e.id = t.entity_id
         JOIN events ev ON ev.id = t.event_id
         JOIN actors a ON a.id = ev.actor_id
         WHERE e.key LIKE ?1 AND ev.at_epoch >= ?2
         GROUP BY ev.actor_id
         ORDER BY commits DESC",
    )?;
    let rows = stmt.query_map(params![like_pattern, since_epoch], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;

    let mut out = Vec::new();
    let mut total = 0i64;
    for row in rows {
        let (name, commits) = row?;
        total += commits;
        out.push(Owner { name, commits, share: 0.0 });
    }

    let mut bus_factor = 0i64;
    let mut acc = 0i64;
    for o in &mut out {
        if total > 0 {
            o.share = o.commits as f64 / total as f64;
        }
        // Igual que el Go: se cuenta ESTE autor si el acumulado ANTES de
        // sumarlo aún no llega a la mitad (puede sobrepasar el 50% al sumar).
        if acc < (total + 1) / 2 {
            acc += o.commits;
            bus_factor += 1;
        }
    }
    Ok((out, bus_factor))
}
