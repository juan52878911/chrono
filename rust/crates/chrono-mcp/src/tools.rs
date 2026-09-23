//! Definición de los 11 tools MCP y su ejecución (`tools/call`).
//!
//! Convención de salida v2 (`entity`/`id`, no `path`/`sha`): mismos nombres de
//! campo que `chrono-cli/src/query.rs` para las consultas que comparten
//! (hotspots, coupling, owners, churn, search, similar); el resto sigue el
//! mismo criterio para las que no tiene el CLI.

use std::path::Path;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::since::since_to_epoch;
use crate::{Result, State};

const HOTSPOTS_LIMIT: usize = 25;
const COUPLING_LIMIT: usize = 25;
const CHURN_LIMIT: usize = 25;
const SEARCH_LIMIT: usize = 25;
const SIMILAR_LIMIT: usize = 15;
const BUGS_LIMIT: usize = 15;
const PRS_LIMIT: i64 = 30;
const COUPLING_MIN_SUPPORT: i64 = 2;
const SIMILAR_MAX_DIST: u32 = 4;

fn str_prop(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

/// Texto de la propiedad `since`, compartido por todos los tools que aceptan
/// ventana temporal.
fn since_prop() -> Value {
    str_prop("Ventana temporal ISO, p.ej. 2025-01-01 (opcional)")
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({"type": "object", "properties": properties});
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    json!({"name": name, "description": description, "inputSchema": schema})
}

/// Los 11 tools expuestos por el servidor (paridad con el Go + `branches`/`prs`).
pub fn tool_defs() -> Vec<Value> {
    vec![
        tool(
            "hotspots",
            "Ficheros que más cambian y más pesan.",
            json!({"since": since_prop()}),
            &[],
        ),
        tool(
            "coupling",
            "Qué ficheros cambian junto a uno dado.",
            json!({"file": str_prop("Ruta del fichero"), "since": since_prop()}),
            &["file"],
        ),
        tool(
            "owners",
            "Propiedad por autor y bus factor de una ruta.",
            json!({"path": str_prop("Prefijo de ruta"), "since": since_prop()}),
            &["path"],
        ),
        tool(
            "churn",
            "Líneas añadidas/borradas por fichero.",
            json!({"since": since_prop()}),
            &[],
        ),
        tool(
            "search",
            "Busca commits por significado (texto completo).",
            json!({"query": str_prop("Texto a buscar")}),
            &["query"],
        ),
        tool(
            "similar",
            "Commits casi-duplicados de un SHA (por SimHash).",
            json!({"sha": str_prop("SHA (o id) del commit")}),
            &["sha"],
        ),
        tool(
            "bugs",
            "Zonas donde se concentran los fixes (relacional).",
            json!({"since": since_prop()}),
            &[],
        ),
        tool(
            "tickets",
            "Commits y ficheros ligados a un ticket.",
            json!({"id": str_prop("Id del ticket, p.ej. PROJ-123 o #45")}),
            &["id"],
        ),
        tool(
            "phases",
            "Fases del proyecto (etiquetas/releases).",
            json!({}),
            &[],
        ),
        tool(
            "branches",
            "Estado de ramas vs base: ahead/behind, mergeada, stale, autores.",
            json!({"base": str_prop("Rama base (opcional; por defecto main/master)")}),
            &[],
        ),
        tool(
            "prs",
            "Pull requests del forge (estado, merge, si es bug).",
            json!({"since": since_prop()}),
            &[],
        ),
        tool(
            "timeline",
            "Conteo de eventos por bucket temporal (log-native, multi-fuente).",
            json!({
                "since": since_prop(),
                "until": str_prop("Fin de la ventana ISO (opcional)"),
                "bucket_secs": json!({"type": "integer", "description": "Tamaño de bucket en segundos (por defecto 3600)"}),
                "by": str_prop("Desglose: level | kind (opcional)"),
                "limit": json!({"type": "integer", "description": "Máximo de buckets (por defecto 500)"})
            }),
            &[],
        ),
        tool(
            "top",
            "Valores más frecuentes de una dimensión (level|kind|actor|entity|attr:<clave>).",
            json!({
                "dim": str_prop("Dimensión: level|kind|actor|entity|attr:<clave>"),
                "since": since_prop(),
                "limit": json!({"type": "integer", "description": "Máximo de valores (por defecto 25)"})
            }),
            &["dim"],
        ),
        tool(
            "correlate",
            "Eventos de OTRAS fuentes cercanos en el tiempo a un evento (±Δt).",
            json!({
                "id": str_prop("Id del evento ancla"),
                "delta_secs": json!({"type": "integer", "description": "Ventana ±Δ en segundos (por defecto 3600)"}),
                "limit": json!({"type": "integer", "description": "Máximo de correlacionados (por defecto 25)"})
            }),
            &["id"],
        ),
    ]
}

/// Texto devuelto por cualquier tool cuando aún no hay índice: nunca se
/// crashea por falta de `.chrono/index.db`, simplemente se pide `chrono init`.
fn no_index_text() -> String {
    "chrono: este repositorio aún no tiene índice. Ejecuta 'chrono init' en su raíz y reintenta."
        .to_string()
}

fn text_content(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn result_content(result: &Value) -> Result<Value> {
    Ok(text_content(serde_json::to_string_pretty(result)?))
}

/// Ejecuta `tools/call`: despacha por `params.name` sobre `params.arguments`.
pub fn call_tool(state: &State, params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call: falta 'name'")?;
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);

    let store = match state.store() {
        Some(s) => s,
        None => return Ok(text_content(no_index_text())),
    };
    let conn = store.conn();
    let since = args.get("since").and_then(Value::as_str).unwrap_or("");
    let since_epoch = since_to_epoch(since);

    let result = match name {
        "hotspots" => hotspots(conn, since_epoch)?,
        "coupling" => {
            let file = arg_str(args, "file")?;
            coupling(conn, file, since_epoch)?
        }
        "owners" => {
            let path = arg_str(args, "path")?;
            owners(conn, path, since_epoch)?
        }
        "churn" => churn(conn, since_epoch)?,
        "search" => {
            let query = arg_str(args, "query")?;
            search(conn, query)?
        }
        "similar" => {
            let sha = arg_str(args, "sha")?;
            similar(conn, sha)?
        }
        "bugs" => bugs(conn, since_epoch)?,
        "tickets" => {
            let id = arg_str(args, "id")?;
            tickets(conn, id)?
        }
        "phases" => phases(conn)?,
        "branches" => {
            let base = args.get("base").and_then(Value::as_str).unwrap_or("");
            branches(store, base)?
        }
        "prs" => prs(conn, since)?,
        "timeline" => {
            let until = args.get("until").and_then(Value::as_str).unwrap_or("");
            let bucket_secs = arg_i64(args, "bucket_secs", 3600).max(1);
            let by = args.get("by").and_then(Value::as_str).filter(|s| !s.is_empty());
            let limit = arg_i64(args, "limit", 500).max(1) as usize;
            timeline(conn, since_epoch, since_to_epoch(until), bucket_secs, by, limit)?
        }
        "top" => {
            let dim = arg_str(args, "dim")?;
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            top(conn, dim, since_epoch, limit)?
        }
        "correlate" => {
            let id = arg_str(args, "id")?;
            let delta_secs = arg_i64(args, "delta_secs", 3600).max(1);
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            correlate(conn, id, delta_secs, limit)?
        }
        other => return Err(format!("herramienta desconocida: {other}").into()),
    };
    result_content(&result)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("tools/call: falta el argumento requerido '{key}'").into())
}

