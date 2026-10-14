//! `patterns`: plantillas de log más frecuentes (Drain-light). Agrupa millones
//! de líneas que solo difieren en valores en unas pocas plantillas, leyendo los
//! `rollups` precomputados (NO escanea `events`). Ver
//! `docs/DESIGN-GENERAL-CORE.md §6`.

use rusqlite::{params, Connection};

use crate::Result;

/// Una plantilla y su frecuencia total (suma de rollups).
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub template: String,
    pub count: i64,
    /// Un event id de ejemplo (el último visto de la plantilla) para inspección.
    pub example_id: String,
}

/// Plantillas más frecuentes por nº de eventos. `since_epoch` filtra por
/// `bucket_epoch` (0 = sin límite). Orden determinista: `count` DESC, luego
/// `template` ASC. `limit` acota.
pub fn patterns(conn: &Connection, since_epoch: i64, limit: usize) -> Result<Vec<Pattern>> {
    patterns_filtered(conn, since_epoch, None, None, limit)
}

/// Como [`patterns`], acotado a una fuente y/o a las plantillas que contienen un texto.
///
/// `source` sigue la semántica de `events --source`: un tipo (`"template"`, `"nod"`, `"git"`…) casa
/// los `source_id` que empiezan por `tipo:`; un id con `:` casa por prefijo. Sin esto, en un índice
/// que mezcla git, Nod y logs, los miles de eventos de una fuente tapan los patrones de las demás.
/// `grep` filtra por subcadena de la plantilla. El orden sigue siendo total (cuenta, plantilla).
pub fn patterns_filtered(
    conn: &Connection,
    since_epoch: i64,
    source: Option<&str>,
    grep: Option<&str>,
    limit: usize,
) -> Result<Vec<Pattern>> {
    let source_like: Option<String> = source
        .filter(|s| !s.is_empty())
        .map(|s| if s.contains(':') { format!("{s}%") } else { format!("{s}:%") });
    let grep_like: Option<String> = grep.filter(|g| !g.is_empty()).map(|g| format!("%{g}%"));
    let mut stmt = conn.prepare(
        "SELECT t.template, SUM(r.count) AS c, MAX(r.last_id) AS ex
         FROM rollups r
         JOIN templates t ON t.id = r.template_id
         WHERE (?1 = 0 OR r.bucket_epoch >= ?1)
           AND (?3 IS NULL OR r.source_id LIKE ?3)
           AND (?4 IS NULL OR t.template LIKE ?4)
         GROUP BY r.template_id
         ORDER BY c DESC, t.template ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_epoch, limit as i64, source_like, grep_like], |r| {
        Ok(Pattern {
            template: r.get::<_, String>(0)?,
            count: r.get::<_, i64>(1)?,
            example_id: r.get::<_, String>(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Como [`patterns_filtered`], acotado además a un NIVEL (`ERROR`, `failure`…).
pub fn patterns_by_level(
    conn: &Connection,
    since_epoch: i64,
    source: Option<&str>,
    grep: Option<&str>,
    level: &str,
    limit: usize,
) -> Result<Vec<Pattern>> {
    let scope = crate::EventScope { source, grep, level: Some(level), ..Default::default() };
    patterns_scoped(conn, since_epoch, &scope, limit)
}

/// Plantillas con los mismos filtros que `events`/`top`/`timeline` (ver
/// [`crate::EventScope`]): «las 3 plantillas FATAL más frecuentes», «las de
/// `product_service` entre las 18 y las 19».
///
/// Si solo se filtra por fuente y/o texto, lee los `rollups` como siempre. Con
/// nivel, tipo, entidad o fin de ventana no sirven (no los guardan), así que
/// agrupa en el momento sobre `events`: O(eventos que casan), que en la práctica
/// son pocos, porque lo que se acota así son los errores (en 24 h de logs
/// reales, 114 de 17 505). `grep` casa entonces con el título del evento.
pub fn patterns_scoped(conn: &Connection, since_epoch: i64, scope: &crate::EventScope, limit: usize) -> Result<Vec<Pattern>> {
    use std::collections::HashMap;
    if scope.only_source_grep() {
        return patterns_filtered(conn, since_epoch, scope.source, scope.grep, limit);
    }
    let (filters, extra) = scope.sql(2);
    let sql = format!(
        "SELECT e.id, e.title
         FROM events e
         WHERE (?1 = 0 OR e.at_epoch >= ?1)
           {filters}
         ORDER BY e.rowid"
    );
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&since_epoch];
    for p in &extra {
        bound.push(p);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bound), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    // plantilla -> (cuenta, último id): el mismo `example_id` que los rollups
    // (el último visto, en orden de inserción).
    let mut agg: HashMap<String, (i64, String)> = HashMap::new();
    for row in rows {
        let (id, title) = row?;
        let e = agg.entry(chrono_core::templatize(&title)).or_insert((0, String::new()));
        e.0 += 1;
        e.1 = id;
    }
    let mut out: Vec<Pattern> =
        agg.into_iter().map(|(template, (count, example_id))| Pattern { template, count, example_id }).collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.template.cmp(&b.template)));
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono_core::{Actor, Event};
    use chrono_store::Store;

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        path: PathBuf,
    }
    impl TempDb {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("chrono-metrics-test-patterns-{}-{}.db", std::process::id(), n));
            let _ = std::fs::remove_file(&path);
            TempDb { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            for s in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{s}", self.path.display()));
            }
        }
    }

    fn ev(id: &str, at_epoch: i64, title: &str) -> Event {
        Event {
            id: id.to_string(),
            source_id: "jsonl:/log".to_string(),
            kind: "log".to_string(),
            at: format!("1970-01-01T00:00:{:02}Z", at_epoch % 60),
            at_epoch,
            actor: Actor::default(),
            title: title.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn agrupa_por_plantilla_y_ordena_por_frecuencia() {
        let db = TempDb::new();
        let mut store = Store::open(db.path()).unwrap();
        {
            let mut w = store.writer().unwrap();
            // 3 "request took N ms" (misma plantilla), 1 "cache miss".
            w.add_event(&ev("a", 100, "request took 12 ms")).unwrap();
            w.add_event(&ev("b", 101, "request took 340 ms")).unwrap();
            w.add_event(&ev("c", 102, "request took 5 ms")).unwrap();
            w.add_event(&ev("d", 103, "cache miss")).unwrap();
            w.commit().unwrap();
        }
        store.build_rollups(60).unwrap();

        let pats = patterns(store.conn(), 0, 10).unwrap();
        assert_eq!(pats.len(), 2);
        assert_eq!(pats[0].template, "request took <NUM> ms");
        assert_eq!(pats[0].count, 3);
        assert_eq!(pats[1].template, "cache miss");
        assert_eq!(pats[1].count, 1);
    }

    #[test]
    fn respeta_since_y_limit() {
        let db = TempDb::new();
        let mut store = Store::open(db.path()).unwrap();
        {
            let mut w = store.writer().unwrap();
            w.add_event(&ev("a", 100, "alfa 1")).unwrap();
            w.add_event(&ev("b", 100_000, "beta 2")).unwrap();
            w.commit().unwrap();
        }
        store.build_rollups(60).unwrap();

        // since recorta por bucket_epoch.
        let recent = patterns(store.conn(), 1000, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].template, "beta <NUM>");
        // limit acota.
        assert_eq!(patterns(store.conn(), 0, 1).unwrap().len(), 1);
    }

    /// `patterns_filtered` acota por fuente y por texto de la plantilla, sin cambiar el orden total.
    #[test]
    fn patterns_filtered_acota_por_fuente_y_texto() {
        let db = TempDb::new();
        let mut store = Store::open(db.path()).unwrap();
        {
            let mut w = store.writer().unwrap();
            w.add_event(&ev("a", 100, "request took 12 ms")).unwrap();
            w.add_event(&ev("b", 101, "request took 340 ms")).unwrap();
            w.add_event(&ev("c", 102, "request took 5 ms")).unwrap();
            for (i, id) in ["t1", "t2"].iter().enumerate() {
                let mut e = ev(id, 103 + i as i64, &format!("listen EADDRINUSE :::{}", 3000 + i));
                e.source_id = "template:/logs/tests.log".to_string();
                w.add_event(&e).unwrap();
            }
            w.commit().unwrap();
        }
        store.build_rollups(60).unwrap();
        let only = |source, grep| -> Vec<(String, i64)> {
            patterns_filtered(store.conn(), 0, source, grep, 10).unwrap().into_iter().map(|p| (p.template, p.count)).collect()
        };
        assert_eq!(only(Some("template"), None), vec![("listen EADDRINUSE :::<NUM>".to_string(), 2)]);
        assert_eq!(only(Some("jsonl:/log"), None), vec![("request took <NUM> ms".to_string(), 3)]);
        assert_eq!(only(None, Some("EADDRINUSE")), vec![("listen EADDRINUSE :::<NUM>".to_string(), 2)]);
        assert_eq!(only(None, None).len(), patterns(store.conn(), 0, 10).unwrap().len());
    }

    #[test]
    fn patterns_by_level_solo_cuenta_ese_nivel() {
        let db = TempDb::new();
        let mut store = Store::open(db.path()).unwrap();
        let lv = |id: &str, at: i64, level: &str, title: &str| Event { level: level.to_string(), ..ev(id, at, title) };
        {
            let mut w = store.writer().unwrap();
            w.add_event(&lv("a", 100, "INFO", "request took 12 ms")).unwrap();
            w.add_event(&lv("b", 101, "INFO", "request took 40 ms")).unwrap();
            w.add_event(&lv("c", 102, "ERROR", "permission denied for schema x")).unwrap();
            w.add_event(&lv("d", 103, "error", "permission denied for schema x")).unwrap();
            w.add_event(&lv("e", 104, "ERROR", "timeout after 30 s")).unwrap();
            w.commit().unwrap();
        }
        let pats = patterns_by_level(store.conn(), 0, None, None, "ERROR", 10).unwrap();
        assert_eq!(pats.len(), 2, "solo los ERROR, sin distinguir mayúsculas");
        assert_eq!(pats[0].template, "permission denied for schema x");
        assert_eq!(pats[0].count, 2);
        assert_eq!(pats[0].example_id, "d", "el último visto, como en los rollups");
        assert_eq!(pats[1].template, "timeout after <NUM> s");
        let solo_timeout = patterns_by_level(store.conn(), 0, None, Some("timeout"), "error", 10).unwrap();
        assert_eq!(solo_timeout.len(), 1);
    }

    #[test]
    fn patterns_scoped_acota_por_varios_niveles_entidad_y_ventana() {
        let db = TempDb::new();
        let mut store = Store::open(db.path()).unwrap();
        let mk = |id: &str, at: i64, level: &str, svc: &str, title: &str| Event {
            level: level.to_string(),
            touches: vec![chrono_core::Touch { entity: svc.to_string(), entity_type: "service".into(), weight: 1, ..Default::default() }],
            ..ev(id, at, title)
        };
        {
            let mut w = store.writer().unwrap();
            w.add_event(&mk("a", 100, "FATAL", "postgres", "terminating connection 1")).unwrap();
            w.add_event(&mk("b", 101, "fatal", "postgres", "terminating connection 2")).unwrap();
            w.add_event(&mk("c", 102, "ERROR", "api", "boom 3")).unwrap();
            w.add_event(&mk("d", 103, "INFO", "postgres", "ready 4")).unwrap();
            w.add_event(&mk("e", 200, "CRITICAL", "api", "rls 5")).unwrap();
            w.commit().unwrap();
        }
        let scope = |level, entity, until_epoch| crate::EventScope { level, entity, until_epoch, ..Default::default() };
        let all = patterns_scoped(store.conn(), 0, &scope(Some("FATAL, error,Critical"), None, 0), 10).unwrap();
        let total: i64 = all.iter().map(|p| p.count).sum();
        assert_eq!(total, 4, "lista de niveles sin distinguir mayúsculas ni espacios: {all:?}");
        let pg = patterns_scoped(store.conn(), 0, &scope(Some("FATAL,ERROR"), Some("postgres"), 0), 10).unwrap();
        assert_eq!(pg.len(), 1);
        assert_eq!(pg[0].count, 2);
        let antes = patterns_scoped(store.conn(), 0, &scope(Some("ERROR,CRITICAL"), Some("api,otro"), 150), 10).unwrap();
        assert_eq!(antes.len(), 1, "until excluye el CRITICAL de at=200: {antes:?}");
        assert_eq!(antes[0].template, "boom <NUM>");
    }
}
