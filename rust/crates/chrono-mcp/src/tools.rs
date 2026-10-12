//! Definición de los tools MCP y su ejecución (`tools/call`).
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
            "patterns",
            "Plantillas de log más frecuentes (clustering Drain-light).",
            json!({
                "since": since_prop(),
                "limit": json!({"type": "integer", "description": "Máximo de plantillas (por defecto 25)"})
            }),
            &[],
        ),
        tool(
            "trace",
            "Sigue la cadena de un evento por links (reverts, mismo ticket/PR, y el vcs.commit de una sesión de Nod ↔ su commit de git, con sha abreviado).",
            json!({
                "id": str_prop("Id del evento ancla"),
                "depth": json!({"type": "integer", "description": "Profundidad BFS (por defecto 5)"}),
                "limit": json!({"type": "integer", "description": "Máximo de nodos (por defecto 50)"})
            }),
            &["id"],
        ),
        tool(
            "session_patterns",
            "Proceso (sesiones de Nod) × resultado (git), por tarea y por sesión: tokens, costo, tiempo del agente frente a espera del humano, permisos negados, interrupciones, bucles, commits entregados y cuántos tocan hotspots, retrabajo a 30 días, cambios incompletos, merge y costo por commit.",
            json!({
                "since": since_prop(),
                "task": str_prop("Solo esta tarea (opcional)"),
                "session": str_prop("Solo esta sesión (opcional)"),
                "hotspots": json!({"type": "integer", "description": "Top N de hotspots que cuenta como hotspot (por defecto 25)"}),
                "limit": json!({"type": "integer", "description": "Máximo de tareas y de sesiones (por defecto 25)"})
            }),
            &[],
        ),
        tool(
            "events",
            "Stream cronológico unificado de todas las fuentes, filtrable.",
            json!({
                "since": since_prop(),
                "until": str_prop("Fin de la ventana ISO (opcional)"),
                "source": str_prop("Fuente: kind (git|jsonl|…) o source_id (opcional)"),
                "kind": str_prop("Tipo de evento (commit|log|release|…) (opcional)"),
                "level": str_prop("Nivel/severidad exacto (ERROR, INFO…) (opcional)"),
                "grep": str_prop("Substring en el título (opcional)"),
                "limit": json!({"type": "integer", "description": "Máximo de eventos (por defecto 100)"})
            }),
            &[],
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
            // El parámetro MCP es en SEGUNDOS (cómodo); `at_epoch` es ms → ×1000.
            let bucket_ms = arg_i64(args, "bucket_secs", 3600).max(1) * 1000;
            let by = args.get("by").and_then(Value::as_str).filter(|s| !s.is_empty());
            let limit = arg_i64(args, "limit", 500).max(1) as usize;
            timeline(conn, since_epoch, since_to_epoch(until), bucket_ms, by, limit)?
        }
        "top" => {
            let dim = arg_str(args, "dim")?;
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            top(conn, dim, since_epoch, limit)?
        }
        "correlate" => {
            let id = arg_str(args, "id")?;
            let delta_ms = arg_i64(args, "delta_secs", 3600).max(1) * 1000;
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            correlate(conn, id, delta_ms, limit)?
        }
        "patterns" => {
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            patterns(conn, since_epoch, limit)?
        }
        "trace" => {
            let id = arg_str(args, "id")?;
            let depth = arg_i64(args, "depth", 5).max(1);
            let limit = arg_i64(args, "limit", 50).max(1) as usize;
            trace(conn, id, depth, limit)?
        }
        "events" => {
            let until = args.get("until").and_then(Value::as_str).unwrap_or("");
            let limit = arg_i64(args, "limit", 100).max(1) as usize;
            let filter = chrono_metrics::EventFilter {
                since_epoch,
                until_epoch: since_to_epoch(until),
                source: args.get("source").and_then(Value::as_str).filter(|s| !s.is_empty()),
                kind: args.get("kind").and_then(Value::as_str).filter(|s| !s.is_empty()),
                level: args.get("level").and_then(Value::as_str).filter(|s| !s.is_empty()),
                grep: args.get("grep").and_then(Value::as_str).filter(|s| !s.is_empty()),
            };
            events(conn, &filter, limit)?
        }
        "session_patterns" => {
            let opts = chrono_metrics::SessionPatternsOpts {
                since_epoch,
                task: args.get("task").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                session: args.get("session").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                hotspot_limit: arg_i64(args, "hotspots", 25).max(1) as usize,
                limit: arg_i64(args, "limit", 25).max(1) as usize,
                ..Default::default()
            };
            session_patterns(store, opts)?
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
    bucket_ms: i64,
    by: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let rows = chrono_metrics::timeline(conn, since_epoch, until_epoch, bucket_ms, by, limit)?;
    let buckets: Vec<Value> = rows
        .iter()
        .map(|b| json!({"bucket_epoch": b.bucket_epoch, "key": b.key, "count": b.count}))
        .collect();
    Ok(json!({"bucket_ms": bucket_ms, "by": by, "buckets": buckets}))
}

