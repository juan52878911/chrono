//! Consultas (`hotspots`, `coupling`, `owners`, `churn`, `search`, `similar`,
//! `bugs`, `tickets`, `prs`, `phases`, `branches`) y el envelope JSON común de
//! `docs/OUTPUT-CONTRACT.md`, con la convención v2: `entity`/`id` en vez de
//! `path`/`sha`.

use std::collections::BTreeMap;
use std::path::Path;

use chrono_store::Store;

use crate::i18n::t;
use crate::json::Json;
use crate::timeutil::{epoch_to_iso8601, now_epoch, parse_duration, parse_since};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Ventana temporal ya resuelta: el texto original (para `window.since`) y su
/// epoch UTC (para las consultas). `since_epoch = 0` significa sin límite.
pub struct Window {
    pub since_text: Option<String>,
    pub since_epoch: i64,
}

impl Window {
    pub fn from_flag(since: Option<&str>) -> Result<Window> {
        match since {
            None | Some("") => Ok(Window { since_text: None, since_epoch: 0 }),
            Some(s) => match parse_since(s) {
                Some(e) => Ok(Window { since_text: Some(s.to_string()), since_epoch: e }),
                None => Err(t(
                    format!("invalid --since: {s:?} (use YYYY-MM-DD)"),
                    format!("--since no válido: {s:?} (usa YYYY-MM-DD)"),
                )
                .into()),
            },
        }
    }
}

/// Arma el envelope común leyendo el manifiesto del store.
fn envelope(
    store: &Store,
    question: &str,
    window: &Window,
    max_items: usize,
    returned: usize,
    result: Json,
) -> Result<Json> {
    let mut manifest = Json::obj();
    for k in ["git_version", "first_parent", "no_merges", "mailmap_used", "bulk_threshold"] {
        if let Some(v) = store.meta(k)? {
            manifest = manifest.set(k, Json::str(&v));
        }
    }
    let since = match &window.since_text {
        Some(s) => Json::str(s),
        None => Json::Null,
    };
    Ok(Json::obj()
        .set("schema_version", Json::Int(chrono_store::SCHEMA_VERSION))
        .set("question", Json::str(question))
        .set("generated_at", Json::str(&epoch_to_iso8601(now_epoch())))
        .set("window", Json::obj().set("since", since).set("until", Json::Null))
        .set("manifest", manifest)
        .set(
            "token_budget",
            Json::obj()
                .set("max_items", Json::Int(max_items as i64))
                .set("returned", Json::Int(returned as i64))
                .set("truncated", Json::Bool(returned >= max_items)),
        )
        .set("result", result))
}

pub fn hotspots(store: &Store, w: &Window, symbols_only: bool) -> Result<Json> {
    const LIMIT: usize = 25;
    let rows = chrono_metrics::hotspots(store.conn(), w.since_epoch, LIMIT, symbols_only)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|h| {
            Json::obj()
                .set("entity", Json::str(&h.entity))
                .set("changes", Json::Int(h.changes))
                .set("size", Json::Int(h.size))
                .set("score", Json::Float(h.score))
        })
        .collect();
    let n = items.len();
    envelope(store, "hotspots", w, LIMIT, n, Json::obj().set("hotspots", Json::Arr(items)))
}

pub fn coupling(store: &Store, w: &Window, entity: &str, symbols_only: bool) -> Result<Json> {
    const LIMIT: usize = 25;
    let (rows, _total) = chrono_metrics::coupling(store.conn(), entity, 2, w.since_epoch, LIMIT, symbols_only)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|c| {
            Json::obj()
                .set("b", Json::str(&c.b))
                .set("support", Json::Int(c.support))
                .set("confidence", Json::Float(c.confidence))
        })
        .collect();
    let n = items.len();
    envelope(
        store,
        "coupling",
        w,
        LIMIT,
        n,
        Json::obj().set("for", Json::str(entity)).set("coupled", Json::Arr(items)),
    )
}

pub fn owners(store: &Store, w: &Window, prefix: &str, symbols_only: bool) -> Result<Json> {
    let (rows, bus_factor) = chrono_metrics::owners(store.conn(), prefix, w.since_epoch, symbols_only)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|o| {
            Json::obj()
                .set("name", Json::str(&o.name))
                .set("changes", Json::Int(o.commits))
                .set("share", Json::Float(o.share))
        })
        .collect();
    let n = items.len();
    envelope(
        store,
        "owners",
        w,
        n,
        n,
        Json::obj().set("owners", Json::Arr(items)).set("bus_factor", Json::Int(bus_factor)),
    )
}

pub fn churn(store: &Store, w: &Window, symbols_only: bool) -> Result<Json> {
    const LIMIT: usize = 25;
    let rows = chrono_metrics::churn(store.conn(), w.since_epoch, LIMIT, symbols_only)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|r| {
            Json::obj()
                .set("entity", Json::str(&r.entity))
                .set("added", Json::Int(r.added))
                .set("deleted", Json::Int(r.deleted))
        })
        .collect();
    let n = items.len();
    envelope(store, "churn", w, LIMIT, n, Json::obj().set("churn", Json::Arr(items)))
}

