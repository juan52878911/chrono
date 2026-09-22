//! Consultas (`hotspots`, `coupling`, `owners`, `churn`, `search`, `similar`)
//! y el envelope JSON común de `docs/OUTPUT-CONTRACT.md`, con la convención
//! v2: `entity`/`id` en vez de `path`/`sha`.

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
