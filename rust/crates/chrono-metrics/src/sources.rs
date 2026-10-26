//! `sources`: inventario del índice. Por fuente, cuántos eventos tiene, su
//! ventana de tiempo, niveles, tipos y entidades principales. Es lo primero que
//! debe mirar un agente para saber si chrono ya tiene los datos que busca
//! (p. ej. los logs de Loki volcados por la fuente de comando `cmd:loki`).

use rusqlite::{params, Connection};

use crate::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct SourceSummary {
    /// `source_id` canónico (`<kind>:<ruta>`).
    pub source: String,
    /// `cmd:<nombre>` si es la caché de una fuente de comando: lo que acepta `--source`.
    pub alias: Option<String>,
    pub kind: String,
    pub events: i64,
    /// `at` del primer y del último evento con tiempo.
    pub first: Option<String>,
    pub last: Option<String>,
    pub levels: Vec<(String, i64)>,
    /// Eventos SIN nivel de la fuente cuyo texto lo dice (`infer_level`), por nivel.
    pub inferred_levels: Vec<(String, i64)>,
    pub kinds: Vec<(String, i64)>,
    pub top_entities: Vec<(String, i64)>,
}

/// Una fila por fuente, orden estable por id.
pub fn sources(conn: &Connection) -> Result<Vec<SourceSummary>> {
    let mut stmt = conn.prepare(
        "SELECT s.ref, s.id, s.kind, COUNT(e.id), json_extract(s.manifest_json, '$.alias'),
                (SELECT at FROM events WHERE source_ref = s.ref AND at_epoch > 0 ORDER BY at_epoch ASC LIMIT 1),
                (SELECT at FROM events WHERE source_ref = s.ref AND at_epoch > 0 ORDER BY at_epoch DESC LIMIT 1)
         FROM sources s LEFT JOIN events e ON e.source_ref = s.ref
         GROUP BY s.ref ORDER BY s.id",
    )?;
    type Row = (i64, String, String, i64, Option<String>, Option<String>, Option<String>);
    let rows: Vec<Row> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let counts = |sql: &str, sref: i64| -> Result<Vec<(String, i64)>> {
        let mut st = conn.prepare(sql)?;
        let v = st
            .query_map(params![sref], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(v)
    };
    let mut out = Vec::new();
    for (sref, id, kind, events, glob_alias, first, last) in rows {
        // Las fuentes de comando se guardan como jsonl de su caché
        // (`…/cmd/<nombre>.cmd.jsonl`); el alias es lo que acepta `--source`.
        // Una fuente glob lleva el suyo (`add --name`) en el manifiesto.
        let alias = glob_alias.or_else(|| id.rsplit('/').next().and_then(|f| f.strip_suffix(".cmd.jsonl")).map(|n| format!("cmd:{n}")));
        out.push(SourceSummary {
            levels: counts(
                "SELECT level, COUNT(*) c FROM events WHERE source_ref = ?1 AND level != ''
                 GROUP BY level ORDER BY c DESC, level LIMIT 10",
                sref,
            )?,
            inferred_levels: counts(
                "SELECT json_extract(attrs, '$.level_inferred') l, COUNT(*) c FROM events
                 WHERE source_ref = ?1 AND level = '' AND json_extract(attrs, '$.level_inferred') IS NOT NULL
                 GROUP BY l ORDER BY c DESC, l LIMIT 10",
                sref,
            )?,
            kinds: counts("SELECT kind, COUNT(*) c FROM events WHERE source_ref = ?1 GROUP BY kind ORDER BY c DESC, kind LIMIT 5", sref)?,
            top_entities: counts(
                "SELECT en.key, COUNT(*) c FROM touches t JOIN events e ON e.rid = t.event_rid
                 JOIN entities en ON en.id = t.entity_id WHERE e.source_ref = ?1
                 GROUP BY en.key ORDER BY c DESC, en.key LIMIT 15",
                sref,
            )?,
            source: id,
            alias,
            kind,
            events,
            first,
            last,
        });
    }
    Ok(out)
}
