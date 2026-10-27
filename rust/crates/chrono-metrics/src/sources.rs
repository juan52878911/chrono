//! `sources`: inventario del índice. Por fuente, cuántos eventos tiene, su
//! ventana de tiempo, niveles, tipos y entidades principales. Es lo primero que
//! debe mirar un agente para saber si chrono ya tiene los datos que busca
//! (p. ej. los logs de Loki volcados por la fuente de comando `cmd:loki`).

use rusqlite::{params, Connection, OptionalExtension};

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
    /// Dims que declara la fuente: valen en `top <dim>`, `--attr k:v` y `--join`.
    pub dims: Vec<String>,
    /// Eventos que conservan las dims u opciones de antes: cambiaron cuando parte
    /// de la fuente ya no se podía releer y no se reindexó (ver el CLI).
    pub stale_dims_events: i64,
    /// El último `sync` no pudo leer la fuente (fichero borrado, repo movido):
    /// cuándo y por qué. Sus eventos siguen en el índice.
    pub sync_error: Option<String>,
}

/// `source_dims` guarda, junto a las dims, las firmas de `relations` (`rel:…`) y
/// de las opciones de la fuente (`opt:…`), y la marca de la versión de sus ids
/// (`ids:…`): no son claves que se puedan consultar.
pub(crate) const NOT_SIGNATURE: &str = "key NOT GLOB 'rel:*' AND key NOT GLOB 'opt:*' AND key NOT GLOB 'ids:*'";

/// `(source_id, alias)` de las fuentes que tienen alias (el de una fuente glob,
/// o `cmd:<nombre>` de una de comando), con el mismo criterio que [`sources`].
pub fn source_aliases(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT id, json_extract(manifest_json, '$.alias') FROM sources ORDER BY id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, glob_alias) = row?;
        let alias = glob_alias.or_else(|| id.rsplit('/').next().and_then(|f| f.strip_suffix(".cmd.jsonl")).map(|n| format!("cmd:{n}")));
        if let Some(a) = alias.filter(|a| !a.is_empty()) {
            out.push((id, a));
        }
    }
    Ok(out)
}

/// Una fila por fuente, orden estable por id.
pub fn sources(conn: &Connection) -> Result<Vec<SourceSummary>> {
    // v6: con `dim_rollups` al día, todo sale de ahí (mismas cifras; el test del
    // cubo lo comprueba), también el total: contarlo en `events` recorría el
    // índice de la fuente entero (segundos a 10M en `informe`). Las entidades
    // por touch, solo si ningún evento de la fuente tiene varios touches.
    let cube = crate::cube::fresh(conn)?.is_some();
    let mut stmt = conn.prepare(
        "SELECT s.ref, s.id, s.kind, CASE WHEN ?1 THEN 0 ELSE (SELECT COUNT(*) FROM events WHERE source_ref = s.ref) END,
                json_extract(s.manifest_json, '$.alias'),
                (SELECT at FROM events WHERE source_ref = s.ref AND at_epoch > 0 ORDER BY at_epoch ASC LIMIT 1),
                (SELECT at FROM events WHERE source_ref = s.ref AND at_epoch > 0 ORDER BY at_epoch DESC LIMIT 1),
                CASE WHEN EXISTS (SELECT 1 FROM meta WHERE key = 'stale_dims:' || s.id)
                     THEN (SELECT COUNT(*) FROM events WHERE source_ref = s.ref
                               AND rid <= CAST((SELECT value FROM meta WHERE key = 'stale_dims:' || s.id) AS INTEGER))
                     ELSE 0 END
         FROM sources s ORDER BY s.id",
    )?;
    type Row = (i64, String, String, i64, Option<String>, Option<String>, Option<String>, i64);
    let rows: Vec<Row> = stmt
        .query_map(params![cube], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let counts = |sql: &str, sref: i64| -> Result<Vec<(String, i64)>> {
        let mut st = conn.prepare(sql)?;
        let v = st
            .query_map(params![sref], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(v)
    };
    let mut out = Vec::new();
    for (sref, id, kind, events, glob_alias, first, last, stale_dims_events) in rows {
        // Las fuentes de comando se guardan como jsonl de su caché
        // (`…/cmd/<nombre>.cmd.jsonl`); el alias es lo que acepta `--source`.
        // Una fuente glob lleva el suyo (`add --name`) en el manifiesto.
        let alias = glob_alias.or_else(|| id.rsplit('/').next().and_then(|f| f.strip_suffix(".cmd.jsonl")).map(|n| format!("cmd:{n}")));
        let events = if cube {
            conn.query_row("SELECT COALESCE(SUM(count), 0) FROM dim_rollups WHERE source_ref = ?1", params![sref], |r| r.get(0))?
        } else {
            events
        };
        let multi: bool = cube
            && conn.query_row("SELECT EXISTS (SELECT 1 FROM dim_rollups WHERE source_ref = ?1 AND entity_id = -1)", params![sref], |r| r.get(0))?;
        let (levels, inferred, kinds) = if cube {
            (
                "SELECT level, SUM(count) c FROM dim_rollups WHERE source_ref = ?1 AND level != ''
                 GROUP BY level ORDER BY c DESC, level LIMIT 10",
                "SELECT level_inf l, SUM(count) c FROM dim_rollups WHERE source_ref = ?1 AND level = '' AND level_inf != ''
                 GROUP BY l ORDER BY c DESC, l LIMIT 10",
                "SELECT kind, SUM(count) c FROM dim_rollups WHERE source_ref = ?1 GROUP BY kind ORDER BY c DESC, kind LIMIT 5",
            )
        } else {
            (
                "SELECT level, COUNT(*) c FROM events WHERE source_ref = ?1 AND level != ''
                 GROUP BY level ORDER BY c DESC, level LIMIT 10",
                "SELECT json_extract(attrs, '$.level_inferred') l, COUNT(*) c FROM events
                 WHERE source_ref = ?1 AND level = '' AND json_extract(attrs, '$.level_inferred') IS NOT NULL
                 GROUP BY l ORDER BY c DESC, l LIMIT 10",
                "SELECT kind, COUNT(*) c FROM events WHERE source_ref = ?1 GROUP BY kind ORDER BY c DESC, kind LIMIT 5",
            )
        };
        let entities = if cube && !multi {
            "SELECT en.key, SUM(r.count) c FROM dim_rollups r JOIN entities en ON en.id = r.entity_id
             WHERE r.source_ref = ?1 GROUP BY en.key ORDER BY c DESC, en.key LIMIT 15"
        } else {
            "SELECT en.key, COUNT(*) c FROM touches t JOIN events e ON e.rid = t.event_rid
             JOIN entities en ON en.id = t.entity_id WHERE e.source_ref = ?1
             GROUP BY en.key ORDER BY c DESC, en.key LIMIT 15"
        };
        let dims = {
            let mut st = conn.prepare_cached(&format!("SELECT key FROM source_dims WHERE source_ref = ?1 AND {NOT_SIGNATURE} ORDER BY key"))?;
            let rows = st.query_map(params![sref], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let sync_error: Option<String> =
            conn.query_row("SELECT value FROM meta WHERE key = ?1", params![format!("sync_error:{id}")], |r| r.get(0)).optional()?;
        out.push(SourceSummary {
            dims,
            sync_error,
            levels: counts(levels, sref)?,
            inferred_levels: counts(inferred, sref)?,
            kinds: counts(kinds, sref)?,
            top_entities: counts(entities, sref)?,
            source: id,
            alias,
            kind,
            events,
            stale_dims_events,
            first,
            last,
        });
    }
    Ok(out)
}
