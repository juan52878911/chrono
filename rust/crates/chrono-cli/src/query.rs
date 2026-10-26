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

/// Arma el envelope común leyendo el manifiesto del store. `truncated` si se
/// devolvió el tope entero (puede haber más).
fn envelope(
    store: &Store,
    question: &str,
    window: &Window,
    max_items: usize,
    returned: usize,
    result: Json,
) -> Result<Json> {
    envelope_with(store, question, window, max_items, returned, returned >= max_items, result)
}

/// Envelope de una respuesta que va siempre entera (un inventario, una ficha):
/// `max_items = returned` y nunca `truncated` (antes `sources` daba n+1 para
/// esquivarlo y `phases`/`tickets`/`show` salían como truncadas).
fn envelope_complete(store: &Store, question: &str, window: &Window, returned: usize, result: Json) -> Result<Json> {
    envelope_with(store, question, window, returned, returned, false, result)
}

fn envelope_with(
    store: &Store,
    question: &str,
    window: &Window,
    max_items: usize,
    returned: usize,
    truncated: bool,
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
                .set("truncated", Json::Bool(truncated)),
        )
        .set("result", result))
}

/// Pone `hints` (avisos para leer el resultado: una attr que no existe, un
/// índice sin git…) al principio del resultado, si hay: en `--compact` es lo
/// primero que se lee.
fn with_hints(result: Json, hints: Vec<String>) -> Json {
    match result {
        Json::Obj(mut fields) if !hints.is_empty() => {
            fields.insert(0, ("hints".to_string(), Json::Arr(hints.iter().map(|h| Json::str(h)).collect())));
            Json::Obj(fields)
        }
        other => other,
    }
}

/// Aviso de una attr que no tiene ningún evento, con las que hay.
fn unknown_attr_hint(store: &Store, key: &str) -> Result<String> {
    let keys = chrono_metrics::attr_keys(store.conn(), 20)?.join(", ");
    Ok(t(format!("no event has the attribute {key:?}; attributes: {keys}"), format!("ningún evento tiene la attr {key:?}; las que hay: {keys}")))
}