pub fn search(store: &Store, query: &str) -> Result<Json> {
    const LIMIT: usize = 25;
    let w = Window { since_text: None, since_epoch: 0 };
    let rows = chrono_metrics::search(store.conn(), query, LIMIT)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|h| {
            Json::obj()
                .set("id", Json::str(&h.id))
                .set("title", Json::str(&h.title))
                .set("at", Json::str(&h.at))
        })
        .collect();
    let n = items.len();
    envelope(
        store,
        "search",
        &w,
        LIMIT,
        n,
        Json::obj().set("query", Json::str(query)).set("results", Json::Arr(items)),
    )
}

pub fn similar(store: &Store, id: &str) -> Result<Json> {
    const LIMIT: usize = 15;
    let w = Window { since_text: None, since_epoch: 0 };
    let rows = chrono_metrics::similar(store.conn(), id, 4, LIMIT)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|s| {
            Json::obj()
                .set("id", Json::str(&s.id))
                .set("title", Json::str(&s.title))
                .set("distance", Json::Int(s.distance as i64))
        })
        .collect();
    let n = items.len();
    envelope(
        store,
        "similar",
        &w,
        LIMIT,
        n,
        Json::obj().set("of", Json::str(id)).set("similar", Json::Arr(items)),
    )
}

/// Ventana vacía (sin `--since`): para consultas que no filtran por tiempo.
fn no_window() -> Window {
    Window { since_text: None, since_epoch: 0 }
}

pub fn bugs(store: &Store, w: &Window) -> Result<Json> {
    const LIMIT: usize = 15;
    let b = chrono_metrics::bugs(store.conn(), w.since_epoch, LIMIT)?;
    let categories: Vec<Json> = b
        .categories
        .iter()
        .map(|c| {
            Json::obj()
                .set("category", Json::str(&c.category))
                .set("fixes", Json::Int(c.fixes))
                .set(
                    "top_files",
                    Json::Arr(c.top_files.iter().map(|s| Json::str(s)).collect()),
                )
                .set("examples", Json::Arr(c.examples.iter().map(|s| Json::str(s)).collect()))
        })
        .collect();
    let hot_areas: Vec<Json> = b
        .hot_areas
        .iter()
        .map(|a| {
            Json::obj()
                .set("dir", Json::str(&a.dir))
                .set("fixes", Json::Int(a.fixes))
                .set("recent_fixes", Json::Int(a.recent_fixes))
                .set("examples", Json::Arr(a.examples.iter().map(|s| Json::str(s)).collect()))
        })
        .collect();
    let result = Json::obj()
        .set("total_fixes", Json::Int(b.total_fixes))
        .set("categories", Json::Arr(categories))
        .set("hot_areas", Json::Arr(hot_areas));
    // Paridad con el Go: max_items/returned fijos a LIMIT (no hay una sola
    // lista plana que truncar; el token_budget aquí es solo orientativo).
    envelope(store, "bugs", w, LIMIT, LIMIT, result)
}

pub fn ticket(store: &Store, id: &str) -> Result<Json> {
    let w = no_window();
    let t = chrono_metrics::ticket(store.conn(), id)?;
    let result = Json::obj()
        .set("ticket", Json::str(&t.ticket))
        .set("commits", Json::Arr(t.commits.iter().map(|s| Json::str(s)).collect()))
        .set("files", Json::Arr(t.files.iter().map(|s| Json::str(s)).collect()))
        .set("prs", Json::Arr(t.prs.iter().map(|n| Json::Int(*n)).collect()));
    envelope(store, "tickets", &w, 1, 1, result)
}

/// `prs`: PRs del forge (tabla `issues`), los más recientes primero.
pub fn prs(store: &Store) -> Result<Json> {
    const LIMIT: usize = 30;
    let w = no_window();
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT number, kind, title, state, labels, merged, is_bug
         FROM issues
         WHERE kind = 'pr'
         ORDER BY number DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([LIMIT as i64], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
        ))
    })?;
    let mut items = Vec::new();
    for row in rows {
        let (number, kind, title, state, labels, merged, is_bug) = row?;
        let labels_arr: Vec<Json> =
            labels.split(',').filter(|s| !s.is_empty()).map(Json::str).collect();
        items.push(
            Json::obj()
                .set("number", Json::Int(number))
                .set("kind", Json::str(&kind))
                .set("title", Json::str(&title))
                .set("state", Json::str(&state))
                .set("labels", Json::Arr(labels_arr))
                .set("merged", Json::Bool(merged != 0))
                .set("is_bug", Json::Bool(is_bug != 0)),
        );
    }
    let n = items.len();
    envelope(store, "prs", &w, LIMIT, n, Json::obj().set("pull_requests", Json::Arr(items)))
}

pub fn phases(store: &Store) -> Result<Json> {
    let w = no_window();
    let rows = chrono_metrics::phases(store.conn())?;
    let items: Vec<Json> = rows
        .iter()
        .map(|p| Json::obj().set("name", Json::str(&p.name)).set("date", Json::str(&p.date)))
        .collect();
    let n = items.len();
    envelope(store, "phases", &w, n, n, Json::obj().set("phases", Json::Arr(items)))
}