/// Lee un entero de `args[key]`, o `default` si falta o no es entero.
fn arg_i64(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

fn hotspots(conn: &Connection, since_epoch: i64) -> Result<Value> {
    let rows = chrono_metrics::hotspots(conn, since_epoch, HOTSPOTS_LIMIT, false)?;
    let items: Vec<Value> = rows
        .iter()
        .map(
            |h| json!({"entity": h.entity, "changes": h.changes, "size": h.size, "score": h.score}),
        )
        .collect();
    Ok(json!({"hotspots": items}))
}

fn coupling(conn: &Connection, entity: &str, since_epoch: i64) -> Result<Value> {
    let (rows, _total) = chrono_metrics::coupling(conn, entity, COUPLING_MIN_SUPPORT, since_epoch, COUPLING_LIMIT, false)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|c| json!({"b": c.b, "support": c.support, "confidence": c.confidence}))
        .collect();
    Ok(json!({"for": entity, "coupled": items}))
}

fn owners(conn: &Connection, prefix: &str, since_epoch: i64) -> Result<Value> {
    let (rows, bus_factor) = chrono_metrics::owners(conn, prefix, since_epoch, false)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|o| json!({"name": o.name, "changes": o.commits, "share": o.share}))
        .collect();
    Ok(json!({"owners": items, "bus_factor": bus_factor}))
}

fn churn(conn: &Connection, since_epoch: i64) -> Result<Value> {
    let rows = chrono_metrics::churn(conn, since_epoch, CHURN_LIMIT, false)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| json!({"entity": r.entity, "added": r.added, "deleted": r.deleted}))
        .collect();
    Ok(json!({"churn": items}))
}

fn search(conn: &Connection, query: &str) -> Result<Value> {
    let rows = chrono_metrics::search(conn, query, SEARCH_LIMIT)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|h| json!({"id": h.id, "title": h.title, "at": h.at}))
        .collect();
    Ok(json!({"query": query, "results": items}))
}

fn similar(conn: &Connection, id: &str) -> Result<Value> {
    let rows = chrono_metrics::similar(conn, id, SIMILAR_MAX_DIST, SIMILAR_LIMIT)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|s| json!({"id": s.id, "title": s.title, "distance": s.distance}))
        .collect();
    Ok(json!({"of": id, "similar": items}))
}

