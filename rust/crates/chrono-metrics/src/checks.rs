//! Comprobaciones baratas para explicar un resultado vacío o engañoso: un
//! nivel o una attr que no existen en el índice, un índice sin git, eventos
//! sin tiempo. No cambian ninguna cifra; los frontales (CLI y MCP) las
//! convierten en avisos (`hints`) en su idioma.

use rusqlite::{params, Connection, OptionalExtension};

use crate::timeline::EventScope;
use crate::Result;

/// Niveles que inventa `infer_level` (ver `chrono_core::infer_level`).
const INFERRED: [&str; 4] = ["FATAL", "CRITICAL", "ERROR", "WARNING"];

/// Los niveles de `levels` (lista con comas) que no tiene ningún evento, y los
/// que hay (para decirlos). Sin mayúsculas, como el filtro. Con `infer`, los
/// que deduce `infer_level` también cuentan como existentes.
pub fn unknown_levels(conn: &Connection, levels: &str, infer: bool) -> Result<(Vec<String>, Vec<String>)> {
    let mut stmt = conn.prepare("SELECT DISTINCT level FROM events WHERE level != '' ORDER BY level")?;
    let known: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<std::result::Result<_, _>>()?;
    let upper: Vec<String> = known.iter().map(|l| l.to_uppercase()).collect();
    let unknown = levels
        .split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| {
            let u = l.to_uppercase();
            let exists = upper.contains(&u) || (infer && INFERRED.contains(&u.as_str()));
            !exists
        })
        .map(str::to_string)
        .collect();
    Ok((unknown, known))
}

/// Algún evento tiene la attr `key` (como dim o en el JSON de `attrs`). Para
/// una ruta `a.b`, basta con que exista `a`.
pub fn attr_key_known(conn: &Connection, key: &str) -> Result<bool> {
    let head = key.split('.').next().unwrap_or(key);
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attr_keys WHERE key IN (?1, ?2)) OR EXISTS(SELECT 1 FROM dim_values WHERE key IN (?1, ?2))",
        params![key, head],
        |r| r.get(0),
    )?)
}

/// Las claves de attr que hay en el índice (orden alfabético, como mucho `max`).
pub fn attr_keys(conn: &Connection, max: usize) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT key FROM (SELECT key FROM attr_keys UNION SELECT key FROM dim_values)
         WHERE key != 'level_inferred' ORDER BY key LIMIT ?1",
    )?;
    let keys = stmt.query_map(params![max as i64], |r| r.get::<_, String>(0))?.collect::<std::result::Result<_, _>>()?;
    Ok(keys)
}

/// Cuántas fuentes de tipo `kind` (`git`, `jsonl`…) tiene el índice.
pub fn sources_of_kind(conn: &Connection, kind: &str) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM sources WHERE kind = ?1", params![kind], |r| r.get(0))?)
}

/// Cuántas fuentes tienen eventos con tiempo además de la del evento `id`
/// (`correlate` por tiempo solo mira OTRAS fuentes). `None` si no existe.
pub fn other_timed_sources(conn: &Connection, id: &str) -> Result<Option<i64>> {
    let sref: Option<i64> = conn.query_row("SELECT source_ref FROM events WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
    let Some(sref) = sref else { return Ok(None) };
    let n = conn.query_row(
        "SELECT COUNT(*) FROM sources s WHERE s.ref != ?1
           AND EXISTS(SELECT 1 FROM events e WHERE e.source_ref = s.ref AND e.at_epoch > 0)",
        params![sref],
        |r| r.get(0),
    )?;
    Ok(Some(n))
}

/// Las claves de relación (links y dims) que tiene el evento `id`: las que
/// valen en `correlate --via` para él.
pub fn event_relation_keys(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT k FROM (
             SELECT v.key k FROM events e JOIN event_dims d ON d.event_rid = e.rid JOIN dim_values v ON v.id = d.dim_id WHERE e.id = ?1
             UNION
             SELECT lt.rel k FROM events e JOIN event_links el ON el.event_rid = e.rid JOIN link_targets lt ON lt.id = el.target_id WHERE e.id = ?1)
         ORDER BY k",
    )?;
    let keys = stmt.query_map(params![id], |r| r.get::<_, String>(0))?.collect::<std::result::Result<_, _>>()?;
    Ok(keys)
}

/// Eventos SIN tiempo (`at_epoch <= 0`) que casan con `scope`: no caben en
/// ninguna vista temporal (`timeline`, `events`), así que se dicen aparte.
pub fn untimed_events(conn: &Connection, scope: &EventScope) -> Result<i64> {
    let scope = EventScope { until_epoch: 0, ..scope.clone() };
    let (filters, extra) = scope.sql(1);
    let sql = format!("SELECT COUNT(*) FROM events e WHERE e.at_epoch <= 0 {filters}");
    let bound: Vec<&dyn rusqlite::ToSql> = extra.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    Ok(conn.query_row(&sql, rusqlite::params_from_iter(bound), |r| r.get(0))?)
}
