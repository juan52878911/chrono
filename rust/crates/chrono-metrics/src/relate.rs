//! Relaciones por clave entre fuentes (P-REL-2). Una clave `K` es la unión de
//! los `links` con `rel = K` (los que escriben las `relations` de la config o
//! el adaptador) y las dims `K` (`event_dims`). Se compara por VALOR, así que
//! une una fuente que la guarda como link con otra que la guarda como dim.
//!
//! - `events --join K=v` (y cualquier consulta con `EventScope::join`): los
//!   eventos con ese valor; `K=@id`, los que comparten algún valor con `id`.
//! - [`correlate_via`]: todo lo que comparte la clave con un evento, contado.
//! - [`gaps`]: «X sin Y», las claves (o unidades) del lado `from` que no
//!   aparecen en el lado `to`.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::{EventRef, EventScope, Result};

/// Una clave usable en SQL literal: `[A-Za-z0-9_.-]`, como las dims.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn check_key(k: &str) -> Result<()> {
    if valid_key(k) {
        Ok(())
    } else {
        Err(format!("clave de relación no válida: {k:?} (usa [A-Za-z0-9_.-])").into())
    }
}

/// `(rid, value)` de la clave `key` (ya validada): links ∪ dims.
pub(crate) fn key_values_sql(key: &str) -> String {
    format!(
        "SELECT el.event_rid AS rid, lt.target AS value FROM link_targets lt JOIN event_links el ON el.target_id = lt.id WHERE lt.rel = '{key}'
         UNION ALL
         SELECT d.event_rid AS rid, v.value AS value FROM dim_values v JOIN event_dims d ON d.dim_id = v.id WHERE v.key = '{key}'"
    )
}

/// Rids con la clave `key` igual a (o, con `prefix`, que empieza por) el
/// parámetro `p`. Por los índices únicos `(rel, target)` y `(key, value)`. El
/// prefijo es literal y distingue mayúsculas, como la igualdad (no `LIKE`).
pub(crate) fn key_rids_sql(key: &str, p: &str, prefix: bool) -> String {
    let cmp = |col: &str| if prefix { format!("substr({col}, 1, length({p})) = {p}") } else { format!("{col} = {p}") };
    format!(
        "SELECT el.event_rid FROM link_targets lt JOIN event_links el ON el.target_id = lt.id WHERE lt.rel = '{key}' AND {}
         UNION ALL
         SELECT d.event_rid FROM dim_values v JOIN event_dims d ON d.dim_id = v.id WHERE v.key = '{key}' AND {}",
        cmp("lt.target"),
        cmp("v.value")
    )
}

/// Rids que comparten algún valor de `key` con el evento de id `p`.
pub(crate) fn key_rids_of_event_sql(key: &str, p: &str) -> String {
    let vals = key_values_sql(key);
    format!(
        "SELECT kv.rid FROM ({vals}) kv WHERE kv.value IN
             (SELECT kv2.value FROM ({vals}) kv2 WHERE kv2.rid = (SELECT rid FROM events WHERE id = {p}))"
    )
}

/// Cláusula de `EventScope::join` (`K=v`, `K=v*`, `K=@id` o solo `K`) sobre
/// el alias `t`, con su parámetro (o `None` si no lleva). Clave inválida: `None`.
pub(crate) fn join_clause(t: &str, raw: &str, param: &str) -> Option<(String, Option<String>)> {
    let (k, v) = raw.split_once('=').or_else(|| raw.split_once(':')).unwrap_or((raw, ""));
    let (k, v) = (k.trim(), v.trim());
    if !valid_key(k) {
        return None;
    }
    Some(if v.is_empty() {
        let vals = key_values_sql(k);
        (format!("{t}.rid IN (SELECT rid FROM ({vals}))"), None)
    } else if let Some(id) = v.strip_prefix('@') {
        (format!("{t}.rid IN ({})", key_rids_of_event_sql(k, param)), Some(id.to_string()))
    } else if let Some(prefix) = v.strip_suffix('*') {
        (format!("{t}.rid IN ({})", key_rids_sql(k, param, true)), Some(prefix.to_string()))
    } else {
        (format!("{t}.rid IN ({})", key_rids_sql(k, param, false)), Some(v.to_string()))
    })
}

/// Lo que comparte una clave con un evento ancla, con cifras exactas.
#[derive(Debug, Clone, PartialEq)]
pub struct Via {
    pub anchor: EventRef,
    pub key: String,
    /// Valores de la clave en el ancla.
    pub values: Vec<String>,
    /// Eventos relacionados (sin el ancla), en total.
    pub total: i64,
    /// `(source_id, eventos)`, de más a menos.
    pub by_source: Vec<(String, i64)>,
    /// `(kind, nivel, eventos)`, de más a menos.
    pub by_kind_level: Vec<(String, String, i64)>,
    /// Los primeros `limit` por tiempo, con `delta_ms` respecto al ancla.
    pub related: Vec<EventRef>,
}

