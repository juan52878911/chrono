//! `stats <attr>`: estadística de un atributo NUMÉRICO de los eventos
//! (`duration_s` de un paso de CI, `latency_ms` de un log, `bytes`…), por grupo.
//!
//! Responde lo que `top` no puede: no «cuántas veces» sino «cuánto»: qué paso
//! del despliegue tarda más, si un endpoint se ha vuelto lento, el p95 de una
//! latencia. Los valores que no parsean como número se ignoran (y se cuentan
//! aparte), así un atributo heterogéneo no rompe la consulta.

use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::timeline::EventScope;
use crate::Result;

/// Un grupo y la distribución del atributo en él.
#[derive(Debug, Clone, PartialEq)]
pub struct StatRow {
    pub group: String,
    pub count: i64,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub sum: f64,
}

/// `attr`: clave en `attrs` (ruta `a.b` admitida). `by`: `level`, `kind`,
/// `entity`, `attr:<clave>` o `None` (un solo grupo, `""`). Orden: `sum` DESC
/// (lo que más pesa en total primero), luego `group`. Percentiles por rango
/// más cercano sobre los valores ordenados: deterministas y exactos.
pub fn stats(
    conn: &Connection,
    attr: &str,
    by: Option<&str>,
    since_epoch: i64,
    scope: &EventScope,
    limit: usize,
) -> Result<Vec<StatRow>> {
    if !valid_key(attr) {
        return Err(format!("stats: invalid attr: {attr:?}").into());
    }
    let (group_expr, join) = match by {
        None | Some("") => ("''".to_string(), ""),
        Some("level") => ("e.level".to_string(), ""),
        Some("kind") => ("e.kind".to_string(), ""),
        Some("entity") => (
            "COALESCE((SELECT en.key FROM touches t JOIN entities en ON en.id = t.entity_id WHERE t.event_rid = e.rid LIMIT 1), '')"
                .to_string(),
            "",
        ),
        Some(other) => match other.strip_prefix("attr:") {
            Some(k) if valid_key(k) => (format!("COALESCE({}, '')", crate::timeline::attr_sql("e", k, &format!("'$.{k}'"))), ""),
            _ => return Err(format!("stats: unsupported --by {other:?} (use level, kind, entity or attr:<key>)").into()),
        },
    };
    let (filters, extra) = scope.sql(3);
    let sql = format!(
        "SELECT g, v FROM (
             SELECT {group_expr} AS g, {value} AS v
             FROM events e {join}
             WHERE (?2 = 0 OR e.at_epoch >= ?2)
             {filters})
         WHERE v IS NOT NULL",
        value = crate::timeline::attr_sql("e", attr, "?1")
    );
    let path = format!("$.{attr}");
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&path, &since_epoch];
    for p in &extra {
        bound.push(p);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bound), |r| {
        let g: String = r.get::<_, Option<String>>(0)?.unwrap_or_default();
        let v: rusqlite::types::Value = r.get(1)?;
        Ok((g, v))
    })?;
    let mut groups: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for row in rows {
        let (g, v) = row?;
        let n = match v {
            rusqlite::types::Value::Integer(i) => Some(i as f64),
            rusqlite::types::Value::Real(f) => Some(f),
            rusqlite::types::Value::Text(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        };
        if let Some(n) = n.filter(|n| n.is_finite()) {
            groups.entry(g).or_default().push(n);
        }
    }
    let mut out: Vec<StatRow> = groups
        .into_iter()
        .map(|(group, mut vals)| {
            vals.sort_by(|a, b| a.partial_cmp(b).expect("finitos"));
            let pct = |p: f64| vals[((p * vals.len() as f64).ceil() as usize).clamp(1, vals.len()) - 1];
            StatRow {
                count: vals.len() as i64,
                min: vals[0],
                p50: pct(0.50),
                p95: pct(0.95),
                max: vals[vals.len() - 1],
                sum: vals.iter().sum(),
                group,
            }
        })
        .collect();
    out.sort_by(|a, b| b.sum.partial_cmp(&a.sum).expect("finitos").then_with(|| a.group.cmp(&b.group)));
    out.truncate(limit);
    Ok(out)
}

/// Clave de atributo segura para interpolar en `$.clave`: letras, dígitos, `_`, `-` y `.`.
fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono_core::{Actor, Event, Touch};
    use chrono_store::Store;

    use super::*;

    fn ev(id: &str, entity: &str, kind: &str, dur: &str) -> Event {
        Event {
            id: id.into(),
            source_id: "jsonl:/x".into(),
            kind: kind.into(),
            at: "2026-01-01T00:00:00Z".into(),
            at_epoch: 1_767_225_600_000,
            actor: Actor::default(),
            title: id.into(),
            attrs: BTreeMap::from([("duration_s".to_string(), dur.to_string())]),
            touches: vec![Touch { entity: entity.into(), entity_type: "log-source".into(), weight: 1, attrs: BTreeMap::new() }],
            ..Default::default()
        }
    }

    #[test]
    fn percentiles_por_entidad_y_filtro_por_kind() {
        let path = std::env::temp_dir().join(format!("chrono-stats-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path).unwrap();
        {
            let mut w = store.writer().unwrap();
            for (i, d) in ["10", "20", "30", "40", "300"].iter().enumerate() {
                w.add_event(&ev(&format!("b{i}"), "Build", "ci.step", d)).unwrap();
            }
            w.add_event(&ev("t0", "Tests", "ci.step", "5")).unwrap();
            w.add_event(&ev("t1", "Tests", "ci.step", "no-num")).unwrap();
            w.add_event(&ev("o0", "Build", "otro", "999")).unwrap();
            w.commit().unwrap();
        }
        let scope = EventScope { kind: Some("ci.*"), ..Default::default() };
        let rows = stats(store.conn(), "duration_s", Some("entity"), 0, &scope, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group, "Build");
        assert_eq!((rows[0].count, rows[0].min, rows[0].p50, rows[0].p95, rows[0].max), (5, 10.0, 30.0, 300.0, 300.0));
        assert_eq!((rows[1].group.as_str(), rows[1].count), ("Tests", 1), "el valor no numérico se ignora");
        assert!(stats(store.conn(), "x'); DROP", None, 0, &scope, 10).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