/// Ruta del repo git del índice, para `branches` (que lee git en vivo). Primero
/// `meta.repo_path` (lo escribe `init` cuando la fuente primaria es git); si no
/// está (p.ej. el git se añadió con `chrono add`), la primera fuente git de la
/// tabla `sources`. `None` si el índice no tiene ninguna fuente git.
pub fn git_repo_path(store: &Store) -> Result<Option<String>> {
    if let Some(rp) = store.meta("repo_path")?.filter(|s| !s.is_empty()) {
        return Ok(Some(rp));
    }
    Ok(store.list_sources()?.into_iter().find(|s| s.kind == "git").map(|s| s.path))
}

/// Parsea `--limit` a `usize` (>0), o cae al `default`. Error accionable si no
/// es un entero positivo.
fn parse_limit(limit: Option<&str>, default: usize) -> Result<usize> {
    match limit {
        None | Some("") => Ok(default),
        Some(s) => match s.parse::<usize>() {
            Ok(0) => Err(t("--limit must be > 0", "--limit debe ser > 0").into()),
            Ok(n) => Ok(n),
            Err(_) => Err(t(
                format!("invalid --limit: {s:?} (use a positive integer)"),
                format!("--limit no válido: {s:?} (usa un entero > 0)"),
            )
            .into()),
        },
    }
}

/// `bench`: mide la latencia de las consultas sobre el índice ACTUAL y reporta
/// estadísticas (nº de eventos/entidades/fuentes, tamaño del `.db`). No
/// reingiere: responde "¿es chrono rápido en MI índice?". Determinista en
/// forma (los tiempos varían, claro).
pub fn bench(store: &Store, db_path: &Path) -> Result<Json> {
    use std::time::Instant;
    let w = no_window();
    let conn = store.conn();

    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let events = count("SELECT COUNT(*) FROM events");
    let entities = count("SELECT COUNT(*) FROM entities");
    let sources = count("SELECT COUNT(*) FROM sources");
    let templates = count("SELECT COUNT(*) FROM templates");
    let db_bytes = std::fs::metadata(db_path).map(|m| m.len() as i64).unwrap_or(0);

    // Un id de evento cualquiera para `similar` (si hay eventos).
    let sample_id: Option<String> =
        conn.query_row("SELECT id FROM events LIMIT 1", [], |r| r.get(0)).ok();

    // (nombre, closure que ejecuta la consulta e ignora el resultado).
    let mut timings = Vec::new();
    let mut time_it = |name: &str, f: &mut dyn FnMut() -> bool| {
        let t = Instant::now();
        let ok = f();
        let us = t.elapsed().as_micros() as i64;
        timings.push(
            Json::obj()
                .set("query", Json::str(name))
                .set("micros", Json::Int(us))
                .set("ok", Json::Bool(ok)),
        );
    };

    time_it("hotspots", &mut || chrono_metrics::hotspots(conn, 0, 25, false).is_ok());
    time_it("churn", &mut || chrono_metrics::churn(conn, 0, 25, false).is_ok());
    time_it("bugs", &mut || chrono_metrics::bugs(conn, 0, 15).is_ok());
    time_it("timeline", &mut || chrono_metrics::timeline(conn, 0, 0, 3_600_000, None, 500).is_ok());
    time_it("top_kind", &mut || chrono_metrics::top(conn, "kind", 0, 25).is_ok());
    time_it("patterns", &mut || chrono_metrics::patterns(conn, 0, 25).is_ok());
    time_it("search", &mut || chrono_metrics::search(conn, "fix", 25).is_ok());
    if let Some(id) = &sample_id {
        time_it("similar", &mut || chrono_metrics::similar(conn, id, 4, 15).is_ok());
    }

    let stats = Json::obj()
        .set("events", Json::Int(events))
        .set("entities", Json::Int(entities))
        .set("sources", Json::Int(sources))
        .set("templates", Json::Int(templates))
        .set("db_bytes", Json::Int(db_bytes));
    let result = Json::obj().set("stats", stats).set("timings", Json::Arr(timings));
    envelope(store, "bench", &w, 1, 1, result)
}

/// `trace <id>`: sigue la cadena de `links` desde un evento (reverts, mismo
/// ticket/PR…). `--depth` (profundidad BFS, por defecto 5), `--limit` (50).
pub fn trace(store: &Store, id: &str, depth: Option<&str>, limit: Option<&str>) -> Result<Json> {
    const DEFAULT_DEPTH: i64 = 5;
    const DEFAULT_LIMIT: usize = 50;
    let w = no_window();
    let max_depth = match depth {
        None | Some("") => DEFAULT_DEPTH,
        Some(s) => match s.parse::<i64>() {
            Ok(n) if n > 0 => n,
            _ => return Err(t(format!("invalid --depth: {s:?} (positive integer)"), format!("--depth no válido: {s:?} (entero positivo)")).into()),
        },
    };
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let tr = chrono_metrics::trace(store.conn(), id, max_depth, limit)?;
    let event_json = |e: &chrono_metrics::EventRow| {
        Json::obj()
            .set("id", Json::str(&e.id))
            .set("source_id", Json::str(&e.source_id))
            .set("kind", Json::str(&e.kind))
            .set("at", Json::str(&e.at))
            .set("level", Json::str(&e.level))
            .set("title", Json::str(&e.title))
    };
    let nodes: Vec<Json> = tr
        .nodes
        .iter()
        .map(|n| {
            event_json(&n.event).set("depth", Json::Int(n.depth)).set("rel", Json::str(&n.rel))
        })
        .collect();
    let n = nodes.len();
    let result = Json::obj().set("anchor", event_json(&tr.anchor)).set("chain", Json::Arr(nodes));
    envelope(store, "trace", &w, limit, n, result)
}

