//! Tests de integración de `chrono-store` sobre ficheros SQLite temporales.
//! No se añade `tempfile` como dependencia: basta un nombre único bajo
//! `std::env::temp_dir()`, limpiado (db + -wal/-shm) al final de cada test.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono_core::{Actor, Event, Label, Link, Touch};

use crate::Store;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fichero temporal único; borra `.db`, `.db-wal` y `.db-shm` al hacer `Drop`.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir()
            .join(format!("chrono-store-test-{name}-{}-{}.db", std::process::id(), n));
        // Por si quedó basura de una corrida anterior con el mismo nombre.
        let _ = std::fs::remove_file(&path);
        TempDb { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn sample_event(id: &str, entities: &[&str], actor_key: &str) -> Event {
    Event {
        id: id.to_string(),
        source_id: "git:/repo".to_string(),
        kind: "commit".to_string(),
        at: "2026-01-01T00:00:00Z".to_string(),
        at_epoch: 1_767_225_600,
        actor: Actor { name: "Juan".to_string(), key: actor_key.to_string() },
        title: format!("commit {id}"),
        body: "cuerpo del mensaje".to_string(),
        level: String::new(),
        attrs: attrs(&[("dummy", "1")]),
        touches: entities
            .iter()
            .map(|e| Touch {
                entity: e.to_string(),
                entity_type: "file".to_string(),
                weight: 5,
                attrs: attrs(&[("added", "3"), ("deleted", "2")]),
            })
            .collect(),
        links: vec![Link { rel: "ticket".to_string(), target: "PROJ-1".to_string() }],
        simhash: 42,
        is_bulk: false,
    }
}

#[test]
fn open_crea_esquema_y_version_2_y_reabrir_no_falla() {
    let db = TempDb::new("open");

    let store = Store::open(db.path()).expect("open crea el índice");
    assert_eq!(store.meta("schema_version").unwrap().as_deref(), Some("2"));
    drop(store);

    // Reabrir un índice ya existente (mismo schema_version) no debe fallar.
    let store2 = Store::open(db.path()).expect("reabrir no falla");
    assert_eq!(store2.meta("schema_version").unwrap().as_deref(), Some("2"));
}

#[test]
fn set_meta_y_meta_hacen_roundtrip() {
    let db = TempDb::new("meta");
    let store = Store::open(db.path()).unwrap();

    assert_eq!(store.meta("git_version").unwrap(), None);
    store.set_meta("git_version", "2.45.0").unwrap();
    assert_eq!(store.meta("git_version").unwrap().as_deref(), Some("2.45.0"));

    // set_meta sobre una clave existente reemplaza el valor (upsert).
    store.set_meta("git_version", "2.46.0").unwrap();
    assert_eq!(store.meta("git_version").unwrap().as_deref(), Some("2.46.0"));
}

#[test]
fn add_event_inserta_evento_touches_links_actor_y_entities() {
    let db = TempDb::new("add_event");
    let mut store = Store::open(db.path()).unwrap();

    {
        let mut w = store.writer().unwrap();
        let mut e1 = sample_event("sha1", &["src/a.rs", "src/b.rs"], "juan@example.com");
        e1.is_bulk = true;
        w.add_event(&e1).unwrap();

        let e2 = sample_event("sha2", &["src/a.rs"], "juan@example.com");
        w.add_event(&e2).unwrap();

        // Tercer evento de otro actor, sin touches (solo para variar recuentos).
        let mut e3 = sample_event("sha3", &[], "otra@example.com");
        e3.touches.clear();
        w.add_labels(&e3.id, &[]).unwrap(); // no-op: evento aún no insertado, pero no debe fallar.
        w.add_event(&e3).unwrap();
        w.add_labels(
            &e3.id,
            &[Label {
                task: "kind".to_string(),
                label: "chore".to_string(),
                confidence: 0.9,
                source: "rules".to_string(),
                evidence: vec!["sin cambios".to_string()],
            }],
        )
        .unwrap();

        w.commit().unwrap();
    }

    let conn = store.conn();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };

    assert_eq!(count("SELECT count(*) FROM events"), 3);
    assert_eq!(count("SELECT count(*) FROM touches"), 3); // 2 de sha1 + 1 de sha2
    assert_eq!(count("SELECT count(*) FROM entities"), 2); // src/a.rs, src/b.rs
    assert_eq!(count("SELECT count(*) FROM actors"), 2);
    assert_eq!(count("SELECT count(*) FROM links"), 3); // uno por evento
    assert_eq!(count("SELECT count(*) FROM labels"), 1);

    let touches_n: i64 =
        conn.query_row("SELECT touches_n FROM events WHERE id='sha1'", [], |r| r.get(0)).unwrap();
    assert_eq!(touches_n, 2);

    let is_bulk: i64 =
        conn.query_row("SELECT is_bulk FROM events WHERE id='sha1'", [], |r| r.get(0)).unwrap();
    assert_eq!(is_bulk, 1);

    let is_bulk2: i64 =
        conn.query_row("SELECT is_bulk FROM events WHERE id='sha2'", [], |r| r.get(0)).unwrap();
    assert_eq!(is_bulk2, 0);
}