fn top(conn: &Connection, dim: &str, since_epoch: i64, limit: usize) -> Result<Value> {
    let rows = chrono_metrics::top(conn, dim, since_epoch, limit)?;
    let items: Vec<Value> = rows.iter().map(|v| json!({"value": v.value, "count": v.count})).collect();
    Ok(json!({"dim": dim, "top": items}))
}

fn trace(conn: &Connection, id: &str, depth: i64, limit: usize) -> Result<Value> {
    let tr = chrono_metrics::trace(conn, id, depth, limit)?;
    let event_json = |e: &chrono_metrics::EventRow| {
        json!({"id": e.id, "source_id": e.source_id, "kind": e.kind, "at": e.at, "level": e.level, "title": e.title})
    };
    let chain: Vec<Value> = tr
        .nodes
        .iter()
        .map(|n| {
            let mut v = event_json(&n.event);
            v["depth"] = json!(n.depth);
            v["rel"] = json!(n.rel);
            v
        })
        .collect();
    Ok(json!({"anchor": event_json(&tr.anchor), "chain": chain}))
}

fn events(conn: &Connection, filter: &chrono_metrics::EventFilter, limit: usize) -> Result<Value> {
    let rows = chrono_metrics::events(conn, filter, limit)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|e| json!({
            "id": e.id, "source_id": e.source_id, "kind": e.kind,
            "at": e.at, "level": e.level, "title": e.title,
        }))
        .collect();
    Ok(json!({"events": items}))
}

fn patterns(conn: &Connection, since_epoch: i64, limit: usize) -> Result<Value> {
    let rows = chrono_metrics::patterns(conn, since_epoch, limit)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|p| json!({"template": p.template, "count": p.count, "example_id": p.example_id}))
        .collect();
    Ok(json!({"patterns": items}))
}

fn correlate(conn: &Connection, id: &str, delta_ms: i64, limit: usize) -> Result<Value> {
    let corr = chrono_metrics::correlate(conn, id, delta_ms, limit)?;
    let event_json = |e: &chrono_metrics::EventRef| {
        json!({
            "id": e.id, "source_id": e.source_id, "kind": e.kind, "at": e.at,
            "level": e.level, "title": e.title, "delta_ms": e.delta_ms,
        })
    };
    let related: Vec<Value> = corr.related.iter().map(event_json).collect();
    Ok(json!({"delta_ms": delta_ms, "anchor": event_json(&corr.anchor), "related": related}))
}

/// Horizonte en ms (último `last_sync_at` de las fuentes git, o ahora) y ramas
/// locales → mergeada (git en vivo). Mismo criterio que el CLI.
fn session_git_context(store: &chrono_store::Store) -> Result<(i64, std::collections::BTreeMap<String, bool>, String)> {
    let last_sync: Option<String> = store
        .conn()
        .query_row("SELECT MAX(last_sync_at) FROM sources WHERE kind = 'git'", [], |r| r.get(0))
        .unwrap_or(None);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // `last_sync_at` puede llevar `.mmm`: `since` entiende hasta el segundo.
    let horizon = last_sync
        .as_deref()
        .map(|s| since_to_epoch(s.split('.').next().unwrap_or(s)))
        .filter(|e| *e > 0)
        .unwrap_or(now_ms);
    let mut merged = std::collections::BTreeMap::new();
    let mut base = String::new();
    // Sin repo legible (sin fuente git, o fuentes aún sin ruta) no hay datos de
    // ramas: el merge sale solo de PRs y la consulta no falla.
    let repo = match store.meta("repo_path")?.filter(|s| !s.is_empty()) {
        Some(rp) => Some(rp),
        None => store.list_sources().ok().and_then(|v| v.into_iter().find(|s| s.kind == "git").map(|s| s.path)),
    };
    if let Some(repo) = repo {
        if let Ok((b, _cur, rows)) = chrono_source_git::branches(Path::new(&repo), "") {
            base = b;
            for r in rows {
                merged.insert(r.name, r.merged);
            }
        }
    }
    Ok((horizon, merged, base))
}