/// `events`: stream cronológico unificado de todas las fuentes, filtrable por
/// `--source`/`--kind`/`--level`/`--grep` y la ventana `--since`/`--until`.
#[allow(clippy::too_many_arguments)]
pub fn events(
    store: &Store,
    w: &Window,
    until: Option<&str>,
    source: Option<&str>,
    kind: Option<&str>,
    level: Option<&str>,
    grep: Option<&str>,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 100;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let until_epoch = match until {
        None | Some("") => 0,
        Some(s) => parse_since(s).ok_or_else(|| {
            t(format!("invalid --until: {s:?} (use YYYY-MM-DD)"), format!("--until no válido: {s:?} (usa YYYY-MM-DD)"))
        })?,
    };
    let filter = chrono_metrics::EventFilter {
        since_epoch: w.since_epoch,
        until_epoch,
        source,
        kind,
        level,
        grep,
    };
    let rows = chrono_metrics::events(store.conn(), &filter, limit)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|e| {
            Json::obj()
                .set("id", Json::str(&e.id))
                .set("source_id", Json::str(&e.source_id))
                .set("kind", Json::str(&e.kind))
                .set("at", Json::str(&e.at))
                .set("level", Json::str(&e.level))
                .set("title", Json::str(&e.title))
        })
        .collect();
    let n = items.len();
    envelope(store, "events", w, limit, n, Json::obj().set("events", Json::Arr(items)))
}

/// `patterns`: plantillas de log más frecuentes (Drain-light, desde rollups).
pub fn patterns(
    store: &Store,
    w: &Window,
    source: Option<&str>,
    grep: Option<&str>,
    level: Option<&str>,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 25;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let rows = match level.filter(|l| !l.is_empty()) {
        // Con nivel no sirven los rollups (no lo guardan): agrupa sobre los eventos.
        Some(l) => chrono_metrics::patterns_by_level(store.conn(), w.since_epoch, source, grep, l, limit)?,
        None => chrono_metrics::patterns_filtered(store.conn(), w.since_epoch, source, grep, limit)?,
    };
    let items: Vec<Json> = rows
        .iter()
        .map(|p| {
            Json::obj()
                .set("template", Json::str(&p.template))
                .set("count", Json::Int(p.count))
                .set("example_id", Json::str(&p.example_id))
        })
        .collect();
    let n = items.len();
    envelope(store, "patterns", w, limit, n, Json::obj().set("patterns", Json::Arr(items)))
}

/// `show <id> [--entity ruta]`: enseña el diff de un commit (git en vivo, sin
/// indexar), acotado por un tope de bytes. `id` es un sha o cualquier revspec.
pub fn show(store: &Store, id: &str, entity: Option<&str>) -> Result<Json> {
    const MAX_BYTES: usize = 60_000;
    let w = no_window();
    let repo = match git_repo_path(store)? {
        Some(rp) => rp,
        None => {
            return Err(t(
                "no git source in the index; 'show' needs a git repo",
                "no hay fuente git en el índice; 'show' necesita un repo git",
            )
            .into())
        }
    };
    let s = chrono_source_git::show(Path::new(&repo), id, entity, MAX_BYTES)?;
    let result = Json::obj()
        .set("id", Json::str(&s.id))
        .set("entity", entity.map_or(Json::Null, Json::str))
        .set("truncated", Json::Bool(s.truncated))
        .set("total_bytes", Json::Int(s.total_bytes as i64))
        .set("diff", Json::str(&s.text));
    envelope(store, "show", &w, 1, 1, result)
}

/// `timeline`: conteo de eventos por bucket temporal. `--bucket` (duración, por
/// defecto 1h), `--until` (fecha), `--by level|kind` (desglose).
pub fn timeline(
    store: &Store,
    w: &Window,
    until: Option<&str>,
    bucket: Option<&str>,
    by: Option<&str>,
    scope: &chrono_metrics::EventScope,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_BUCKET: i64 = 3_600_000; // 1h en ms
    const DEFAULT_LIMIT: usize = 500;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let bucket_ms = match bucket {
        None | Some("") => DEFAULT_BUCKET,
        Some(s) => parse_duration(s).ok_or_else(|| {
            t(
                format!("invalid --bucket: {s:?} (e.g. 1h, 30m, 3600)"),
                format!("--bucket no válido: {s:?} (usa p.ej. 1h, 30m, 3600)"),
            )
        })?,
    };
    let until_epoch = match until {
        None | Some("") => 0,
        Some(s) => parse_since(s).ok_or_else(|| {
            t(
                format!("invalid --until: {s:?} (use YYYY-MM-DD)"),
                format!("--until no válido: {s:?} (usa YYYY-MM-DD)"),
            )
        })?,
    };
    let group_by = match by {
        None | Some("") => None,
        Some(b) => Some(b),
    };
    let rows = chrono_metrics::timeline_scoped(store.conn(), w.since_epoch, until_epoch, bucket_ms, group_by, scope, limit)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|b| {
            Json::obj()
                .set("bucket_epoch", Json::Int(b.bucket_epoch))
                .set("at", Json::str(&epoch_to_iso8601(b.bucket_epoch)))
                .set("key", Json::str(&b.key))
                .set("count", Json::Int(b.count))
        })
        .collect();
    let n = items.len();
    let until_json = if until_epoch == 0 { Json::Null } else { Json::str(&epoch_to_iso8601(until_epoch)) };
    let result = Json::obj()
        .set("bucket_ms", Json::Int(bucket_ms))
        .set("by", group_by.map_or(Json::Null, Json::str))
        .set("until", until_json)
        .set("buckets", Json::Arr(items));
    envelope(store, "timeline", w, limit, n, result)
}

