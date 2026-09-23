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
    let mut stmt = conn.prepare(
        "SELECT t.template, SUM(r.count) AS c, MAX(r.last_id) AS ex
         FROM rollups r
         JOIN templates t ON t.id = r.template_id
         WHERE (?1 = 0 OR r.bucket_epoch >= ?1)
         GROUP BY r.template_id
         ORDER BY c DESC, t.template ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_epoch, limit as i64], |r| {
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
}
