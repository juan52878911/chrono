//! Ticket: commits y ficheros ligados a un ticket, más PRs del forge que lo
//! citen. Paridad con `Ticket` en `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// Commits, ficheros y PRs asociados a un ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub ticket: String,
    pub commits: Vec<String>,
    pub files: Vec<String>,
    pub prs: Vec<i64>,
}

/// Eventos ligados por `links(rel='ticket', target=id)` (ordenados por
/// fecha), sus entidades tocadas (hasta 50, orden alfabético), y PRs de
/// `issues` cuyo título o labels citen `id`.
pub fn ticket(conn: &Connection, id: &str) -> Result<Ticket> {
    let mut stmt = conn.prepare(
        "SELECT ev.id
         FROM links l
         JOIN events ev ON ev.id = l.event_id
         WHERE l.rel = 'ticket' AND l.target = ?1
         ORDER BY ev.at_epoch",
    )?;
    let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
    let mut commits = Vec::new();
    for row in rows {
        commits.push(row?);
    }

    let mut stmt = conn.prepare(
        "SELECT DISTINCT e.key
         FROM links l
         JOIN touches t ON t.event_id = l.event_id
         JOIN entities e ON e.id = t.entity_id
         WHERE l.rel = 'ticket' AND l.target = ?1
         ORDER BY e.key
         LIMIT 50",
    )?;
    let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }

    let mut stmt = conn.prepare(
        "SELECT number
         FROM issues
         WHERE kind = 'pr'
           AND (('#'||number) = ?1 OR title LIKE '%'||?1||'%' OR labels LIKE '%'||?1||'%')
         ORDER BY number",
    )?;
    let rows = stmt.query_map(params![id], |r| r.get::<_, i64>(0))?;
    let mut prs = Vec::new();
    for row in rows {
        prs.push(row?);
    }

    Ok(Ticket { ticket: id.to_string(), commits, files, prs })
}