/// `top <dim>`: valores más frecuentes de una dimensión.
pub fn top(
    store: &Store,
    w: &Window,
    dim: &str,
    source: Option<&str>,
    grep: Option<&str>,
    level: Option<&str>,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 25;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let filter = chrono_metrics::TopFilter { source, grep, level };
    let rows = chrono_metrics::top_filtered(store.conn(), dim, w.since_epoch, &filter, limit)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|v| Json::obj().set("value", Json::str(&v.value)).set("count", Json::Int(v.count)))
        .collect();
    let n = items.len();
    envelope(store, "top", w, limit, n, Json::obj().set("dim", Json::str(dim)).set("top", Json::Arr(items)))
}

/// Serializa un `EventRef` de `correlate` a JSON.
fn event_ref_json(e: &chrono_metrics::EventRef) -> Json {
    Json::obj()
        .set("id", Json::str(&e.id))
        .set("source_id", Json::str(&e.source_id))
        .set("kind", Json::str(&e.kind))
        .set("at", Json::str(&e.at))
        .set("level", Json::str(&e.level))
        .set("title", Json::str(&e.title))
        .set("delta_ms", Json::Int(e.delta_ms))
}

/// `correlate <id>`: eventos de OTRAS fuentes en ±Δt del evento ancla.
/// `--delta` (duración, por defecto 1h), `--limit`.
pub fn correlate(store: &Store, id: &str, delta: Option<&str>, kind: Option<&str>, limit: Option<&str>) -> Result<Json> {
    const DEFAULT_DELTA: i64 = 3_600_000; // 1h en ms
    const DEFAULT_LIMIT: usize = 25;
    let w = no_window();
    let delta_ms = match delta {
        None | Some("") => DEFAULT_DELTA,
        Some(s) => parse_duration(s).ok_or_else(|| {
            t(
                format!("invalid --delta: {s:?} (e.g. 1h, 30m, 3600)"),
                format!("--delta no válido: {s:?} (usa p.ej. 1h, 30m, 3600)"),
            )
        })?,
    };
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    // `--kind commit,ci.*`: solo esos tipos (ver `correlate_kinds`).
    let kinds: Vec<String> = kind.unwrap_or("").split(',').map(str::to_string).collect();
    let corr = chrono_metrics::correlate_kinds(store.conn(), id, delta_ms, &kinds, limit)?;
    let related: Vec<Json> = corr.related.iter().map(event_ref_json).collect();
    let n = related.len();
    let result = Json::obj()
        .set("delta_ms", Json::Int(delta_ms))
        .set("anchor", event_ref_json(&corr.anchor))
        .set("related", Json::Arr(related));
    envelope(store, "correlate", &w, limit, n, result)
}

/// `branches [base]`: lee git EN VIVO (no el índice), usando `repo_path` de
/// `meta` (escrito por `init`/`sync`).
pub fn branches(store: &Store, repo: &Path, base: &str) -> Result<Json> {
    let w = no_window();
    let (base, current, rows) = chrono_source_git::branches(repo, base)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|b| {
            let tip = Json::obj()
                .set("sha", Json::str(&b.tip.sha))
                .set("author", Json::str(&b.tip.author))
                .set("date", Json::str(&b.tip.date))
                .set("subject", Json::str(&b.tip.subject));
            let authors: Vec<Json> = b
                .authors
                .iter()
                .map(|a| Json::obj().set("name", Json::str(&a.name)).set("commits", Json::Int(a.commits)))
                .collect();
            Json::obj()
                .set("name", Json::str(&b.name))
                .set("current", Json::Bool(b.current))
                .set("tip", tip)
                .set("age_days", Json::Int(b.age_days))
                .set("ahead", Json::Int(b.ahead))
                .set("behind", Json::Int(b.behind))
                .set("merged", Json::Bool(b.merged))
                .set("stale", Json::Bool(b.stale))
                .set("authors", Json::Arr(authors))
                .set("bus_factor", Json::Int(b.bus_factor))
        })
        .collect();
    let n = items.len();
    envelope(
        store,
        "branches",
        &w,
        n,
        n,
        Json::obj().set("base", Json::str(&base)).set("current", Json::str(&current)).set(
            "branches",
            Json::Arr(items),
        ),
    )
}

