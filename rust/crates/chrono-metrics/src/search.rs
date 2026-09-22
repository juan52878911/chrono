//! Search: búsqueda de eventos por texto. Paridad con `Search` en
//! `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// Un resultado de búsqueda.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: String,
    pub title: String,
    pub at: String,
}

/// Búsqueda de eventos por texto. Usa FTS5 (orden por `rank`, BM25) con la
/// query ESCAPADA como frase entre comillas dobles (duplicando las internas)
/// para que un `:` o unas comillas no rompan la sintaxis `MATCH`. Si la
/// consulta FTS falla o no devuelve filas, cae a `LIKE` sobre título+cuerpo.
pub fn search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let escaped = format!("\"{}\"", query.replace('"', "\"\""));
    match run_fts(conn, &escaped, limit) {
        Ok(hits) if !hits.is_empty() => Ok(hits),
        _ => run_like(conn, query, limit),
    }
}

fn run_fts(conn: &Connection, escaped_query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let mut stmt = conn.prepare(
        "SELECT ev.id, ev.title, ev.at
         FROM events_fts f
         JOIN events ev ON ev.rowid = f.rowid
         WHERE events_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![escaped_query, limit as i64], |r| {
        Ok(SearchHit { id: r.get(0)?, title: r.get(1)?, at: r.get(2)? })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn run_like(conn: &Connection, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let pattern = format!("%{query}%");
    let mut stmt = conn.prepare(
        "SELECT id, title, at FROM events
         WHERE (title || ' ' || body) LIKE ?1
         ORDER BY at DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |r| {
        Ok(SearchHit { id: r.get(0)?, title: r.get(1)?, at: r.get(2)? })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