#[test]
fn add_event_es_idempotente_en_reingesta() {
    let db = TempDb::new("idempotente");
    let mut store = Store::open(db.path()).unwrap();

    let ev = sample_event("sha1", &["src/a.rs"], "juan@example.com");
    {
        let mut w = store.writer().unwrap();
        w.add_event(&ev).unwrap();
        w.add_event(&ev).unwrap(); // reingesta del mismo evento en la misma tx
        w.commit().unwrap();
    }
    {
        let mut w = store.writer().unwrap();
        w.add_event(&ev).unwrap(); // reingesta en una tx nueva
        w.commit().unwrap();
    }

    let conn = store.conn();
    let events: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
    let touches: i64 = conn.query_row("SELECT count(*) FROM touches", [], |r| r.get(0)).unwrap();
    let actors: i64 = conn.query_row("SELECT count(*) FROM actors", [], |r| r.get(0)).unwrap();
    assert_eq!(events, 1);
    assert_eq!(touches, 1);
    assert_eq!(actors, 1);
}

#[test]
fn writer_sin_commit_revierte_al_hacer_drop() {
    let db = TempDb::new("rollback");
    let mut store = Store::open(db.path()).unwrap();

    {
        let mut w = store.writer().unwrap();
        w.add_event(&sample_event("sha1", &["src/a.rs"], "juan@example.com")).unwrap();
        // Se deja caer `w` sin llamar a commit().
    }

    let conn = store.conn();
    let events: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
    assert_eq!(events, 0);
}

#[test]
fn weight_binario_menos_uno_se_guarda_tal_cual() {
    let db = TempDb::new("binario");
    let mut store = Store::open(db.path()).unwrap();

    let mut ev = sample_event("sha1", &["img.png"], "juan@example.com");
    ev.touches[0].weight = -1;
    {
        let mut w = store.writer().unwrap();
        w.add_event(&ev).unwrap();
        w.commit().unwrap();
    }

    let weight: i64 =
        store.conn().query_row("SELECT weight FROM touches WHERE event_id='sha1'", [], |r| r.get(0)).unwrap();
    assert_eq!(weight, -1);
}

#[test]
fn top_changed_entities_ordena_por_touches_desc() {
    let db = TempDb::new("top_entities");
    let mut store = Store::open(db.path()).unwrap();

    {
        let mut w = store.writer().unwrap();
        w.add_event(&sample_event("sha1", &["hot.rs", "warm.rs"], "a@x.com")).unwrap();
        w.add_event(&sample_event("sha2", &["hot.rs"], "a@x.com")).unwrap();
        w.add_event(&sample_event("sha3", &["hot.rs", "cold.rs"], "a@x.com")).unwrap();
        w.commit().unwrap();
    }

    let top = store.top_changed_entities(2).unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0], "hot.rs"); // 3 touches
    assert!(top[1] == "warm.rs" || top[1] == "cold.rs"); // empatan a 1 touch
}

