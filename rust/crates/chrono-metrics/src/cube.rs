//! `dim_rollups` como cubo: conteos por (plantilla, kind, nivel, entidad,
//! bucket) sin recorrer `events` (P-ESC-1). Lo usan `top level|kind|entity`,
//! `patterns` y `timeline` con filtros, y por tanto `informe`.
//!
//! Exacto o nada: [`cube`] devuelve `None` cuando no puede dar la misma cifra que
//! el camino de `events` (filtro de texto o de attr, rollups atrasados, una
//! entidad con varios touches en el alcance…) y el llamador cae a `events`. Una
//! ventana que parte un bucket no cae entera: los buckets completos salen del
//! cubo y los trozos de los extremos, de `events` (a lo sumo dos buckets de 10 min).

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::{EventScope, Result};

/// Una dimensión de agrupación.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Dim {
    /// Nivel de la fuente (`events.level`).
    Level,
    /// Nivel efectivo con `infer_level` (el de la fuente o el deducido).
    LevelInf,
    Kind,
    /// Entidad de cada touch (`entities.key`); los eventos sin touches no cuentan.
    Entity,
    /// Plantilla Drain-light del título.
    Template,
}

/// `(valores de las dims, bucket)` → `(cuenta, último rid)`. El bucket es 0 sin `bucket_ms`.
pub(crate) type Cells = BTreeMap<(Vec<String>, i64), (i64, i64)>;

/// Bucket de `dim_rollups` si están al día (watermark = MAX(rid)); si no, `None`.
pub(crate) fn fresh(conn: &Connection) -> Result<Option<i64>> {
    let meta = |k: &str| -> Result<Option<i64>> {
        let v: Option<String> = conn.query_row("SELECT value FROM meta WHERE key = ?1", params![k], |r| r.get(0)).optional()?;
        Ok(v.and_then(|s| s.parse().ok()))
    };
    let (Some(drb), Some(wm)) = (meta("dim_rollup_bucket_ms")?, meta("rollup_max_rowid")?) else { return Ok(None) };
    let max: i64 = conn.query_row("SELECT COALESCE(MAX(rid), 0) FROM events", [], |r| r.get(0))?;
    Ok((drb > 0 && wm == max).then_some(drb))
}

/// ¿Hay algún evento de otro `kind`? Por el cubo si está al día; si no, por `events`.
pub(crate) fn any_kind_other_than(conn: &Connection, kind: &str) -> Result<bool> {
    let sql = if fresh(conn)?.is_some() {
        "SELECT EXISTS (SELECT 1 FROM dim_rollups WHERE kind <> ?1)"
    } else {
        "SELECT EXISTS (SELECT 1 FROM events WHERE kind <> ?1)"
    };
    Ok(conn.query_row(sql, params![kind], |r| r.get(0))?)
}

