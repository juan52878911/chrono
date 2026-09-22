//! Bugs: errores más comunes — DÓNDE se concentran (`hot_areas`, por
//! directorio) y de QUÉ tipo son (`categories`, por
//! `label(task='bug_category')`). Paridad con `CommonBugs` en
//! `internal/metrics/metrics.go`, salvo que aquí las categorías ya vienen del
//! clasificador (tabla `labels`) en vez de una taxonomía de palabras clave
//! aplicada en caliente sobre el mensaje.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{params, Connection};

use crate::Result;

/// Días de "reciente" para `BugArea::recent_fixes`: igual que el Go, se
/// cuentan desde el evento más nuevo del ÍNDICE completo, sin acotar por la
/// ventana `since_epoch` de la consulta.
const RECENT_DAYS: i64 = 90;

/// Un tema recurrente entre los fixes (viene de `label(task='bug_category')`).
#[derive(Debug, Clone, PartialEq)]
pub struct BugCategory {
    pub category: String,
    pub fixes: i64,
    pub top_files: Vec<String>,
    pub examples: Vec<String>,
}

/// Una zona (directorio) con concentración de fixes.
#[derive(Debug, Clone, PartialEq)]
pub struct BugArea {
    pub dir: String,
    pub fixes: i64,
    pub recent_fixes: i64,
    pub examples: Vec<String>,
}

/// "Errores más comunes": `categories` (taxonomía ya clasificada) +
/// `hot_areas` (directorios con más fixes), en la ventana `since_epoch`.
#[derive(Debug, Clone, PartialEq)]
pub struct Bugs {
    pub total_fixes: i64,
    pub categories: Vec<BugCategory>,
    pub hot_areas: Vec<BugArea>,
}