/// Datos de git para `session_patterns`: horizonte observado (el
/// `last_sync_at` más reciente de las fuentes git, o ahora) y rama → mergeada
/// en la base (git en vivo; vacío si no hay repo o git falla).
fn session_git_context(store: &Store) -> Result<(i64, BTreeMap<String, bool>, String)> {
    let last_sync: Option<String> = store
        .conn()
        .query_row("SELECT MAX(last_sync_at) FROM sources WHERE kind = 'git'", [], |r| r.get(0))
        .unwrap_or(None);
    // `last_sync_at` puede llevar `.mmm`; `parse_since` entiende hasta el segundo.
    let horizon = last_sync.as_deref().and_then(|s| parse_since(s.split('.').next().unwrap_or(s))).unwrap_or_else(now_epoch);
    let mut merged = BTreeMap::new();
    let mut base = String::new();
    // Sin repo legible no hay datos de ramas (el merge sale solo de PRs).
    if let Some(repo) = git_repo_path(store).ok().flatten() {
        if let Ok((b, _current, rows)) = chrono_source_git::branches(Path::new(&repo), "") {
            base = b;
            for r in rows {
                merged.insert(r.name, r.merged);
            }
        }
    }
    Ok((horizon, merged, base))
}

fn opt_str(s: &Option<String>) -> Json {
    s.as_deref().map_or(Json::Null, Json::str)
}

fn strs(v: &[String], max: usize) -> Json {
    Json::Arr(v.iter().take(max).map(|s| Json::str(s)).collect())
}

/// Serializa un grupo de `session_patterns`. Listas internas acotadas a 5.
fn session_group_json(g: &chrono_metrics::Group, key_name: &str, related_name: &str) -> Json {
    const MAX: usize = 5;
    let t = &g.tokens;
    let tokens = Json::obj()
        .set("input", Json::Int(t.input))
        .set("output", Json::Int(t.output))
        .set("cache_read", Json::Int(t.cache_read))
        .set("cache_write", Json::Int(t.cache_write))
        .set("total", Json::Int(t.total()))
        .set("subagents", Json::Int(t.subagents));
    let time = Json::obj()
        .set("agent_ms", Json::Int(g.agent_ms))
        .set("waiting_human_ms", Json::Int(g.waiting_ms))
        .set("waiting_decisions", Json::Int(g.waiting_decisions))
        .set("waiting_upper_bounds", Json::Int(g.waiting_upper_bounds));
    let permissions = Json::obj()
        .set("asked", Json::Int(g.permissions_asked))
        .set("denied", Json::Int(g.denied))
        .set("denied_by_user", Json::Int(g.denied_by_user))
        .set(
            "denied_by_tool",
            Json::Arr(
                g.denied_by_tool
                    .iter()
                    .take(MAX)
                    .map(|d| Json::obj().set("tool", Json::str(&d.tool)).set("denied", Json::Int(d.count)))
                    .collect(),
            ),
        );
    let loops = Json::obj().set("count", Json::Int(g.loops.len() as i64)).set(
        "top",
        Json::Arr(
            g.loops
                .iter()
                .take(MAX)
                .map(|l| {
                    Json::obj()
                        .set("tool", Json::str(&l.tool))
                        .set("failures", Json::Int(l.failures))
                        .set("session", Json::str(&l.session))
                        .set("agent", if l.agent.is_empty() { Json::Null } else { Json::str(&l.agent) })
                })
                .collect(),
        ),
    );
    let c = &g.commits;
    let commits = Json::obj()
        .set("delivered", Json::Int(c.delivered.len() as i64))
        .set("resolved", Json::Int(c.resolved.len() as i64))
        .set("ids", strs(&c.resolved, MAX))
        .set("unresolved", strs(&c.unresolved, MAX))
        .set("touching_hotspots", Json::Int(c.touching_hotspots))
        .set("hotspots_touched", strs(&c.hotspots_touched, MAX));
    let rework = match &g.rework {
        None => Json::Null,
        Some(r) => Json::obj()
            .set("window_days", Json::Int(r.window_days))
            .set("files_delivered", Json::Int(r.files_delivered))
            .set("files_reworked", Json::Int(r.files_reworked))
            .set("file_ratio", Json::Float(r.file_ratio))
            .set("lines_delivered", Json::Int(r.lines_delivered))
            .set("lines_reworked", Json::Int(r.lines_reworked))
            .set("open_windows", Json::Int(r.open_windows))
            .set("reworked", strs(&r.reworked, MAX)),
    };
    let incomplete = match &g.incomplete {
        None => Json::Null,
        Some(i) => Json::obj()
            .set("edited_files", Json::Int(i.edited_files))
            .set("uncommitted", Json::Int(i.uncommitted.len() as i64))
            .set("examples", strs(&i.uncommitted, MAX))
            .set("outside_repo", Json::Int(i.outside_repo)),
    };
    let m = &g.merge;
    let merge = Json::obj()
        .set("status", Json::str(&m.status))
        .set("via", if m.via.is_empty() { Json::Null } else { Json::str(&m.via) })
        .set("branches", strs(&m.branches, MAX))
        .set(
            "prs",
            Json::Arr(
                m.prs
                    .iter()
                    .take(MAX)
                    .map(|(n, s, mg)| Json::obj().set("number", Json::Int(*n)).set("state", Json::str(s)).set("merged", Json::Bool(*mg)))
                    .collect(),
            ),
        )
        .set("reason", if m.reason.is_empty() { Json::Null } else { Json::str(&m.reason) });
    let at = |e: i64| if e == 0 { Json::Null } else { Json::str(&epoch_to_iso8601(e)) };
    let mut out = Json::obj().set(key_name, opt_str(&g.key));
    if key_name == "session" {
        out = out.set("remote_session", opt_str(&g.remote_session));
    }
    out.set(related_name, strs(&g.related, MAX))
        .set("profiles", strs(&g.profiles, MAX))
        .set("hosts", strs(&g.hosts, MAX))
        .set("first_at", at(g.first_epoch))
        .set("last_at", at(g.last_epoch))
        .set("tokens", tokens)
        .set("cost_usd", g.cost_usd.map_or(Json::Null, Json::Float))
        .set("time", time)
        .set("permissions", permissions)
        .set("interruptions", Json::Int(g.interruptions))
        .set("loops", loops)
        .set("commits", commits)
        .set("produced", strs(&g.produced, MAX))
        .set("rework", rework)
        .set("incomplete", incomplete)
        .set("merge", merge)
        .set("cost_per_commit", g.cost_per_commit.map_or(Json::Null, Json::Float))
        .set("tokens_per_commit", g.tokens_per_commit.map_or(Json::Null, Json::Int))
}