pub fn hotspots(store: &Store, w: &Window, symbols_only: bool) -> Result<Json> {
    const LIMIT: usize = 25;
    // Sin git no hay tamaño: el ranking salía con los nombres de tools o
    // servicios por frecuencia (size 0), que no es lo que mide hotspots.
    if chrono_metrics::sources_of_kind(store.conn(), "git")? == 0 {
        let hint = t(
            "hotspots needs a git source (changes × size of each file) and this index has none; for logs or tools: chrono top entity",
            "hotspots necesita una fuente git (cambios × tamaño de cada fichero) y este índice no tiene ninguna; para logs o tools: chrono top entity",
        );
        return envelope(store, "hotspots", w, LIMIT, 0, with_hints(Json::obj().set("hotspots", Json::Arr(vec![])), vec![hint]));
    }
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

pub fn search(store: &Store, query: &str, limit: Option<&str>) -> Result<Json> {
    // `--limit` se ignoraba: siempre 25, aunque la ayuda de las demás consultas lo admite.
    let limit = parse_limit(limit, 25)?;
    let w = Window { since_text: None, since_epoch: 0 };
    let rows = chrono_metrics::search(store.conn(), query, limit)?;
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
        limit,
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
    envelope_complete(store, "tickets", &w, 1, result)
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
    envelope_complete(store, "phases", &w, n, Json::obj().set("phases", Json::Arr(items)))
}

/// `sources`: qué hay indexado (por fuente: eventos, ventana, niveles, entidades).
pub fn sources(store: &Store) -> Result<Json> {
    let w = no_window();
    let rows = chrono_metrics::sources(store.conn())?;
    let pairs = |v: &[(String, i64)]| v.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(" ");
    let items: Vec<Json> = rows
        .iter()
        .map(|s| {
            let o = Json::obj()
                .set("source", Json::str(s.alias.as_deref().unwrap_or(&s.source)))
                .set("kind", Json::str(&s.kind))
                .set("events", Json::Int(s.events))
                .set("first", s.first.as_deref().map_or(Json::Null, Json::str))
                .set("last", s.last.as_deref().map_or(Json::Null, Json::str))
                .set("levels", Json::str(&pairs(&s.levels)))
                .set("unlabeled_inferred", Json::str(&pairs(&s.inferred_levels)))
                .set("top_entities", Json::str(&pairs(&s.top_entities)))
                .set("dims", Json::str(&s.dims.join(" ")));
            // Solo si los hay: eventos con las dims u opciones de antes de un cambio sin reindexar.
            let o = if s.stale_dims_events > 0 { o.set("stale_dims_events", Json::Int(s.stale_dims_events)) } else { o };
            // Solo si el último sync no pudo leerla.
            match &s.sync_error {
                Some(e) => o.set("sync_error", Json::str(e)),
                None => o,
            }
        })
        .collect();
    let n = items.len();
    envelope_complete(store, "sources", &w, n, Json::obj().set("sources", Json::Arr(items)))
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
    envelope_complete(store, "bench", &w, 1, result)
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

/// `--until` → epoch en ms (0 = sin fin).
fn until_epoch(until: Option<&str>) -> Result<i64> {
    match until {
        None | Some("") => Ok(0),
        Some(s) => Ok(parse_since(s).ok_or_else(|| {
            t(format!("invalid --until: {s:?} (use YYYY-MM-DD)"), format!("--until no válido: {s:?} (usa YYYY-MM-DD)"))
        })?),
    }
}

/// `events`: stream cronológico unificado de todas las fuentes, filtrable por
/// `--source`/`--kind`/`--level`/`--grep` y la ventana `--since`/`--until`.
/// `--order desc`: los más recientes primero.
pub fn events(
    store: &Store,
    w: &Window,
    until: Option<&str>,
    scope: &chrono_metrics::EventScope,
    order: Option<&str>,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 100;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let newest_first = match order.map(str::trim).unwrap_or("") {
        "" | "asc" => false,
        "desc" => true,
        o => return Err(t(format!("invalid --order: {o:?} (use asc or desc)"), format!("--order no válido: {o:?} (usa asc o desc)")).into()),
    };
    let filter = chrono_metrics::EventFilter {
        since_epoch: w.since_epoch,
        until_epoch: until_epoch(until)?,
        source: scope.source,
        kind: scope.kind,
        level: scope.level,
        grep: scope.grep,
        entity: scope.entity,
        attr: scope.attr,
        join: scope.join,
        infer_level: scope.infer_level,
    };
    let rows = chrono_metrics::events_ordered(store.conn(), &filter, limit, newest_first)?;
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
    let result = Json::obj().set("order", Json::str(if newest_first { "desc" } else { "asc" })).set("events", Json::Arr(items));
    envelope(store, "events", w, limit, n, result)
}

/// `patterns`: plantillas de log más frecuentes (Drain-light, desde rollups).
pub fn patterns(
    store: &Store,
    w: &Window,
    until: Option<&str>,
    scope: &chrono_metrics::EventScope,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 25;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let scope = chrono_metrics::EventScope { until_epoch: until_epoch(until)?, ..scope.clone() };
    let rows = chrono_metrics::patterns_scoped(store.conn(), w.since_epoch, &scope, limit)?;
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
    envelope_complete(store, "show", &w, 1, result)
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
    let mut result = Json::obj()
        .set("bucket_ms", Json::Int(bucket_ms))
        .set("by", group_by.map_or(Json::Null, Json::str))
        .set("until", until_json)
        .set("buckets", Json::Arr(items));
    // Los eventos sin tiempo no caben en ningún bucket: se dicen aparte.
    let untimed = chrono_metrics::untimed_events(store.conn(), scope)?;
    if untimed > 0 {
        result = with_hints(
            result.set("untimed", Json::Int(untimed)),
            vec![t(
                format!("{untimed} matching events have no time and are in no bucket"),
                format!("{untimed} eventos que casan no tienen tiempo y no están en ningún bucket"),
            )],
        );
    }
    envelope(store, "timeline", w, limit, n, result)
}

/// `top <dim>`: valores más frecuentes de una dimensión.
pub fn top(
    store: &Store,
    w: &Window,
    dim: &str,
    until: Option<&str>,
    scope: &chrono_metrics::EventScope,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 25;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let filter = chrono_metrics::TopFilter {
        source: scope.source,
        grep: scope.grep,
        level: scope.level,
        kind: scope.kind,
        entity: scope.entity,
        attr: scope.attr,
        join: scope.join,
        infer_level: scope.infer_level,
        until_epoch: until_epoch(until)?,
    };
    let rows = chrono_metrics::top_filtered(store.conn(), dim, w.since_epoch, &filter, limit)?;
    let items: Vec<Json> = rows
        .iter()
        .map(|v| Json::obj().set("value", Json::str(&v.value)).set("count", Json::Int(v.count)))
        .collect();
    let n = items.len();
    // `attr:K` con una K que no existe da vacío: no es error (`informe` pide
    // `attr:container` en cualquier índice), pero se dice.
    let mut hints = Vec::new();
    if let Some(k) = dim.strip_prefix("attr:").filter(|_| rows.is_empty()) {
        if !chrono_metrics::attr_key_known(store.conn(), k)? {
            hints.push(unknown_attr_hint(store, k)?);
        }
    }
    let result = Json::obj().set("dim", Json::str(dim)).set("top", Json::Arr(items));
    envelope(store, "top", w, limit, n, with_hints(result, hints))
}

/// `precede`: episodios de lo que casan los filtros y qué los precede.
#[allow(clippy::too_many_arguments)]
pub fn precede(
    store: &Store,
    w: &Window,
    until: Option<&str>,
    scope: &chrono_metrics::EventScope,
    gap: Option<&str>,
    window: Option<&str>,
    before_levels: Option<&str>,
    limit: Option<&str>,
) -> Result<Json> {
    let dur = |v: Option<&str>, flag: &str| -> Result<i64> {
        match v {
            None | Some("") => Ok(120_000),
            Some(s) => Ok(parse_duration(s).ok_or_else(|| {
                t(format!("invalid --{flag}: {s:?} (e.g. 2m, 90s)"), format!("--{flag} no válido: {s:?} (usa p.ej. 2m, 90s)"))
            })?),
        }
    };
    if scope.is_empty() {
        return Err(t(
            "precede needs an anchor: --grep, --entity, --attr or --level (what marks each episode)",
            "precede necesita un ancla: --grep, --entity, --attr o --level (lo que marca cada episodio)",
        )
        .into());
    }
    let max = parse_limit(limit, 50)?;
    let anchor = chrono_metrics::EventScope { until_epoch: until_epoch(until)?, ..scope.clone() };
    let o = chrono_metrics::PrecedeOpts {
        since_epoch: w.since_epoch,
        anchor,
        gap_ms: dur(gap, "gap")?,
        window_ms: dur(window, "window")?,
        levels: before_levels.unwrap_or("FATAL,CRITICAL,ERROR,WARNING"),
        max_episodes: max,
        join: scope.join.and_then(|j| j.split(['=', ':']).next()).map(str::trim).filter(|k| !k.is_empty()),
        ..Default::default()
    };
    let r = chrono_metrics::precede(store.conn(), &o)?;
    let total = r.episodes_total;
    let episodes: Vec<Json> = r
        .episodes
        .iter()
        .map(|e| {
            let pre: Vec<Json> = e
                .preceding
                .iter()
                .map(|p| {
                    Json::obj()
                        .set("level", Json::str(&p.level))
                        .set("count", Json::Int(p.count))
                        .set("secs_before", Json::Int(p.last_before_secs))
                        .set("entity", Json::str(&p.entity))
                        .set("template", Json::str(&p.template))
                })
                .collect();
            Json::obj()
                .set("start", Json::str(&epoch_to_iso8601(e.start_epoch)))
                .set("anchors", Json::Int(e.anchors))
                .set("origins", Json::str(&e.origins.join(",")))
                .set("first_id", Json::str(&e.first_id))
                .set("preceding", Json::Arr(pre))
        })
        .collect();
    let opt = |v: Option<f64>, scale: f64| v.map_or(Json::Null, |x| Json::Float((x * scale).round() / scale));
    let commons: Vec<Json> = r
        .common
        .iter()
        .map(|c| {
            Json::obj()
                .set("before_episodes", Json::str(&format!("{}/{}", c.episodes, total)))
                .set("level", Json::str(&c.level))
                .set("template", Json::str(&c.template))
                .set("active_episodes", c.active_episodes.map_or(Json::Null, Json::Int))
                .set("expected", opt(c.expected, 10.0))
                .set("lift", opt(c.lift, 100.0))
        })
        .collect();
    let n = episodes.len();
    let mut result = Json::obj().set("episodes_total", Json::Int(total as i64));
    if o.join.is_some() {
        result = result.set("anchors_without_key", Json::Int(r.anchors_without_key as i64));
    }
    let result = result
        .set("gap_ms", Json::Int(o.gap_ms))
        .set("window_ms", Json::Int(o.window_ms))
        .set("common_precursors", Json::Arr(commons))
        .set("episodes", Json::Arr(episodes));
    envelope(store, "precede", w, max, n, result)
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
    // Por tiempo solo se miran OTRAS fuentes: con una sola, siempre vacío.
    let mut hints = Vec::new();
    if n == 0 && chrono_metrics::other_timed_sources(store.conn(), id)? == Some(0) {
        let keys = chrono_metrics::event_relation_keys(store.conn(), id)?;
        let ej = keys.first().map_or("<key>".to_string(), |k| k.clone());
        let list = keys.join(", ");
        hints.push(if keys.is_empty() {
            t(
                format!("correlate by time only looks at OTHER sources and the index has no other with time; to relate by a key: --via {ej}"),
                format!("correlate por tiempo solo mira OTRAS fuentes y el índice no tiene otra con tiempo; para relacionar por clave: --via {ej}"),
            )
        } else {
            t(
                format!("correlate by time only looks at OTHER sources and the index has no other with time; to relate by a key: --via {ej} (this event's keys: {list})"),
                format!("correlate por tiempo solo mira OTRAS fuentes y el índice no tiene otra con tiempo; para relacionar por clave: --via {ej} (claves de este evento: {list})"),
            )
        });
    }
    envelope(store, "correlate", &w, limit, n, with_hints(result, hints))
}

/// `correlate <id> --via K`: lo que comparte la clave `K` con el evento, de
/// cualquier fuente, con las cifras exactas por fuente y por kind/nivel.
pub fn correlate_via(store: &Store, id: &str, via: &str, kind: Option<&str>, limit: Option<&str>) -> Result<Json> {
    let w = no_window();
    let limit = parse_limit(limit, 25)?;
    let kinds: Vec<String> = kind.unwrap_or("").split(',').map(str::to_string).collect();
    let v = chrono_metrics::correlate_via(store.conn(), id, via, &kinds, limit)?;
    let related: Vec<Json> = v.related.iter().map(event_ref_json).collect();
    let n = related.len();
    let by_source: Vec<Json> =
        v.by_source.iter().map(|(s, c)| Json::obj().set("source_id", Json::str(s)).set("count", Json::Int(*c))).collect();
    let by_kind: Vec<Json> = v
        .by_kind_level
        .iter()
        .map(|(k, l, c)| Json::obj().set("kind", Json::str(k)).set("level", Json::str(l)).set("count", Json::Int(*c)))
        .collect();
    let result = Json::obj()
        .set("via", Json::str(&v.key))
        .set("values", Json::Arr(v.values.iter().map(|x| Json::str(x)).collect()))
        .set("anchor", event_ref_json(&v.anchor))
        .set("related_total", Json::Int(v.total))
        .set("by_source", Json::Arr(by_source))
        .set("by_kind_level", Json::Arr(by_kind))
        .set("related", Json::Arr(related));
    envelope(store, "correlate", &w, limit, n, result)
}

/// `gaps --from A --to B --via K [--by U]`: «X sin Y» con cifras exactas.
pub fn gaps(
    store: &Store,
    w: &Window,
    from: Option<&str>,
    to: Option<&str>,
    via: Option<&str>,
    by: Option<&str>,
    until: Option<&str>,
    any_order: bool,
    limit: Option<&str>,
) -> Result<Json> {
    let need = |v: Option<&str>, flag: &str| -> Result<String> {
        v.filter(|s| !s.trim().is_empty()).map(str::to_string).ok_or_else(|| {
            t(
                format!("gaps needs --{flag} (gaps --from <source> --to <source> --via <key> [--by <key>])"),
                format!("gaps necesita --{flag} (gaps --from <fuente> --to <fuente> --via <clave> [--by <clave>])"),
            )
            .into()
        })
    };
    let (from_raw, to_raw, via) = (need(from, "from")?, need(to, "to")?, need(via, "via")?);
    fn side<'a>(raw: &'a str, flag: &str) -> Result<chrono_metrics::EventScope<'a>> {
        chrono_metrics::parse_side(raw).map_err(|e| t(format!("--{flag}: {}", e.en()), format!("--{flag}: {}", e.es())).into())
    }
    let (fs, ts) = (side(&from_raw, "from")?, side(&to_raw, "to")?);
    let limit = parse_limit(limit, 25)?;
    let o = chrono_metrics::GapsOpts { since_epoch: w.since_epoch, until_epoch: until_epoch(until)?, any_order, limit };
    let g = chrono_metrics::gaps(store.conn(), &fs, &ts, &via, by, &o)?;
    let sample: Vec<Json> = g
        .sample
        .iter()
        .map(|u| {
            Json::obj()
                .set("unit", Json::str(&u.unit))
                .set("events", Json::Int(u.events))
                .set("first_at", Json::str(&u.first_at))
                .set("last_at", Json::str(&u.last_at))
        })
        .collect();
    let n = sample.len();
    let result = Json::obj()
        .set("from", Json::str(&from_raw))
        .set("to", Json::str(&to_raw))
        .set("via", Json::str(&via))
        .set("by", by.map_or(Json::Null, Json::str))
        .set("order", Json::str(if any_order { "any" } else { "to_after_first_from" }))
        .set(
            "criterion",
            Json::str(&if any_order {
                t("covered if any --to event has the value, before or after (--any-order)", "cubierta si algún evento de --to tiene el valor, antes o después (--any-order)")
            } else {
                t(
                    "covered if a --to event with the value is at or after the unit's first --from event (--any-order: in any order)",
                    "cubierta si un evento de --to con el valor está en o después del primer evento de --from de la unidad (--any-order: en cualquier orden)",
                )
            }),
        )
        .set("units", Json::Int(g.units))
        .set("covered", Json::Int(g.covered))
        .set("missing", Json::Int(g.missing))
        .set("to_values", Json::Int(g.to_values))
        .set("from_without_key", Json::Int(g.from_without_key))
        .set("until", until.filter(|u| !u.is_empty()).map_or(Json::Null, Json::str))
        .set("missing_sample", Json::Arr(sample));
    envelope(store, "gaps", w, limit, n, result)
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
    // Sin Nod todo sale a cero: se dice delante, no solo en `notes` al final.
    let mut hints = Vec::new();
    if sp.events_scanned == 0 {
        hints.push(t(
            "no Nod events in the index: every figure is 0 (add them with: chrono add <events.jsonl> --as nod)",
            "no hay eventos Nod en el índice: todas las cifras son 0 (añádelos con: chrono add <events.jsonl> --as nod)",
        ));
    }
    envelope(store, "session_patterns", w, limit, returned, with_hints(result, hints))
}

