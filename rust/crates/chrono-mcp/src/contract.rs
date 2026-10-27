//! Contrato de respuesta para agentes: lo que acompaña a CADA salida de las
//! consultas sobre eventos, para que el modelo sepa qué tiene delante sin
//! recontar ni suponer.
//!
//! - Texto tabular por defecto (`format=json` devuelve el JSON de siempre, con
//!   la misma cabecera en `meta`): 2–3 veces menos tokens que el JSON indentado.
//! - Cabecera de una línea: `total` de eventos que casan con los filtros en la
//!   ventana, `coverage` (desde/hasta qué fecha hay datos en las fuentes
//!   consultadas) con aviso si la ventana pedida se sale de ella, `truncated`,
//!   `index` (huella del índice) y `q` (la consulta canónica). `index`+`q`
//!   identifican la respuesta: mismo par, mismos bytes.

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::since::since_to_epoch;

/// Herramientas que consultan eventos y llevan cabecera, con su lista principal
/// y su tope por defecto.
pub fn main_list(tool: &str) -> Option<(&'static str, i64)> {
    Some(match tool {
        "top" => ("top", 25),
        "patterns" => ("patterns", 25),
        "events" => ("events", 25),
        "timeline" => ("buckets", 500),
        "stats" => ("groups", 25),
        "precede" => ("episodes", 50),
        "informe" => ("", 0),
        _ => return None,
    })
}

/// Huella corta del estado del índice: cambia si entra o sale cualquier evento
/// o se mueve el watermark de una fuente.
pub fn index_hash(conn: &Connection) -> String {
    let state: String = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM events) || ':' || (SELECT COALESCE(MAX(rowid), 0) FROM events) || ':' ||
                    (SELECT COALESCE(group_concat(id || '=' || COALESCE(watermark, ''), ','), '') FROM (SELECT id, watermark FROM sources ORDER BY id))",
            [],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let h = state.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{:08x}", h >> 32)
}

/// La consulta canónica: herramienta + argumentos ordenados, sin `format`.
pub fn canonical(tool: &str, args: &Value) -> String {
    let mut m: Vec<(String, String)> = args
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, v)| k.as_str() != "format" && !v.is_null() && v.as_str() != Some(""))
                .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                .collect()
        })
        .unwrap_or_default();
    m.sort();
    let parts: Vec<String> = m.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    if parts.is_empty() {
        tool.to_string()
    } else {
        format!("{tool} {}", parts.join(" "))
    }
}

/// `(total que casa, primera y última fecha de las fuentes consultadas)`.
pub fn coverage(conn: &Connection, args: &Value, since_epoch: i64) -> (i64, Option<(i64, String, i64, String)>) {
    let scope = crate::tools::scope_from(args);
    let total = chrono_metrics::count_events(conn, since_epoch, &scope).unwrap_or(0);
    // Cobertura: solo por fuente (lo que hay), sin los demás filtros (lo que se busca).
    let src = chrono_metrics::EventScope { source: scope.source, ..Default::default() };
    let (sf, sx) = src.sql_public(1);
    let cov_sql = format!(
        "SELECT MIN(e.at_epoch), MAX(e.at_epoch) FROM events e WHERE e.at_epoch > 0 {sf}"
    );
    let bound2: Vec<&dyn rusqlite::ToSql> = sx.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let cov = conn
        .query_row(&cov_sql, rusqlite::params_from_iter(bound2), |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)))
        .ok()
        .and_then(|(a, b)| Some((a?, b?)))
        .map(|(a, b)| (a, iso(a), b, iso(b)));
    (total, cov)
}