/// Conteos de los eventos de `scope` desde `since` (0 = sin límite), agrupados
/// por `dims` y, con `bucket_ms`, por bucket (solo eventos con tiempo, como
/// `timeline`). Sin `bucket_ms` cuenta también los eventos sin tiempo cuando
/// `since` es 0, como `top`/`patterns`. `None`: el cubo no puede responder.
pub(crate) fn cube(conn: &Connection, since: i64, scope: &EventScope, dims: &[Dim], bucket_ms: Option<i64>) -> Result<Option<Cells>> {
    let nonempty = |v: Option<&str>| v.is_some_and(|s| !s.trim().is_empty());
    if nonempty(scope.grep) || nonempty(scope.attr) || nonempty(scope.join) || since < 0 {
        return Ok(None);
    }
    let Some(drb) = fresh(conn)? else { return Ok(None) };
    if bucket_ms.is_some_and(|b| b <= 0 || b % drb != 0) {
        return Ok(None);
    }
    let until = scope.until_epoch.max(0);
    // Buckets completos: [s_up, u_down). Los extremos que sobran, de `events`.
    let s_up = if since > 0 { since + (drb - since.rem_euclid(drb)) % drb } else { 0 };
    let u_down = if until > 0 { until - until.rem_euclid(drb) } else { i64::MAX };
    let timed = bucket_ms.is_some();
    let by_entity = dims.contains(&Dim::Entity);
    let level_col = if scope.infer_level { "r.level_inf" } else { "r.level" };

    // Filtros sobre `dim_rollups r`.
    let mut conds: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    let bind = |v: String, binds: &mut Vec<String>| {
        binds.push(v);
        format!("?{}", binds.len())
    };
    if let Some(s) = scope.source.filter(|s| !s.is_empty()) {
        let like = if s.contains(':') { format!("{s}%") } else { format!("{s}:%") };
        let (a, b) = (bind(like, &mut binds), bind(s.to_string(), &mut binds));
        conds.push(format!("r.source_ref IN (SELECT ref FROM sources WHERE id LIKE {a} OR json_extract(manifest_json, '$.alias') = {b})"));
    }
    if let Some(l) = scope.level.filter(|l| !l.trim().is_empty()) {
        let p = bind(crate::timeline::comma_list(l, true), &mut binds);
        conds.push(format!("instr({p}, ',' || UPPER({level_col}) || ',') > 0"));
    }
    if let Some(k) = scope.kind.filter(|k| !k.is_empty()) {
        match k.strip_suffix('*') {
            Some(prefix) => {
                let p = bind(format!("{prefix}%"), &mut binds);
                conds.push(format!("r.kind LIKE {p}"));
            }
            None => {
                let p = bind(k.to_string(), &mut binds);
                conds.push(format!("r.kind = {p}"));
            }
        }
    }
    // El filtro de entidad va aparte: la comprobación de multi-touch no lo lleva.
    let entity_cond = scope.entity.filter(|e| !e.trim().is_empty()).map(|en| {
        let p = bind(crate::timeline::comma_list(en, false), &mut binds);
        format!("r.entity_id IN (SELECT id FROM entities WHERE instr({p}, ',' || key || ',') > 0)")
    });
    if since > 0 {
        conds.push(format!("r.bucket_epoch >= {s_up}"));
    }
    if until > 0 {
        conds.push(format!("r.bucket_epoch < {u_down}"));
    }
    if timed {
        conds.push("r.bucket_epoch >= 0".to_string());
    }
    let base_where = if conds.is_empty() { "1".to_string() } else { conds.join(" AND ") };
    let where_ = match &entity_cond {
        Some(c) => format!("{base_where} AND {c}"),
        None => base_where.clone(),
    };

    // Un evento con varios touches (o un commit) no tiene entidad en el cubo:
    // si cae en el alcance de una consulta por entidad, solo `events` sabe contarlo.
    if by_entity || entity_cond.is_some() {
        let multi: bool = conn.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM dim_rollups r WHERE r.entity_id = -1 AND {base_where})"),
            // El parámetro de la entidad es el último: este chequeo no lo usa.
            rusqlite::params_from_iter(binds[..binds.len() - usize::from(entity_cond.is_some())].iter()),
            |r| r.get(0),
        )?;
        if multi {
            return Ok(None);
        }
    }

    let mut cells: Cells = BTreeMap::new();
    let mut add = |keys: Vec<String>, bucket: i64, n: i64, last: i64| {
        let c = cells.entry((keys, bucket)).or_insert((0, 0));
        c.0 += n;
        c.1 = c.1.max(last);
    };

    // 1. Buckets completos, del cubo.
    let exprs: Vec<&str> = dims
        .iter()
        .map(|d| match d {
            Dim::Level => "r.level",
            Dim::LevelInf => "r.level_inf",
            Dim::Kind => "r.kind",
            Dim::Entity => "en.key",
            Dim::Template => "tp.template",
        })
        .collect();
    let mut select: Vec<String> = exprs.iter().map(|e| e.to_string()).collect();
    let bucket_expr = match bucket_ms {
        Some(b) => format!("(r.bucket_epoch - r.bucket_epoch % {b})"),
        None => "0".to_string(),
    };
    select.push(bucket_expr.clone());
    let mut joins = String::new();
    if dims.contains(&Dim::Template) {
        joins.push_str(" JOIN templates tp ON tp.id = r.template_id");
    }
    if by_entity {
        joins.push_str(" JOIN entities en ON en.id = r.entity_id");
    }
    // Un literal en GROUP BY sería un índice de columna: se agrupa solo por expresiones.
    let group: Vec<&String> = select.iter().filter(|e| e.as_str() != "0").collect();
    let group_by = if group.is_empty() {
        String::new()
    } else {
        format!(" GROUP BY {}", group.iter().map(|e| e.as_str()).collect::<Vec<_>>().join(", "))
    };
    let sql = format!(
        "SELECT {}, SUM(r.count), MAX(r.last_rid) FROM dim_rollups r{joins} WHERE {where_}{group_by} HAVING COUNT(*) > 0",
        select.join(", ")
    );
    let n = dims.len();
    {
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(binds.iter()))?;
        while let Some(r) = rows.next()? {
            let keys: Vec<String> = (0..n).map(|i| r.get::<_, String>(i)).collect::<rusqlite::Result<_>>()?;
            add(keys, r.get(n)?, r.get(n + 1)?, r.get(n + 2)?);
        }
    }

    // 2. Los extremos de la ventana que no llenan un bucket, de `events`.
    let mut edges: Vec<(i64, i64)> = Vec::new();
    if since > 0 && since < s_up {
        edges.push((since, if until > 0 { until.min(s_up) } else { s_up }));
    }
    if until > 0 && until > u_down && u_down >= s_up {
        edges.push((if since > 0 { since.max(u_down) } else { u_down }, until));
    }
    for (lo, hi) in edges {
        let (filters, extra) = scope.sql_on("e", 3);
        let entity_join = if by_entity {
            " JOIN touches t ON t.event_rid = e.rid JOIN entities en ON en.id = t.entity_id"
        } else {
            ""
        };
        let entity_col = if by_entity { "en.key" } else { "''" };
        let sql = format!(
            "SELECT e.rid, e.kind, e.level, {}, e.title, e.at_epoch, {entity_col}
             FROM events e{entity_join}
             WHERE e.at_epoch > 0 AND e.at_epoch >= ?1 AND e.at_epoch < ?2 {filters}",
            crate::timeline::level_expr("e", true)
        );
        let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&lo, &hi];
        for p in &extra {
            bound.push(p);
        }
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(bound))?;
        while let Some(r) = rows.next()? {
            let rid: i64 = r.get(0)?;
            let at: i64 = r.get(5)?;
            let mut keys = Vec::with_capacity(n);
            for d in dims {
                keys.push(match d {
                    Dim::Kind => r.get(1)?,
                    Dim::Level => r.get(2)?,
                    Dim::LevelInf => r.get(3)?,
                    Dim::Template => chrono_core::templatize(&r.get::<_, String>(4)?),
                    Dim::Entity => r.get(6)?,
                });
            }
            let bucket = bucket_ms.map_or(0, |b| at - at.rem_euclid(b));
            add(keys, bucket, 1, rid);
        }
    }
    Ok(Some(cells))
}