/// `stats <attr>`: min/p50/p95/max/suma de un atributo numérico por grupo
/// (`--by level|kind|entity|attr:<k>`), con los filtros de siempre.
pub fn stats(
    store: &Store,
    w: &Window,
    attr: &str,
    by: Option<&str>,
    until: Option<&str>,
    scope: &chrono_metrics::EventScope,
    limit: Option<&str>,
) -> Result<Json> {
    const DEFAULT_LIMIT: usize = 25;
    let limit = parse_limit(limit, DEFAULT_LIMIT)?;
    let scope = chrono_metrics::EventScope { until_epoch: until_epoch(until)?, ..scope.clone() };
    let rows = chrono_metrics::stats(store.conn(), attr, by, w.since_epoch, &scope, limit)?;
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
    let mut hints = Vec::new();
    if rows.is_empty() {
        hints.push(if chrono_metrics::attr_key_known(store.conn(), attr)? {
            t(
                format!("the attribute {attr:?} has no numeric values in these events (stats only counts numbers)"),
                format!("la attr {attr:?} no tiene valores numéricos en estos eventos (stats solo cuenta números)"),
            )
        } else {
            unknown_attr_hint(store, attr)?
        });
    }
    envelope(store, "stats", w, limit, n, with_hints(result, hints))
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