fn group_json(g: &chrono_metrics::Group, key_name: &str, related_name: &str) -> Value {
    const MAX: usize = 5;
    let take = |v: &[String]| v.iter().take(MAX).cloned().collect::<Vec<_>>();
    let t = &g.tokens;
    let c = &g.commits;
    let m = &g.merge;
    let mut out = json!({
        "profiles": take(&g.profiles),
        "hosts": take(&g.hosts),
        "tokens": {"input": t.input, "output": t.output, "cache_read": t.cache_read,
                   "cache_write": t.cache_write, "total": t.total(), "subagents": t.subagents},
        "cost_usd": g.cost_usd,
        "time": {"agent_ms": g.agent_ms, "waiting_human_ms": g.waiting_ms,
                 "waiting_decisions": g.waiting_decisions, "waiting_upper_bounds": g.waiting_upper_bounds},
        "permissions": {"asked": g.permissions_asked, "denied": g.denied, "denied_by_user": g.denied_by_user,
                        "denied_by_tool": g.denied_by_tool.iter().take(MAX)
                            .map(|d| json!({"tool": d.tool, "denied": d.count})).collect::<Vec<_>>()},
        "interruptions": g.interruptions,
        "loops": {"count": g.loops.len(), "top": g.loops.iter().take(MAX)
            .map(|l| json!({"tool": l.tool, "failures": l.failures, "session": l.session,
                            "agent": if l.agent.is_empty() { Value::Null } else { json!(l.agent) }}))
            .collect::<Vec<_>>()},
        "commits": {"delivered": c.delivered.len(), "resolved": c.resolved.len(), "ids": take(&c.resolved),
                    "unresolved": take(&c.unresolved), "touching_hotspots": c.touching_hotspots,
                    "hotspots_touched": take(&c.hotspots_touched)},
        "produced": take(&g.produced),
        "rework": g.rework.as_ref().map(|r| json!({
            "window_days": r.window_days, "files_delivered": r.files_delivered, "files_reworked": r.files_reworked,
            "file_ratio": r.file_ratio, "lines_delivered": r.lines_delivered, "lines_reworked": r.lines_reworked,
            "open_windows": r.open_windows, "reworked": take(&r.reworked)})),
        "incomplete": g.incomplete.as_ref().map(|i| json!({
            "edited_files": i.edited_files, "uncommitted": i.uncommitted.len(),
            "examples": take(&i.uncommitted), "outside_repo": i.outside_repo})),
        "merge": {"status": m.status, "via": if m.via.is_empty() { Value::Null } else { json!(m.via) },
                  "branches": take(&m.branches),
                  "prs": m.prs.iter().take(MAX).map(|(n, st, mg)| json!({"number": n, "state": st, "merged": mg})).collect::<Vec<_>>(),
                  "reason": if m.reason.is_empty() { Value::Null } else { json!(m.reason) }},
        "cost_per_commit": g.cost_per_commit,
        "tokens_per_commit": g.tokens_per_commit,
    });
    out[key_name] = json!(g.key);
    out[related_name] = json!(take(&g.related));
    if key_name == "session" {
        out["remote_session"] = json!(g.remote_session);
    }
    out
}

fn session_patterns(store: &chrono_store::Store, mut opts: chrono_metrics::SessionPatternsOpts) -> Result<Value> {
    let (horizon, merged, base) = session_git_context(store)?;
    opts.horizon_epoch = horizon;
    opts.branch_merged = merged;
    opts.base_branch = base.clone();
    let sp = chrono_metrics::session_patterns(store.conn(), &opts)?;
    let s = &sp.summary;
    Ok(json!({
        "summary": {"sessions": s.sessions, "tasks": s.tasks, "merge_rate": s.merge_rate,
                    "tasks_merged": s.tasks_merged, "tasks_merge_known": s.tasks_merge_known,
                    "totals": group_json(&s.all, "scope", "tasks")},
        "basis": {"events_scanned": sp.events_scanned, "hotspots": sp.hotspots,
                  "rework_window_days": opts.rework_days, "horizon_ms": horizon,
                  "base_branch": if base.is_empty() { Value::Null } else { json!(base) }},
        "tasks_total": sp.tasks_total,
        "tasks": sp.tasks.iter().map(|g| group_json(g, "task", "sessions")).collect::<Vec<_>>(),
        "sessions_total": sp.sessions_total,
        "sessions": sp.sessions.iter().map(|g| group_json(g, "session", "tasks")).collect::<Vec<_>>(),
        "notes": sp.notes,
    }))
}