/// Lee `labels`: fixes = eventos con `label(task='is_fix', label='true')`;
/// categorías desde `label(task='bug_category')` de esos mismos eventos;
/// `hot_areas` agrupa por los dos primeros segmentos de `entity` (o
/// "(raíz)" si solo tiene uno, igual que `topDir` en el Go).
pub fn bugs(conn: &Connection, since_epoch: i64, limit: usize) -> Result<Bugs> {
    // Umbral de "reciente": 90 días antes del evento más nuevo del índice
    // completo (no de la ventana), igual que el Go.
    let max_epoch: Option<i64> =
        conn.query_row("SELECT MAX(at_epoch) FROM events", [], |r| r.get(0))?;
    let recent_threshold = match max_epoch {
        Some(m) => m - RECENT_DAYS * 86_400,
        None => i64::MIN,
    };

    // 1) IDs de eventos fix en la ventana, con su epoch (para total_fixes y
    // para decidir qué cuenta como "reciente" en hot_areas).
    let mut fixes: BTreeMap<String, i64> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT ev.id, ev.at_epoch
             FROM labels l
             JOIN events ev ON ev.id = l.event_id
             WHERE l.task = 'is_fix' AND l.label = 'true' AND ev.is_bulk = 0
               AND ev.at_epoch >= ?1",
        )?;
        let rows = stmt.query_map(params![since_epoch], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, at_epoch) = row?;
            fixes.insert(id, at_epoch);
        }
    }
    let total_fixes = fixes.len() as i64;

    // Touches de esos eventos fix -> agregación por directorio.
    struct AreaAgg {
        seen: BTreeSet<String>,
        recent: i64,
        examples: Vec<String>,
    }
    let mut areas: HashMap<String, AreaAgg> = HashMap::new();
    if !fixes.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT ev.id, e.key
             FROM labels l
             JOIN events ev ON ev.id = l.event_id
             JOIN touches t ON t.event_id = ev.id
             JOIN entities e ON e.id = t.entity_id
             WHERE l.task = 'is_fix' AND l.label = 'true' AND ev.is_bulk = 0
               AND e.excluded = 0 AND ev.at_epoch >= ?1
             ORDER BY ev.id",
        )?;
        let rows = stmt.query_map(params![since_epoch], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, entity) = row?;
            let dir = top_dir(&entity);
            let at_epoch = *fixes.get(&id).unwrap_or(&0);
            let agg = areas.entry(dir).or_insert_with(|| AreaAgg {
                seen: BTreeSet::new(),
                recent: 0,
                examples: Vec::new(),
            });
            if agg.seen.insert(id.clone()) {
                if at_epoch >= recent_threshold {
                    agg.recent += 1;
                }
                if agg.examples.len() < 2 {
                    agg.examples.push(short_id(&id));
                }
            }
        }
    }

    let mut hot_areas: Vec<BugArea> = areas
        .into_iter()
        .map(|(dir, agg)| BugArea {
            dir,
            fixes: agg.seen.len() as i64,
            recent_fixes: agg.recent,
            examples: agg.examples,
        })
        .collect();
    hot_areas.sort_by(|a, b| {
        let ka = a.fixes + a.recent_fixes;
        let kb = b.fixes + b.recent_fixes;
        kb.cmp(&ka).then_with(|| a.dir.cmp(&b.dir))
    });
    hot_areas.truncate(limit);

    // 2) Categorías: label(task='bug_category') de eventos fix en la ventana.
    let mut cat_events: BTreeMap<String, Vec<String>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT l.label, l.event_id
             FROM labels l
             JOIN events ev ON ev.id = l.event_id
             JOIN labels lf ON lf.event_id = l.event_id AND lf.task = 'is_fix' AND lf.label = 'true'
             WHERE l.task = 'bug_category' AND ev.is_bulk = 0 AND ev.at_epoch >= ?1
             ORDER BY l.event_id",
        )?;
        let rows = stmt.query_map(params![since_epoch], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (category, event_id) = row?;
            cat_events.entry(category).or_default().push(event_id);
        }
    }

    let mut cat_files: BTreeMap<String, HashMap<String, i64>> = BTreeMap::new();
    if !cat_events.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT l.label, e.key
             FROM labels l
             JOIN events ev ON ev.id = l.event_id
             JOIN labels lf ON lf.event_id = l.event_id AND lf.task = 'is_fix' AND lf.label = 'true'
             JOIN touches t ON t.event_id = ev.id
             JOIN entities e ON e.id = t.entity_id
             WHERE l.task = 'bug_category' AND ev.is_bulk = 0 AND e.excluded = 0
               AND ev.at_epoch >= ?1",
        )?;
        let rows = stmt.query_map(params![since_epoch], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (category, file) = row?;
            *cat_files.entry(category).or_default().entry(file).or_insert(0) += 1;
        }
    }

    let mut categories: Vec<BugCategory> = cat_events
        .into_iter()
        .map(|(category, ids)| {
            let examples = ids.iter().take(2).map(|id| short_id(id)).collect();
            let top_files = top_n(cat_files.get(&category).cloned().unwrap_or_default(), 3);
            BugCategory { category, fixes: ids.len() as i64, top_files, examples }
        })
        .collect();
    categories.sort_by(|a, b| b.fixes.cmp(&a.fixes).then_with(|| a.category.cmp(&b.category)));
    categories.truncate(limit);

    Ok(Bugs { total_fixes, categories, hot_areas })
}

/// Primer/segundo segmento de una ruta jerárquica ("a/b/c" -> "a/b", "a/b" ->
/// "a/b"); "(raíz)" si solo tiene un segmento. Réplica literal de `topDir`.
fn top_dir(entity: &str) -> String {
    let parts: Vec<&str> = entity.split('/').collect();
    if parts.len() <= 1 {
        "(raíz)".to_string()
    } else {
        format!("{}/{}", parts[0], parts[1])
    }
}

/// Igual que `sha[:min(8, len(sha))]` en el Go.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn top_n(counts: HashMap<String, i64>, n: usize) -> Vec<String> {
    let mut v: Vec<(String, i64)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().take(n).map(|(k, _)| k).collect()
}
