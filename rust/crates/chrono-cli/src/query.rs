//! Consultas (`hotspots`, `coupling`, `owners`, `churn`, `search`, `similar`,
//! `bugs`, `tickets`, `prs`, `phases`, `branches`) y el envelope JSON común de
//! `docs/OUTPUT-CONTRACT.md`, con la convención v2: `entity`/`id` en vez de
//! `path`/`sha`.

use std::path::Path;

use chrono_store::Store;

use crate::json::Json;
use crate::timeutil::{epoch_to_iso8601, now_epoch, parse_since};

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
                None => Err(format!("--since no válido: {s:?} (usa YYYY-MM-DD)").into()),
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

pub fn hotspots(store: &Store, w: &Window) -> Result<Json> {
    const LIMIT: usize = 25;
    let rows = chrono_metrics::hotspots(store.conn(), w.since_epoch, LIMIT)?;
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

pub fn coupling(store: &Store, w: &Window, entity: &str) -> Result<Json> {
    const LIMIT: usize = 25;
    let (rows, _total) = chrono_metrics::coupling(store.conn(), entity, 2, w.since_epoch, LIMIT)?;
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

pub fn owners(store: &Store, w: &Window, prefix: &str) -> Result<Json> {
    let (rows, bus_factor) = chrono_metrics::owners(store.conn(), prefix, w.since_epoch)?;
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

pub fn churn(store: &Store, w: &Window) -> Result<Json> {
    const LIMIT: usize = 25;
    let rows = chrono_metrics::churn(store.conn(), w.since_epoch, LIMIT)?;
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