/// `correlate <id> --via K`: los eventos de cualquier fuente que comparten
/// algún valor de `K` con `id`. `kinds` filtra como en `correlate_kinds`.
pub fn correlate_via(conn: &Connection, event_id: &str, key: &str, kinds: &[String], limit: usize) -> Result<Via> {
    check_key(key)?;
    let anchor = conn
        .query_row(
            "SELECT e.id, s.id, e.kind, e.at, e.at_epoch, e.level, e.title
             FROM events e JOIN sources s ON s.ref = e.source_ref WHERE e.id = ?1",
            params![event_id],
            |r| {
                Ok(EventRef {
                    id: r.get(0)?,
                    source_id: r.get(1)?,
                    kind: r.get(2)?,
                    at: r.get(3)?,
                    at_epoch: r.get(4)?,
                    level: r.get(5)?,
                    title: r.get(6)?,
                    delta_ms: 0,
                })
            },
        )
        .optional()?
        .ok_or_else(|| format!("correlate: event not found: {event_id}"))?;
    let vals = key_values_sql(key);
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT kv.value FROM ({vals}) kv WHERE kv.rid = (SELECT rid FROM events WHERE id = ?1) ORDER BY kv.value"
    ))?;
    let values: Vec<String> = stmt.query_map(params![event_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;

    let mut kind_sql = Vec::new();
    let mut bound: Vec<String> = vec![event_id.to_string()];
    for k in kinds.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
        let n = bound.len() + 1;
        match k.strip_suffix('*') {
            Some(prefix) => {
                kind_sql.push(format!("e.kind LIKE ?{n}"));
                bound.push(format!("{prefix}%"));
            }
            None => {
                kind_sql.push(format!("e.kind = ?{n}"));
                bound.push(k.to_string());
            }
        }
    }
    let kind_filter = if kind_sql.is_empty() { String::new() } else { format!("AND ({})", kind_sql.join(" OR ")) };
    // Los rids relacionados una vez (un evento con dos valores compartidos cuenta uno).
    let related_rids = format!(
        "SELECT DISTINCT r.rid FROM ({}) r WHERE r.rid <> (SELECT rid FROM events WHERE id = ?1)",
        key_rids_of_event_sql(key, "?1")
    );
    let base = format!("FROM events e JOIN sources s ON s.ref = e.source_ref WHERE e.rid IN ({related_rids}) {kind_filter}");
    let p = || rusqlite::params_from_iter(bound.iter());

    let mut by_source: Vec<(String, i64)> = conn
        .prepare(&format!("SELECT s.id, COUNT(*) {base} GROUP BY s.id"))?
        .query_map(p(), |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    by_source.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let total = by_source.iter().map(|(_, n)| n).sum();
    let mut by_kind_level: Vec<(String, String, i64)> = conn
        .prepare(&format!("SELECT e.kind, e.level, COUNT(*) {base} GROUP BY e.kind, e.level"))?
        .query_map(p(), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    by_kind_level.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));

    let n = bound.len() + 1;
    let at = anchor.at_epoch;
    let mut stmt = conn.prepare(&format!(
        "SELECT e.id, s.id, e.kind, e.at, e.at_epoch, e.level, e.title {base}
         ORDER BY e.at_epoch ASC, e.id ASC LIMIT ?{n}"
    ))?;
    let mut lb: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let lim = limit as i64;
    lb.push(&lim);
    let related: Vec<EventRef> = stmt
        .query_map(rusqlite::params_from_iter(lb), |r| {
            let at_epoch: i64 = r.get(4)?;
            Ok(EventRef {
                id: r.get(0)?,
                source_id: r.get(1)?,
                kind: r.get(2)?,
                at: r.get(3)?,
                at_epoch,
                level: r.get(5)?,
                title: r.get(6)?,
                delta_ms: if at_epoch > 0 && at > 0 { at_epoch - at } else { 0 },
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(Via { anchor, key: key.to_string(), values, total, by_source, by_kind_level, related })
}

/// Una unidad del lado `from` sin pareja en el lado `to`.
#[derive(Debug, Clone, PartialEq)]
pub struct GapUnit {
    pub unit: String,
    /// Eventos del lado `from` con esa unidad.
    pub events: i64,
    pub first_at: String,
    pub last_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gaps {
    /// Unidades distintas en el lado `from` (valores de `via`, o de `by`).
    pub units: i64,
    /// Las que tienen algún valor de `via` también en el lado `to`.
    pub covered: i64,
    /// `units - covered`: el hueco.
    pub missing: i64,
    /// Valores distintos de `via` en el lado `to`.
    pub to_values: i64,
    /// Las primeras `limit` unidades del hueco, de la más reciente a la más antigua.
    pub sample: Vec<GapUnit>,
}

/// `gaps --from A --to B --via K [--by U]`: las claves `K` del lado `from`
/// que ningún evento del lado `to` tiene («pruebas sin resultado»). Con `by`,
/// la unidad es el valor de `U` (p. ej. `sessionId`): una sesión cuenta como
/// cubierta si ALGUNA de sus claves `K` aparece en `to` («sesiones sin
/// commit»). Exacto: cuenta todas las unidades; `limit` solo acota la muestra.
pub fn gaps(conn: &Connection, from: &EventScope, to: &EventScope, via: &str, by: Option<&str>, since_epoch: i64, limit: usize) -> Result<Gaps> {
    check_key(via)?;
    if let Some(b) = by {
        check_key(b)?;
    }
    let (ff, fp) = from.sql(2);
    let (tf, tp) = to.sql(2 + fp.len());
    let vals = key_values_sql(via);
    let unit_join = match by {
        Some(b) => format!("JOIN ({}) u ON u.rid = e.rid", key_values_sql(b)),
        None => String::new(),
    };
    let unit = if by.is_some() { "u.value" } else { "kv.value" };
    // `f`: (unidad, valor, rid) del lado from; `t`: valores del lado to.
    let ctes = format!(
        "WITH f AS (SELECT {unit} AS unit, kv.value AS value, e.rid AS rid, e.at_epoch AS at_epoch
                    FROM events e JOIN ({vals}) kv ON kv.rid = e.rid {unit_join}
                    WHERE (?1 = 0 OR e.at_epoch >= ?1) {ff}),
              t AS (SELECT DISTINCT kv.value AS value
                    FROM events e JOIN ({vals}) kv ON kv.rid = e.rid
                    WHERE (?1 = 0 OR e.at_epoch >= ?1) {tf}),
              cov AS (SELECT DISTINCT f.unit FROM f JOIN t ON t.value = f.value)"
    );
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&since_epoch];
    for p in fp.iter().chain(&tp) {
        bound.push(p);
    }
    let (units, covered, to_values): (i64, i64, i64) = conn.query_row(
        &format!("{ctes} SELECT (SELECT COUNT(DISTINCT unit) FROM f), (SELECT COUNT(*) FROM cov), (SELECT COUNT(*) FROM t)"),
        rusqlite::params_from_iter(bound.iter()),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let lim = limit as i64;
    let n = bound.len() + 1;
    bound.push(&lim);
    // Sin tiempo (at_epoch 0) da '' en vez de 1970.
    let iso = |agg: &str| format!("CASE WHEN {agg}(f.at_epoch) > 0 THEN strftime('%Y-%m-%dT%H:%M:%SZ', {agg}(f.at_epoch) / 1000, 'unixepoch') ELSE '' END");
    let (iso_min, iso_max) = (iso("MIN"), iso("MAX"));
    let mut stmt = conn.prepare(&format!(
        "{ctes}
         SELECT f.unit, COUNT(DISTINCT f.rid), {iso_min}, {iso_max} FROM f
         WHERE f.unit NOT IN (SELECT unit FROM cov)
         GROUP BY f.unit ORDER BY MAX(f.at_epoch) DESC, f.unit ASC LIMIT ?{n}"
    ))?;
    let sample: Vec<GapUnit> = stmt
        .query_map(rusqlite::params_from_iter(bound.iter()), |r| {
            Ok(GapUnit { unit: r.get(0)?, events: r.get(1)?, first_at: r.get(2)?, last_at: r.get(3)? })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(Gaps { units, covered, missing: units - covered, to_values, sample })
}

/// Valores de una clave y en cuántas fuentes aparece cada uno: lo que lista
/// `sources`/`informe` como relaciones disponibles. `(clave, valores, eventos, fuentes)`.
pub fn relation_keys(conn: &Connection) -> Result<Vec<(String, i64, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT lt.rel, COUNT(DISTINCT lt.id), COUNT(*), COUNT(DISTINCT e.source_ref)
         FROM link_targets lt JOIN event_links el ON el.target_id = lt.id JOIN events e ON e.rid = el.event_rid
         GROUP BY lt.rel ORDER BY lt.rel",
    )?;
    let mut out: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))? {
        let (k, a, b, c) = row?;
        out.insert(k, (a, b, c));
    }
    Ok(out.into_iter().map(|(k, (a, b, c))| (k, a, b, c)).collect())
}