pub fn iso(ms: i64) -> String {
    // Mismo formato que el resto de chrono (UTC, milisegundos, Z).
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = chrono_core::time::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z", sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// Argumentos con los que se cuenta el `total`. En `stats`, `attr` es el
/// atributo numérico y no el filtro `clave:valor` (que no casaba nada y daba
/// `total=0` con grupos): cuenta los eventos que tienen ese atributo.
fn count_args(tool: &str, args: &Value) -> Value {
    if tool != "stats" {
        return args.clone();
    }
    // Y el filtro `where_attr`, si lo hay, junto a él.
    let mut a = crate::tools::stats_filters(args);
    if let Some(k) = args.get("attr").and_then(Value::as_str) {
        let attr = match a.get("attr").and_then(Value::as_str).filter(|w| !w.trim().is_empty()) {
            Some(w) => format!("{k}:*,{w}"),
            None => format!("{k}:*"),
        };
        a["attr"] = json!(attr);
    }
    a
}

/// Cabecera (texto y objeto `meta`) de una respuesta. `truncated` lo da la
/// herramienta: hay más de lo devuelto en su lista principal (antes era
/// «devolvió el tope», que con exactamente `limit` elementos mentía).
pub fn header(conn: &Connection, tool: &str, args: &Value, truncated: bool) -> (String, Value) {
    let since = args.get("since").and_then(Value::as_str).unwrap_or("");
    let until = args.get("until").and_then(Value::as_str).unwrap_or("");
    let since_epoch = since_to_epoch(since);
    let count_args = count_args(tool, args);
    let args_q = args;
    let args = &count_args;
    let (total, cov) = coverage(conn, args, since_epoch);
    let mut warn = Vec::new();
    if let Some((first, _, last, _)) = &cov {
        let tol = 60 * 60 * 1000; // una hora de holgura: los datos no empiezan en punto
        if since_epoch > 0 && since_epoch + tol < *first {
            warn.push(format!(
                "no hay datos indexados antes de {} (la ventana pedida empieza antes): o no hubo eventos o no se indexaron; no lo cuentes como cero sin comprobarlo",
                iso(*first)
            ));
        }
        let ue = since_to_epoch(until);
        if ue > 0 && ue > *last + tol {
            warn.push(format!(
                "no hay datos indexados después de {} (la ventana pedida acaba más tarde): o no hubo eventos o no se indexaron",
                iso(*last)
            ));
        }
    } else {
        warn.push("no hay datos en las fuentes consultadas".to_string());
    }
    // Con infer_level, la cifra mezcla dos cosas: se dicen por separado para que
    // nadie dé como «total por nivel» lo que incluye líneas sin nivel de la fuente.
    let mut total_s = total.to_string();
    if crate::tools::arg_bool(args, "infer_level") && args.get("level").is_some() {
        let mut plain = args.clone();
        if let Some(o) = plain.as_object_mut() {
            o.remove("infer_level");
        }
        let (labeled, _) = coverage(conn, &plain, since_epoch);
        total_s = format!("{total}({labeled}_con_nivel_de_la_fuente+{}_deducidos_del_texto)", total - labeled);
        warn.push(format!(
            "infer_level: {} de estas {total} líneas NO traen nivel en la fuente (se deduce del texto). Las cifras «por nivel» se dan con el nivel de la fuente ({labeled}); las deducidas, aparte",
            total - labeled
        ));
    }
    let idx = index_hash(conn);
    let q = canonical(tool, args_q);
    let cov_s = cov.as_ref().map(|(_, a, _, b)| format!("{a}..{b}")).unwrap_or_else(|| "-".into());
    let mut text = format!(
        "[chrono {tool}] total={total_s} coverage={cov_s} truncated={} index={idx} q=\"{q}\"",
        if truncated { "yes" } else { "no" }
    );
    for w in &warn {
        text.push_str(&format!("\n⚠ {w}"));
    }
    let meta = json!({"total": total, "coverage": cov_s, "truncated": truncated, "index": idx, "q": q, "warnings": warn});
    (text, meta)
}

/// Nivel de detalle de la respuesta (`depth`): `counts` deja solo las cifras
/// (cada lista se cambia por su longitud), `summary` quita las sub-tablas de
/// cada fila (los `preceding` de cada episodio de `precede`) y `full` lo deja
/// todo. Se aplica DESPUÉS de la cabecera, así que `total` y `truncated`
/// siguen hablando del resultado completo.
pub fn apply_depth(result: &mut Value, depth: &str) {
    let Some(o) = result.as_object_mut() else { return };
    match depth {
        "counts" => {
            for v in o.values_mut() {
                if let Some(a) = v.as_array() {
                    *v = json!(a.len());
                }
            }
        }
        "summary" => {
            for v in o.values_mut() {
                for row in v.as_array_mut().into_iter().flatten() {
                    if let Some(r) = row.as_object_mut() {
                        r.retain(|_, c| !c.is_array() && !c.is_object());
                    }
                }
            }
        }
        _ => {}
    }
}

/// `depth` por defecto: `summary` donde las filas llevan sub-tablas, `full` en el resto.
pub fn default_depth(tool: &str) -> &'static str {
    if tool == "precede" { "summary" } else { "full" }
}