#[test]
fn blob_lines_roundtrip_y_set_entity_size() {
    let db = TempDb::new("blob_lines");
    let mut store = Store::open(db.path()).unwrap();

    {
        let mut w = store.writer().unwrap();
        w.add_event(&sample_event("sha1", &["src/a.rs"], "a@x.com")).unwrap();
        w.commit().unwrap();
    }

    let before = store.get_blob_lines(&["oid1".to_string(), "oid2".to_string()]).unwrap();
    assert!(before.is_empty());

    store
        .put_blob_lines(&[("oid1".to_string(), 120), ("oid2".to_string(), 40)])
        .unwrap();
    // Upsert: reescribir oid1 con otro valor no debe duplicar la fila.
    store.put_blob_lines(&[("oid1".to_string(), 130)]).unwrap();

    let after = store.get_blob_lines(&["oid1".to_string(), "oid2".to_string(), "oid3".to_string()]).unwrap();
    assert_eq!(after.get("oid1"), Some(&130));
    assert_eq!(after.get("oid2"), Some(&40));
    assert_eq!(after.get("oid3"), None);

    store.set_entity_size("src/a.rs", 500).unwrap();
    let size: i64 = store
        .conn()
        .query_row("SELECT size FROM entities WHERE key = 'src/a.rs'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(size, 500);
}

#[test]
fn rebuild_fts_permite_buscar_por_titulo_y_cuerpo() {
    let db = TempDb::new("fts");
    let mut store = Store::open(db.path()).unwrap();

    {
        let mut w = store.writer().unwrap();
        let mut e1 = sample_event("sha1", &["src/a.rs"], "a@x.com");
        e1.title = "corrige el desbordamiento en el parser".to_string();
        e1.body = "detalle largo sin relación".to_string();
        w.add_event(&e1).unwrap();

        let mut e2 = sample_event("sha2", &["src/b.rs"], "a@x.com");
        e2.title = "añade tests".to_string();
        e2.body = "cobertura de casos borde".to_string();
        w.add_event(&e2).unwrap();

        w.commit().unwrap();
    }

    store.rebuild_fts().unwrap();

    let n: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM events_fts WHERE events_fts MATCH 'desbordamiento'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);

    let n2: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM events_fts WHERE events_fts MATCH 'tests'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n2, 1);

    // Llamar dos veces no debe duplicar coincidencias (delete-all antes de rellenar).
    store.rebuild_fts().unwrap();
    let n3: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM events_fts WHERE events_fts MATCH 'tests'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n3, 1);
}

#[test]
fn optimize_no_falla_tras_ingesta_y_fts() {
    let db = TempDb::new("optimize");
    let mut store = Store::open(db.path()).unwrap();
    {
        let mut w = store.writer().unwrap();
        w.add_event(&sample_event("sha1", &["src/a.rs"], "a@x.com")).unwrap();
        w.commit().unwrap();
    }
    store.rebuild_fts().unwrap();
    store.optimize().unwrap();

    let events: i64 = store.conn().query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
    assert_eq!(events, 1);
}

#[test]
fn reset_borra_datos_pero_conserva_meta() {
    let db = TempDb::new("reset");
    let mut store = Store::open(db.path()).unwrap();
    store.set_meta("git_version", "2.45.0").unwrap();
    {
        let mut w = store.writer().unwrap();
        w.add_event(&sample_event("sha1", &["src/a.rs"], "a@x.com")).unwrap();
        w.commit().unwrap();
    }
    store.rebuild_fts().unwrap();

    store.reset().unwrap();

    let conn = store.conn();
    for table in ["events", "touches", "entities", "actors", "links", "labels"] {
        let n: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "{table} debería quedar vacía tras reset");
    }
    // meta se conserva.
    assert_eq!(store.meta("git_version").unwrap().as_deref(), Some("2.45.0"));
    assert_eq!(store.meta("schema_version").unwrap().as_deref(), Some("2"));
}

#[test]
fn open_con_schema_version_distinta_devuelve_err() {
    let db = TempDb::new("bad_version");
    {
        let store = Store::open(db.path()).unwrap();
        store.set_meta("schema_version", "99").unwrap();
    }

    let err = Store::open(db.path());
    assert!(err.is_err(), "abrir con schema_version incompatible debe fallar");
}