/// Cuántos eventos CON tiempo de `scope` hay desde `since` (la cabecera `total`
/// del MCP): del cubo si puede, si no de `events`.
pub fn count_events(conn: &Connection, since: i64, scope: &EventScope) -> Result<i64> {
    if let Some(drb) = fresh(conn)? {
        if let Some(cells) = cube(conn, since, scope, &[], Some(drb))? {
            return Ok(cells.values().map(|(n, _)| n).sum());
        }
    }
    let (filters, extra) = scope.sql(2);
    let sql = format!("SELECT COUNT(*) FROM events e WHERE e.at_epoch > 0 AND (?1 = 0 OR e.at_epoch >= ?1) {filters}");
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&since];
    for p in &extra {
        bound.push(p);
    }
    Ok(conn.query_row(&sql, rusqlite::params_from_iter(bound), |r| r.get(0))?)
}

/// Id del evento de un rid (el `example_id` de `patterns`).
pub(crate) fn id_of(conn: &Connection, rid: i64) -> Result<String> {
    Ok(conn.query_row("SELECT id FROM events WHERE rid = ?1", params![rid], |r| r.get(0)).optional()?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono_core::{Actor, Event, Touch};
    use chrono_store::Store;

    use crate::{patterns_scoped, timeline_scoped, top_filtered, EventScope, TopFilter};

    fn ev(i: i64, source: &str, kind: &str, level: &str, at_epoch: i64, title: &str, entities: &[&str]) -> Event {
        Event {
            id: format!("{source}-{i}"),
            source_id: source.to_string(),
            kind: kind.to_string(),
            at_epoch,
            actor: Actor::default(),
            title: title.to_string(),
            level: level.to_string(),
            touches: entities
                .iter()
                .map(|e| Touch { entity: e.to_string(), entity_type: "log-source".to_string(), weight: 1, attrs: BTreeMap::new() })
                .collect(),
            ..Default::default()
        }
    }

    /// Todas las consultas que sirve el cubo, en texto, para comparar caminos.
    fn answers(conn: &rusqlite::Connection) -> Vec<String> {
        let windows = [(0, 0), (600_000, 0), (123_457, 0), (0, 1_800_000), (123_457, 1_234_567), (600_000, 1_200_000), (700_000, 800_000), (0, 650_000)];
        let scopes: Vec<EventScope> = vec![
            EventScope::default(),
            EventScope { level: Some("error,FATAL"), ..Default::default() },
            EventScope { level: Some("FATAL"), infer_level: true, ..Default::default() },
            EventScope { kind: Some("tool_*"), ..Default::default() },
            EventScope { source: Some("jsonl:/b"), ..Default::default() },
            EventScope { entity: Some("svc-1,svc-3"), level: Some("ERROR"), ..Default::default() },
        ];
        let mut out = vec![format!("sources {:?}", crate::sources(conn).unwrap())];
        for (since, until) in windows {
            for sc in &scopes {
                let sc = EventScope { until_epoch: until, ..sc.clone() };
                for dim in ["level", "kind", "entity"] {
                    let f = TopFilter {
                        source: sc.source,
                        level: sc.level,
                        kind: sc.kind,
                        entity: sc.entity,
                        infer_level: sc.infer_level,
                        until_epoch: until,
                        ..Default::default()
                    };
                    out.push(format!("top {dim} {since} {sc:?}: {:?}", top_filtered(conn, dim, since, &f, 50).unwrap()));
                }
                if !sc.only_source_grep() {
                    out.push(format!("patterns {since} {sc:?}: {:?}", patterns_scoped(conn, since, &sc, 50).unwrap()));
                }
                out.push(format!("count {since} {sc:?}: {}", super::count_events(conn, since, &sc).unwrap()));
                for (bucket, by) in [(600_000, None), (1_200_000, Some("level")), (600_000, Some("kind"))] {
                    let t = timeline_scoped(conn, since, until, bucket, by, &sc, 0).unwrap();
                    out.push(format!("timeline {bucket} {by:?} {since} {sc:?}: {t:?}"));
                }
            }
        }
        out
    }

    #[test]
    fn el_cubo_da_lo_mismo_que_events_con_ventanas_alineadas_o_no() {
        let path = std::env::temp_dir().join(format!("chrono-cube-{}.db", std::process::id()));
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", path.display()));
        }
        let mut store = Store::open(&path).unwrap();
        {
            let mut w = store.writer().unwrap();
            let levels = ["ERROR", "error", "", "INFO", "FATAL", "WARN"];
            for i in 0..400i64 {
                let source = if i % 3 == 0 { "jsonl:/b" } else { "jsonl:/a" };
                let kind = if i % 4 == 0 { "tool_use" } else { "log" };
                let level = levels[(i % 6) as usize];
                // Sin tiempo, uno de cada 37; el resto repartido en ~2,5 buckets de 10 min.
                let at = match i {
                    _ if i % 37 == 0 => 0,
                    11 => -700_000,
                    _ => 1 + i * 4_321,
                };
                let title = if level.is_empty() && i % 2 == 0 { format!("FATAL: disk {i} full") } else { format!("request {i} failed with {}", i % 5) };
                let svc = format!("svc-{}", i % 4);
                let ents: Vec<&str> = match i % 50 {
                    7 => vec![],
                    _ => vec![svc.as_str()],
                };
                w.add_event(&ev(i, source, kind, level, at, &title, &ents)).unwrap();
            }
            w.commit().unwrap();
        }
        store.build_rollups(60_000).unwrap();
        let conn = store.conn();
        assert!(super::fresh(conn).unwrap().is_some(), "el cubo tiene que estar al día");
        // El cubo responde de verdad (si devolviera siempre None, se compararía events con events).
        for (since, until) in [(0, 0), (123_457, 1_234_567)] {
            let sc = EventScope { level: Some("ERROR"), entity: Some("svc-1"), until_epoch: until, ..Default::default() };
            assert!(super::cube(conn, since, &sc, &[super::Dim::Template, super::Dim::Entity], Some(600_000)).unwrap().is_some());
        }
        let con_cubo = answers(conn);
        conn.execute("DELETE FROM meta WHERE key = 'dim_rollup_bucket_ms'", []).unwrap();
        assert!(super::fresh(conn).unwrap().is_none());
        let con_events = answers(conn);
        assert_eq!(con_cubo.len(), con_events.len());
        for (a, b) in con_cubo.iter().zip(&con_events) {
            assert_eq!(a, b);
        }

        // Un evento con dos touches: las consultas por entidad caen a events y dan lo mismo.
        {
            let mut w = store.writer().unwrap();
            w.add_event(&ev(999, "jsonl:/a", "log", "ERROR", 50_000, "request 999 failed with 4", &["svc-1", "svc-2"])).unwrap();
            w.commit().unwrap();
        }
        store.build_rollups(60_000).unwrap();
        let conn = store.conn();
        let con_cubo = answers(conn);
        conn.execute("DELETE FROM meta WHERE key = 'dim_rollup_bucket_ms'", []).unwrap();
        let con_events = answers(conn);
        assert_eq!(con_cubo.len(), con_events.len());
        for (a, b) in con_cubo.iter().zip(&con_events) {
            assert_eq!(a, b);
        }
        drop(store);
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", path.display()));
        }
    }
}