fn bugs(conn: &Connection, since_epoch: i64) -> Result<Value> {
    let b = chrono_metrics::bugs(conn, since_epoch, BUGS_LIMIT)?;
    let categories: Vec<Value> = b
        .categories
        .iter()
        .map(|c| {
            json!({
                "category": c.category,
                "fixes": c.fixes,
                "top_files": c.top_files,
                "examples": c.examples,
            })
        })
        .collect();
    let hot_areas: Vec<Value> = b
        .hot_areas
        .iter()
        .map(|a| {
            json!({
                "dir": a.dir,
                "fixes": a.fixes,
                "recent_fixes": a.recent_fixes,
                "examples": a.examples,
            })
        })
        .collect();
    Ok(json!({"total_fixes": b.total_fixes, "categories": categories, "hot_areas": hot_areas}))
}

fn tickets(conn: &Connection, id: &str) -> Result<Value> {
    let t = chrono_metrics::ticket(conn, id)?;
    Ok(json!({"ticket": t.ticket, "commits": t.commits, "files": t.files, "prs": t.prs}))
}

fn phases(conn: &Connection) -> Result<Value> {
    let rows = chrono_metrics::phases(conn)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|p| json!({"name": p.name, "date": p.date}))
        .collect();
    Ok(json!({"phases": items}))
}

fn branches(store: &chrono_store::Store, base: &str) -> Result<Value> {
    // `meta.repo_path` (fuente primaria git de `init`); si no, la primera
    // fuente git de `sources` (git añadido con `chrono add`).
    let repo_path = match store.meta("repo_path")?.filter(|s| !s.is_empty()) {
        Some(rp) => rp,
        None => store
            .list_sources()?
            .into_iter()
            .find(|s| s.kind == "git")
            .map(|s| s.path)
            .ok_or("no git source in the index")?,
    };
    let (base, current, rows) = chrono_source_git::branches(Path::new(&repo_path), base)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|b| {
            let authors: Vec<Value> = b
                .authors
                .iter()
                .map(|a| json!({"name": a.name, "commits": a.commits}))
                .collect();
            json!({
                "name": b.name,
                "current": b.current,
                "tip": {
                    "sha": b.tip.sha,
                    "author": b.tip.author,
                    "date": b.tip.date,
                    "subject": b.tip.subject,
                },
                "age_days": b.age_days,
                "ahead": b.ahead,
                "behind": b.behind,
                "merged": b.merged,
                "stale": b.stale,
                "authors": authors,
                "bus_factor": b.bus_factor,
            })
        })
        .collect();
    Ok(json!({"base": base, "current": current, "branches": items}))
}

/// PRs del forge: lee la tabla `issues` directamente (no hay `chrono-metrics`
/// equivalente en el port Rust). Réplica de `PullRequests` en
/// `internal/mcp/mcp.go` / `internal/metrics/metrics.go`: comparación textual
/// de `closed_at` contra `since` ("0000" si no hay ventana, menor que
/// cualquier fecha ISO), `closed_at=''` siempre pasa (PRs aún abiertos).
fn prs(conn: &Connection, since: &str) -> Result<Value> {
    let since_arg = if since.is_empty() { "0000" } else { since };
    let mut stmt = conn.prepare(
        "SELECT number, title, state, merged, is_bug, labels FROM issues
         WHERE kind = 'pr' AND (closed_at >= ?1 OR closed_at = '')
         ORDER BY number DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_arg, PRS_LIMIT], |r| {
        Ok(json!({
            "number": r.get::<_, i64>(0)?,
            "title": r.get::<_, String>(1)?,
            "state": r.get::<_, String>(2)?,
            "merged": r.get::<_, i64>(3)? == 1,
            "is_bug": r.get::<_, i64>(4)? == 1,
            "labels": r.get::<_, String>(5)?,
        }))
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(json!({"pull_requests": items}))
}

fn timeline(
    conn: &Connection,
    since_epoch: i64,
    until_epoch: i64,
    bucket_secs: i64,
    by: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let rows = chrono_metrics::timeline(conn, since_epoch, until_epoch, bucket_secs, by, limit)?;
    let buckets: Vec<Value> = rows
        .iter()
        .map(|b| json!({"bucket_epoch": b.bucket_epoch, "key": b.key, "count": b.count}))
        .collect();
    Ok(json!({"bucket_secs": bucket_secs, "by": by, "buckets": buckets}))
}

fn top(conn: &Connection, dim: &str, since_epoch: i64, limit: usize) -> Result<Value> {
    let rows = chrono_metrics::top(conn, dim, since_epoch, limit)?;
    let items: Vec<Value> = rows.iter().map(|v| json!({"value": v.value, "count": v.count})).collect();
    Ok(json!({"dim": dim, "top": items}))
}

fn correlate(conn: &Connection, id: &str, delta_secs: i64, limit: usize) -> Result<Value> {
    let corr = chrono_metrics::correlate(conn, id, delta_secs, limit)?;
    let event_json = |e: &chrono_metrics::EventRef| {
        json!({
            "id": e.id, "source_id": e.source_id, "kind": e.kind, "at": e.at,
            "level": e.level, "title": e.title, "delta_secs": e.delta_secs,
        })
    };
    let related: Vec<Value> = corr.related.iter().map(event_json).collect();
    Ok(json!({"delta_secs": delta_secs, "anchor": event_json(&corr.anchor), "related": related}))
}