/// Ajustes por herramienta sobre el resultado antes de pintarlo.
pub fn enrich(tool: &str, result: &mut Value, total: i64) {
    match tool {
        "top" => {
            if let Some(arr) = result.get_mut("top").and_then(Value::as_array_mut) {
                let shown: i64 = arr.iter().filter_map(|v| v.get("count").and_then(Value::as_i64)).sum();
                if total > shown && result.get("dim").and_then(Value::as_str) != Some("entity") {
                    if let Some(a) = result.get_mut("top").and_then(Value::as_array_mut) {
                        a.push(json!({"value": "(resto)", "count": total - shown}));
                    }
                }
            }
        }
        "timeline" => {
            if let Some(arr) = result.get_mut("buckets").and_then(Value::as_array_mut) {
                for b in arr.iter_mut() {
                    if let Some(e) = b.get("bucket_epoch").and_then(Value::as_i64) {
                        let o = b.as_object_mut().unwrap();
                        o.insert("at".into(), json!(iso(e)));
                        o.remove("bucket_epoch");
                    }
                }
                let mut peaks: Vec<&Value> = arr.iter().collect();
                peaks.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()).then(a["at"].as_str().cmp(&b["at"].as_str())));
                let peaks: Vec<Value> = peaks.into_iter().take(3).map(|p| json!({"at": p["at"], "key": p["key"], "count": p["count"]})).collect();
                if let Some(o) = result.as_object_mut() {
                    o.insert("peaks".into(), Value::Array(peaks));
                }
            }
        }
        _ => {}
    }
}

const ORDER: &[&str] = &[
    "value", "count", "level", "template", "start", "at", "key", "anchors", "origins", "before_episodes", "secs_before",
    "entity", "source", "alias", "kind", "events", "first", "last", "group", "min", "p50", "p95", "max", "sum", "title", "id",
    "example_id", "first_id",
];

/// Quita el prefijo de fecha/proceso de una plantilla (`<TS> <TS> UTC [<NUM>] `):
/// no distingue nada y es lo que más ocupa en las tablas.
pub(crate) fn short(s: &str) -> &str {
    let mut t = s;
    loop {
        let before = t;
        for p in ["<TS> ", "UTC ", "[<NUM>] "] {
            t = t.strip_prefix(p).unwrap_or(t);
        }
        if t == before {
            return t;
        }
    }
}

fn cell(v: &Value) -> String {
    match v {
        Value::String(s) => short(s).replace(['\t', '\n'], " "),
        Value::Null => "-".into(),
        Value::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => a.iter().map(cell).collect::<Vec<_>>().join(","),
        Value::Array(a) if a.iter().all(|x| x.get("value").is_some() && x.get("count").is_some()) => {
            a.iter().map(|x| format!("{}={}", cell(&x["value"]), cell(&x["count"]))).collect::<Vec<_>>().join(" ")
        }
        other => other.to_string(),
    }
}

/// Lista de objetos que no son pares `{value,count}`: va como subtabla.
fn is_nested(v: &Value) -> bool {
    v.as_array().is_some_and(|a| a.iter().any(|x| x.is_object() && !(x.get("value").is_some() && x.get("count").is_some())))
}