/// `session_patterns`: proceso (Nod) × resultado (git) por tarea y por sesión.
pub fn session_patterns(
    store: &Store,
    w: &Window,
    task: Option<&str>,
    session: Option<&str>,
    hotspots: Option<&str>,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 25;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let hotspot_limit = parse_limit(hotspots, 25)?;
    let (horizon, branch_merged, base) = session_git_context(store)?;
    let opts = chrono_metrics::SessionPatternsOpts {
        since_epoch: w.since_epoch,
        horizon_epoch: horizon,
        rework_days: 30,
        hotspot_limit,
        limit,
        task: task.map(str::to_string),
        session: session.map(str::to_string),
        branch_merged,
        base_branch: base.clone(),
    };
    let sp = chrono_metrics::session_patterns(store.conn(), &opts)?;
    let s = &sp.summary;
    let summary = Json::obj()
        .set("sessions", Json::Int(s.sessions))
        .set("tasks", Json::Int(s.tasks))
        .set("merge_rate", s.merge_rate.map_or(Json::Null, Json::Float))
        .set("tasks_merged", Json::Int(s.tasks_merged))
        .set("tasks_merge_known", Json::Int(s.tasks_merge_known))
        .set("totals", session_group_json(&s.all, "scope", "tasks"));
    let tasks: Vec<Json> = sp.tasks.iter().map(|g| session_group_json(g, "task", "sessions")).collect();
    let sessions: Vec<Json> = sp.sessions.iter().map(|g| session_group_json(g, "session", "tasks")).collect();
    let returned = tasks.len().max(sessions.len());
    let result = Json::obj()
        .set("summary", summary)
        .set(
            "basis",
            Json::obj()
                .set("events_scanned", Json::Int(sp.events_scanned))
                .set("hotspots", strs(&sp.hotspots, sp.hotspots.len()))
                .set("rework_window_days", Json::Int(30))
                .set("horizon", Json::str(&epoch_to_iso8601(horizon)))
                .set("base_branch", if base.is_empty() { Json::Null } else { Json::str(&base) }),
        )
        .set("tasks_total", Json::Int(sp.tasks_total as i64))
        .set("tasks", Json::Arr(tasks))
        .set("sessions_total", Json::Int(sp.sessions_total as i64))
        .set("sessions", Json::Arr(sessions))
        .set("notes", strs(&sp.notes, sp.notes.len()));
    envelope(store, "session_patterns", w, limit, returned, result)
}

/// `stats <attr>`: min/p50/p95/max/suma de un atributo numérico por grupo
/// (`--by level|kind|entity|attr:<k>`), con los filtros de siempre.
pub fn stats(
    store: &Store,
    w: &Window,
    attr: &str,
    by: Option<&str>,
    scope: &chrono_metrics::EventScope,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 25;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let rows = chrono_metrics::stats(store.conn(), attr, by, w.since_epoch, scope, limit)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|r| {
            Json::obj()
                .set("group", Json::str(&r.group))
                .set("count", Json::Int(r.count))
                .set("min", Json::Float(r.min))
                .set("p50", Json::Float(r.p50))
                .set("p95", Json::Float(r.p95))
                .set("max", Json::Float(r.max))
                .set("sum", Json::Float(r.sum))
        })
        .collect();
    let n = items.len();
    let result = Json::obj().set("attr", Json::str(attr)).set("by", by.map_or(Json::Null, Json::str)).set("groups", Json::Arr(items));
    envelope(store, "stats", w, limit, n, result)
}

