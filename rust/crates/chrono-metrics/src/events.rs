//! `events`: stream cronológico UNIFICADO de todas las fuentes, filtrable. Es
//! la vista de "trazabilidad en el tiempo": qué pasó, en qué orden, mezclando
//! git + logs + deploys + lo que haya en el índice. Lee `events` crudos.

use rusqlite::Connection;

use crate::Result;

/// Una fila del stream unificado.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub id: String,
    pub source_id: String,
    pub kind: String,
    pub at: String,
    pub at_epoch: i64,
    pub level: String,
    pub title: String,
}

/// Filtros opcionales del stream (todos AND; `None`/"" = sin ese filtro).
#[derive(Debug, Default, Clone)]
pub struct EventFilter<'a> {
    /// `since_epoch`/`until_epoch` en MILISEGUNDOS UTC (0 = sin ese límite).
    pub since_epoch: i64,
    pub until_epoch: i64,
    /// Fuente: un `kind` ("git", "jsonl"…) → casa `source_id` por prefijo
    /// `kind:%`; o un `source_id` completo (con ':') → casa exacto/prefijo.
    pub source: Option<&'a str>,
    /// Exacto, o prefijo con `*` (`ci.*`).
    pub kind: Option<&'a str>,
    /// Uno o varios niveles separados por coma, sin distinguir mayúsculas.
    pub level: Option<&'a str>,
    /// Substring en `title` (LIKE `%grep%`).
    pub grep: Option<&'a str>,
    /// Una o varias entidades exactas separadas por coma (servicio, fichero…).
    pub entity: Option<&'a str>,
    /// `clave:valor` sobre los atributos (ver `EventScope::attr`).
    pub attr: Option<&'a str>,
    /// `level` casa también el nivel deducido del texto.
    pub infer_level: bool,
}

/// Eventos en orden CRONOLÓGICO (`at_epoch` ASC, luego `id` ASC), aplicando los
/// filtros. Solo eventos con `at_epoch > 0` (los sin tiempo no entran en una
/// vista temporal). `limit` acota (salida "bounded for an AI").
pub fn events(conn: &Connection, f: &EventFilter, limit: usize) -> Result<Vec<EventRow>> {
    // Mismas reglas de filtro que `top`/`patterns`/`timeline` (ver `EventScope`).
    let scope = crate::EventScope {
        source: f.source,
        grep: f.grep,
        level: f.level,
        kind: f.kind,
        entity: f.entity,
        attr: f.attr,
        infer_level: f.infer_level,
        until_epoch: f.until_epoch,
    };
    let (filters, extra) = scope.sql(3);
    // v5: events.source_ref es entero; el string ("git:/path") se recupera por
    // JOIN a sources. El JOIN es INNER pero todo evento tiene un source_ref
    // válido (lo garantiza el Writer), así que no se pierde ninguna fila.
    let sql = format!(
        "SELECT e.id, s.id, e.kind, e.at, e.at_epoch, e.level, e.title
         FROM events e JOIN sources s ON s.ref = e.source_ref
         WHERE e.at_epoch > 0
           AND (?1 = 0 OR e.at_epoch >= ?1)
           {filters}
         ORDER BY e.at_epoch ASC, e.id ASC
         LIMIT ?2"
    );
    let limit = limit as i64;
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&f.since_epoch, &limit];
    for p in &extra {
        bound.push(p);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bound), |r| {
        Ok(EventRow {
            id: r.get(0)?,
            source_id: r.get(1)?,
            kind: r.get(2)?,
            at: r.get(3)?,
            at_epoch: r.get(4)?,
            level: r.get(5)?,
            title: r.get(6)?,
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
                .join(format!("chrono-metrics-test-events-{}-{}.db", std::process::id(), n));
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

    fn ev(id: &str, src: &str, kind: &str, at: i64, level: &str, title: &str) -> Event {
        Event {
            id: id.to_string(),
            source_id: src.to_string(),
            kind: kind.to_string(),
            at: format!("ms:{at}"),
            at_epoch: at,
            actor: Actor::default(),
            title: title.to_string(),
            level: level.to_string(),
            ..Default::default()
        }
    }

    fn fixture() -> (TempDb, Store) {
        let db = TempDb::new();
        let mut store = Store::open(db.path()).unwrap();
        let evs = [
            ev("c1", "git:/repo", "commit", 1000, "", "fix parser"),
            ev("d1", "jsonl:/deploys", "log", 2000, "INFO", "deploy api"),
            ev("e1", "jsonl:/errors", "log", 2500, "ERROR", "parser crash"),
            ev("c2", "git:/repo", "commit", 3000, "", "revert fix"),
            ev("z0", "jsonl:/errors", "log", 0, "INFO", "sin tiempo"), // at_epoch 0 -> fuera
        ];
        let mut w = store.writer().unwrap();
        for e in &evs {
            w.add_event(e).unwrap();
        }
        w.commit().unwrap();
        (db, store)
    }

    #[test]
    fn stream_cronologico_mezcla_fuentes_y_excluye_sin_tiempo() {
        let (_db, store) = fixture();
        let rows = events(store.conn(), &EventFilter::default(), 100).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        // Orden por at_epoch; z0 (at_epoch 0) excluido.
        assert_eq!(ids, vec!["c1", "d1", "e1", "c2"]);
    }

    #[test]
    fn filtros_source_kind_level_grep_y_ventana() {
        let (_db, store) = fixture();
        let conn = store.conn();

        // source por kind (prefijo "jsonl:%").
        let f = EventFilter { source: Some("jsonl"), ..Default::default() };
        assert_eq!(events(conn, &f, 100).unwrap().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), vec!["d1", "e1"]);

        // kind exacto.
        let f = EventFilter { kind: Some("commit"), ..Default::default() };
        assert_eq!(events(conn, &f, 100).unwrap().len(), 2);

        // level.
        let f = EventFilter { level: Some("ERROR"), ..Default::default() };
        assert_eq!(events(conn, &f, 100).unwrap()[0].id, "e1");

        // grep en title.
        let f = EventFilter { grep: Some("parser"), ..Default::default() };
        assert_eq!(events(conn, &f, 100).unwrap().len(), 2); // "fix parser", "parser crash"

        // ventana [2000, 3000): d1 y e1.
        let f = EventFilter { since_epoch: 2000, until_epoch: 3000, ..Default::default() };
        assert_eq!(events(conn, &f, 100).unwrap().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), vec!["d1", "e1"]);

        // limit.
        assert_eq!(events(conn, &EventFilter::default(), 2).unwrap().len(), 2);
    }
}