fn columns(rows: &[Value]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for r in rows {
        if let Some(o) = r.as_object() {
            for k in o.keys() {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
        }
    }
    keys.sort_by_key(|k| (ORDER.iter().position(|o| o == k).unwrap_or(ORDER.len()), k.clone()));
    keys
}

/// Pinta un resultado como texto: escalares `clave: valor`, listas de objetos
/// como tablas separadas por tabuladores, y listas anidadas como subtablas.
pub fn render(v: &Value) -> String {
    let mut out = String::new();
    render_into(v, "", &mut out);
    out.trim_end().to_string()
}

fn render_into(v: &Value, indent: &str, out: &mut String) {
    let Some(o) = v.as_object() else {
        out.push_str(&format!("{indent}{}\n", cell(v)));
        return;
    };
    // En el orden en que la herramienta construye el resultado (lo importante, primero).
    for (k, val) in o {
        match val {
            Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_object) => render_table(k, a, indent, out),
            Value::Array(a) if a.iter().any(|x| x.as_str().is_some_and(|s| s.len() > 40)) => {
                out.push_str(&format!("{indent}{k}:\n"));
                for x in a {
                    out.push_str(&format!("{indent}  - {}\n", cell(x)));
                }
            }
            Value::Object(m) => {
                out.push_str(&format!("{indent}{k}:\n"));
                render_into(&Value::Object(m.clone()), &format!("{indent}  "), out);
            }
            _ => out.push_str(&format!("{indent}{k}: {}\n", cell(val))),
        }
    }
}

fn render_table(k: &str, rows: &[Value], indent: &str, out: &mut String) {
    let cols = columns(rows);
    let (nested, flat): (Vec<String>, Vec<String>) = cols.into_iter().partition(|c| rows.iter().any(|r| r.get(c).is_some_and(is_nested)));
    out.push_str(&format!("{indent}{k} ({}):\n{indent}{}\n", rows.len(), flat.join("\t")));
    for r in rows {
        let line: Vec<String> = flat.iter().map(|c| r.get(c).map(cell).unwrap_or_else(|| "-".into())).collect();
        out.push_str(&format!("{indent}{}\n", line.join("\t")));
        for c in &nested {
            if let Some(a) = r.get(c).and_then(Value::as_array).filter(|a| !a.is_empty()) {
                render_table(c, a, &format!("{indent}    "), out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_recorta_sin_tocar_las_cifras() {
        let full = json!({"episodes_total": 2, "episodes": [{"start": "a", "preceding": [{"x": 1}]}, {"start": "b", "preceding": []}]});
        let mut s = full.clone();
        apply_depth(&mut s, "summary");
        assert_eq!(s, json!({"episodes_total": 2, "episodes": [{"start": "a"}, {"start": "b"}]}));
        let mut c = full.clone();
        apply_depth(&mut c, "counts");
        assert_eq!(c, json!({"episodes_total": 2, "episodes": 2}));
        let mut f = full.clone();
        apply_depth(&mut f, "full");
        assert_eq!(f, full);
    }

    #[test]
    fn consulta_canonica_ordenada_y_sin_format() {
        let a = json!({"level": "FATAL", "dim": "entity", "format": "json", "since": ""});
        assert_eq!(canonical("top", &a), "top dim=entity level=FATAL");
        assert_eq!(canonical("sources", &json!({})), "sources");
    }

    #[test]
    fn pinta_tablas_y_subtablas() {
        let v = json!({"episodes_total": 1, "episodes": [{"start": "t1", "anchors": 2, "preceding": [{"level": "ERROR", "count": 1, "template": "boom"}]}]});
        let t = render(&v);
        assert!(t.contains("episodes_total: 1"), "{t}");
        assert!(t.contains("start\tanchors"), "{t}");
        assert!(t.contains("    preceding (1):"), "{t}");
        assert!(t.contains("count\tlevel\ttemplate") || t.contains("level\tcount"), "{t}");
    }

    #[test]
    fn iso_utc() {
        assert_eq!(iso(1_792_015_200_000), "2026-10-14T22:00:00.000Z");
    }
}