/// `explain <fichero>[:desde-hasta]`: por qué un trozo de código está así.
///
/// `git blame` del rango en la revisión INDEXADA (no en el árbol de trabajo) +
/// lo que el índice sabe de esos commits: si eran arreglos (`yes` = prefijo
/// `fix:` o revert, `maybe` = solo una palabra clave), su categoría, sus
/// tickets/PRs, los autores del trozo y qué otras entidades cambiaron en esos
/// mismos commits (acoplamiento del trozo, sin commits masivos). Orden total y
/// sin texto generado: el mismo índice da siempre la misma respuesta.
pub fn explain(store: &Store, target: &str, limit: Option<&str>) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 15;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let w = no_window();
    let repo = git_repo_path(store)?.ok_or_else(|| {
        t("no git source in the index; 'explain' needs a git repo", "no hay fuente git en el índice; 'explain' necesita un repo git")
    })?;
    // Revisión indexada: el watermark de la fuente git (`sha:<head>`).
    let rev = store
        .list_sources()?
        .into_iter()
        .find(|s| s.kind == "git")
        .map(|s| s.watermark)
        .or(store.meta("last_watermark")?)
        .and_then(|w| w.strip_prefix("sha:").map(str::to_string))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| t("the index has no git revision; run 'chrono sync'", "el índice no tiene revisión de git; ejecuta 'chrono sync'"))?;
    let (file, range) = chrono_metrics::parse_target(target).map_err(|e| {
        t(e.clone(), format!("rango no válido en {target:?} (usa fichero:DESDE-HASTA)"))
    })?;
    let b = chrono_source_git::blame(Path::new(&repo), &rev, &file, range)
        .map_err(|e| t(format!("explain {file}: {e}"), format!("explain {file}: {e}")))?;

    let blamed: Vec<chrono_metrics::Blamed> = b
        .commits
        .iter()
        .map(|c| chrono_metrics::Blamed {
            sha: c.sha.clone(),
            author: c.author.clone(),
            author_time: c.author_time,
            summary: c.summary.clone(),
            lines: c.lines,
        })
        .collect();
    let x = chrono_metrics::explain(store.conn(), &file, &blamed, limit)?;
    let date = chrono_metrics::utc_date;
    let commits: Vec<Json> = x
        .commits
        .iter()
        .map(|ec| {
            let (c, f) = (&ec.blamed, &ec.facts);
            let tickets: Vec<String> = f
                .tickets
                .iter()
                .map(|tk| if tk.title.is_empty() { tk.id.clone() } else { format!("{} {} {}: {}", tk.id, tk.kind, tk.state, tk.title) })
                .collect();
            // Campos opcionales solo si tienen valor: una columna siempre vacía
            // son tokens que el agente paga por nada. Sigue siendo determinista
            // (depende solo de los datos).
            let mut row = Json::obj()
                .set("id", Json::str(&c.sha[..10.min(c.sha.len())]))
                .set("date", Json::str(&date(c.author_time)))
                .set("author", Json::str(&c.author))
                .set("lines", Json::Int(c.lines as i64))
                .set("fix", Json::str(f.fix.as_str()));
            if !f.category.is_empty() {
                row = row.set("category", Json::str(&f.category));
            }
            row = row.set("title", Json::str(&c.summary));
            if !tickets.is_empty() {
                row = row.set("tickets", Json::Arr(tickets.iter().map(|s| Json::str(s)).collect()));
            }
            if f.is_bulk {
                row = row.set("bulk", Json::Bool(true));
            }
            row
        })
        .collect();
    let total_commits = x.total_commits;
    let result = Json::obj()
        .set("file", Json::str(&file))
        .set("from", Json::Int(b.from as i64))
        .set("to", Json::Int(b.to as i64))
        .set("file_lines", Json::Int(b.total_lines as i64))
        .set("revision", Json::str(&rev[..12.min(rev.len())]))
        .set(
            "summary",
            Json::obj()
                .set("commits", Json::Int(x.total_commits as i64))
                .set("fixes_yes", Json::Int(x.fixes_yes))
                .set("fixes_maybe", Json::Int(x.fixes_maybe))
                .set("not_indexed", Json::Int(x.not_indexed))
                .set("oldest", x.oldest.map_or(Json::Null, |s| Json::str(&date(s))))
                .set("newest", x.newest.map_or(Json::Null, |s| Json::str(&date(s)))),
        )
        .set("commits", Json::Arr(commits))
        .set("authors", Json::Arr(x.authors.iter().map(|(a, n)| Json::obj().set("author", Json::str(a)).set("lines", Json::Int(*n))).collect()))
        .set(
            "changes_with",
            Json::Arr(
                x.co_changes
                    .iter()
                    .map(|c| {
                        let mut o = Json::obj().set("entity", Json::str(&c.entity)).set("commits", Json::Int(c.commits)).set("of", Json::Int(x.co_base));
                        if !c.symbols.is_empty() {
                            let s: Vec<Json> = c.symbols.iter().map(|(n, k)| Json::str(&format!("{n} ({k})"))).collect();
                            o = o.set("symbols", Json::Arr(s));
                        }
                        o
                    })
                    .collect(),
            ),
        );
    envelope(store, "explain", &w, limit, total_commits, result)
}
