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
fn open_crea_esquema_y_version_actual_y_reabrir_no_falla() {
    let db = TempDb::new("open");

    let store = Store::open(db.path()).expect("open crea el índice");
    assert_eq!(store.meta("schema_version").unwrap().as_deref(), Some("7"));
    drop(store);

    // Reabrir un índice ya existente (mismo schema_version) no debe fallar.
    let store2 = Store::open(db.path()).expect("reabrir no falla");
    assert_eq!(store2.meta("schema_version").unwrap().as_deref(), Some("7"));
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
fn un_evento_repetido_con_otra_entidad_no_suma_touches() {
    let db = TempDb::new("repetido");
    let mut store = Store::open(db.path()).unwrap();
    let mut w = store.writer().unwrap();
    assert!(w.add_event(&sample_event("tool_use:toolu_1", &["Bash"], "a")).unwrap());
    // La misma llamada copiada en otro fichero, sin su uso previo: otra entidad.
    assert!(!w.add_event(&sample_event("tool_use:toolu_1", &["unknown"], "a")).unwrap());
    w.commit().unwrap();
    let touches: i64 = store.conn().query_row("SELECT count(*) FROM touches", [], |r| r.get(0)).unwrap();
    assert_eq!(touches, 1);
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
        store.conn().query_row("SELECT weight FROM touches WHERE event_rid = (SELECT rid FROM events WHERE id = 'sha1')", [], |r| r.get(0)).unwrap();
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
    store.optimize(true).unwrap();
    store.optimize(false).unwrap();

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
    assert_eq!(store.meta("schema_version").unwrap().as_deref(), Some("7"));
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

#[test]
fn open_read_usa_la_cache_pequena_y_sin_mmap() {
    let db = TempDb::new("open-read");
    drop(Store::open(db.path()).unwrap());
    let s = Store::open_read(db.path()).unwrap();
    let pragma = |p: &str| -> i64 { s.conn().query_row(&format!("PRAGMA {p}"), [], |r| r.get(0)).unwrap() };
    assert_eq!(pragma("cache_size"), -16384);
    assert_eq!(pragma("mmap_size"), 0);
    // La de ingesta sigue con su caché grande.
    let w = Store::open(db.path()).unwrap();
    assert_eq!(w.conn().query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0)).unwrap(), -262144);
}

#[test]
fn borrar_eventos_invalida_los_rollups_y_el_incremental_no_suma_dos_veces() {
    let db = TempDb::new("rollups-borrado");
    let mut store = Store::open(db.path()).unwrap();
    let log = |id: &str, source: &str| Event { source_id: source.to_string(), kind: "log".to_string(), level: "ERROR".to_string(), ..sample_event(id, &["svc"], "") };
    {
        let mut w = store.writer().unwrap();
        for (id, src) in [("a1", "jsonl:/a"), ("b1", "jsonl:/b"), ("a2", "jsonl:/a")] {
            w.add_event(&log(id, src)).unwrap();
        }
        w.commit().unwrap();
    }
    store.build_rollups(60_000).unwrap();
    let total = |s: &Store| -> (i64, i64) {
        s.conn().query_row("SELECT (SELECT SUM(count) FROM rollups), (SELECT SUM(count) FROM dim_rollups)", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    };
    assert_eq!(total(&store), (3, 3));
    // Borrar una fuente deja los rollups sin watermark (nadie los da por al día)…
    store.delete_source_events("jsonl:/b").unwrap();
    assert_eq!(store.meta("rollup_max_rowid").unwrap(), None);
    // …y un build incremental sin watermark reconstruye en vez de sumar encima.
    store.build_rollups_sync(60_000).unwrap();
    assert_eq!(total(&store), (2, 2));
    assert_eq!(store.meta("rollup_max_rowid").unwrap().as_deref(), Some("3"));
}

#[test]
fn attr_rollups_solo_desde_un_build_completo_y_al_dia_con_event_dims() {
    let db = TempDb::new("attr-rollups");
    let mut store = Store::open(db.path()).unwrap();
    let add = |store: &mut Store, ids: &[&str], src: &str| {
        let mut w = store.writer().unwrap();
        w.set_dims(vec!["host".into()]);
        for id in ids {
            let e = Event { source_id: src.to_string(), kind: "log".to_string(), attrs: attrs(&[("host", id)]), ..sample_event(id, &["svc"], "") };
            w.add_event(&e).unwrap();
        }
        w.commit().unwrap();
    };
    let suma = |s: &Store| -> i64 { s.conn().query_row("SELECT COALESCE(SUM(count), 0) FROM attr_rollups", [], |r| r.get(0)).unwrap() };
    let dims = |s: &Store| -> i64 { s.conn().query_row("SELECT COUNT(*) FROM event_dims", [], |r| r.get(0)).unwrap() };
    add(&mut store, &["a1", "a2"], "jsonl:/a");
    // Un incremental sin las claves elegidas (un índice de antes) no las construye.
    store.set_meta("rollup_max_rowid", "0").unwrap();
    store.build_rollups_sync(60_000).unwrap();
    assert_eq!(store.meta("attr_rollup_keys").unwrap(), None);
    // El completo elige las claves y cuenta cada evento con la dim.
    store.build_rollups(60_000).unwrap();
    assert_eq!(store.meta("attr_rollup_keys").unwrap().as_deref(), Some("host"));
    assert_eq!((suma(&store), dims(&store)), (2, 2));
    // Desde ahí, el incremental suma lo nuevo y quitar una fuente quita lo suyo.
    add(&mut store, &["b1"], "jsonl:/b");
    store.build_rollups_sync(60_000).unwrap();
    assert_eq!((suma(&store), dims(&store)), (3, 3));
    store.remove_source("jsonl:/a").unwrap();
    assert_eq!((suma(&store), dims(&store)), (1, 1));
    store.reset().unwrap();
    assert_eq!(suma(&store), 0);
}

#[test]
fn un_indice_v6_migra_sus_links_a_targets_internados() {
    let db = TempDb::new("v6-links");
    {
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta VALUES ('schema_version', '6');
             CREATE TABLE links(event_rid INTEGER NOT NULL, rel TEXT NOT NULL, target TEXT NOT NULL,
                                PRIMARY KEY (event_rid, rel, target)) WITHOUT ROWID;
             CREATE INDEX idx_links_rel ON links(rel, target);
             INSERT INTO links VALUES (1, 'tool_use', 'toolu_a'), (2, 'tool_use', 'toolu_a'), (2, 'file', 'x.rs'), (3, 'file', 'x.rs');",
        )
        .unwrap();
    }
    let store = Store::open(db.path()).unwrap();
    assert_eq!(store.meta("schema_version").unwrap().as_deref(), Some("7"));
    let c = store.conn();
    let n = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(n("SELECT COUNT(*) FROM link_targets"), 2);
    assert_eq!(n("SELECT COUNT(*) FROM event_links"), 4);
    // La vista conserva la forma de antes.
    assert_eq!(n("SELECT COUNT(*) FROM links WHERE rel = 'tool_use' AND target = 'toolu_a'"), 2);
    assert_eq!(n("SELECT COUNT(*) FROM links WHERE event_rid = 2"), 2);
    assert_eq!(n("SELECT COUNT(*) FROM sqlite_master WHERE name = 'links_v6'"), 0, "la tabla vieja no queda");
    assert_eq!(n("SELECT COUNT(*) FROM sqlite_master WHERE type = 'view' AND name = 'links'"), 1);
    assert_eq!(n("SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name IN ('idx_event_links_target', 'idx_link_targets_target')"), 2);
    drop(store);
    // Reabrir no vuelve a migrar.
    Store::open(db.path()).unwrap();
}

/// Varios procesos (aquí hilos con su propia conexión) abren a la vez el mismo
/// índice v6: uno migra y los demás esperan y siguen, sin error.
#[test]
fn la_migracion_v6_con_aperturas_simultaneas_no_falla() {
    let db = TempDb::new("v6-a-la-vez");
    {
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta VALUES ('schema_version', '6');
             CREATE TABLE links(event_rid INTEGER NOT NULL, rel TEXT NOT NULL, target TEXT NOT NULL,
                                PRIMARY KEY (event_rid, rel, target)) WITHOUT ROWID;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000)
             INSERT INTO links SELECT i, 'file', 'f' || (i % 5000) FROM n;",
        )
        .unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let (path, barrier) = (db.path().to_path_buf(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                Store::open(&path).map(|_| ()).map_err(|e| e.to_string())
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap().expect("las aperturas que no migran esperan a la que migra y siguen");
    }
    let store = Store::open(db.path()).unwrap();
    assert_eq!(store.meta("schema_version").unwrap().as_deref(), Some("7"));
    let n: i64 = store.conn().query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 200_000);
}

/// Esperar a otra migración más de la cuenta da un error que lo dice, no un
/// «database is locked» a secas.
#[test]
fn la_espera_de_otra_migracion_agotada_lo_dice() {
    let db = TempDb::new("v6-espera");
    let conn = rusqlite::Connection::open(db.path()).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta VALUES ('schema_version', '6');
         CREATE TABLE links(event_rid INTEGER NOT NULL, rel TEXT NOT NULL, target TEXT NOT NULL,
                            PRIMARY KEY (event_rid, rel, target)) WITHOUT ROWID;
         BEGIN IMMEDIATE;",
    )
    .unwrap();
    crate::store::MIGRATION_WAIT_MS.with(|w| w.set(300));
    let err = Store::open(db.path()).err().expect("con la migración de otro en curso no abre").to_string();
    assert!(err.contains("otro proceso sigue migrando"), "{err}");
    conn.execute_batch("ROLLBACK").unwrap();
}

/// Varias aperturas a la vez sobre una ruta nueva: una crea el esquema y las
/// demás lo ven entero (antes alguna veía `meta` sin versión: «esquema ?»).
#[test]
fn aperturas_simultaneas_sobre_una_ruta_nueva_no_fallan() {
    for round in 0..5 {
        let db = TempDb::new(&format!("nueva-a-la-vez-{round}"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (path, barrier) = (db.path().to_path_buf(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    Store::open(&path).map(|_| ()).map_err(|e| e.to_string())
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().expect("ninguna apertura ve el esquema a medias");
        }
    }
}

/// Una consulta sobre una ruta sin índice falla diciéndolo y no crea un `.db` vacío.
#[test]
fn open_read_sin_indice_no_crea_nada() {
    let db = TempDb::new("sin-indice");
    let err = Store::open_read(db.path()).err().expect("sin índice no abre").to_string();
    assert!(err.contains("no hay índice"), "{err}");
    assert!(!db.path().exists());
}

/// Eventos de log de `src`, uno por id, con la dim `file`. Devuelve sus rid.
fn add_logs(store: &mut Store, src: &str, file: &str, ids: &[&str]) -> Vec<i64> {
    let mut w = store.writer().unwrap();
    w.set_dims(vec!["file".to_string()]);
    for id in ids {
        let ev = Event {
            source_id: src.to_string(),
            kind: "log".to_string(),
            attrs: attrs(&[("file", file)]),
            touches: vec![],
            links: vec![],
            ..sample_event(id, &[], "")
        };
        assert!(w.add_event(&ev).unwrap());
    }
    w.commit().unwrap();
    ids.iter().map(|id| store.conn().query_row("SELECT rid FROM events WHERE id = ?1", [id], |r| r.get(0)).unwrap()).collect()
}

#[test]
fn los_rid_de_lo_borrado_no_se_reutilizan() {
    let db = TempDb::new("rid-hwm");
    let mut store = Store::open(db.path()).unwrap();
    assert_eq!(add_logs(&mut store, "jsonl:/a", "a", &["a1", "a2", "a3"]), [1, 2, 3]);
    // Borrar los rid más altos (relectura de una fuente): antes lo nuevo volvía a
    // tener 1, 2, 3 y quien sigue por rid (`tail`) no lo veía.
    store.delete_source_events("jsonl:/a").unwrap();
    assert_eq!(add_logs(&mut store, "jsonl:/a", "a", &["b1", "b2"]), [4, 5]);
    // Igual con un fichero de una glob y tras `reset`.
    store.delete_source_file_events("jsonl:/a", "a").unwrap();
    assert_eq!(add_logs(&mut store, "jsonl:/a", "a", &["c1"]), [6]);
    store.reset().unwrap();
    assert_eq!(add_logs(&mut store, "jsonl:/a", "a", &["d1"]), [7]);
    // Un índice sin la marca (de antes) sigue por encima de su máximo.
    store.conn().execute("DELETE FROM meta WHERE key = 'rid_hwm'", []).unwrap();
    assert_eq!(add_logs(&mut store, "jsonl:/a", "a", &["e1"]), [8]);
    // Un evento repetido no gasta rid.
    let mut w = store.writer().unwrap();
    let dup = Event { source_id: "jsonl:/a".into(), kind: "log".into(), touches: vec![], links: vec![], ..sample_event("e1", &[], "") };
    assert!(!w.add_event(&dup).unwrap());
    w.commit().unwrap();
    assert_eq!(add_logs(&mut store, "jsonl:/a", "a", &["f1"]), [9]);
}

#[test]
fn la_marca_stale_dims_no_cuenta_lo_que_entra_tras_un_borrado() {
    let db = TempDb::new("stale-dims-rid");
    let mut store = Store::open(db.path()).unwrap();
    add_logs(&mut store, "jsonl:/g", "viejo", &["v1"]);
    add_logs(&mut store, "jsonl:/g", "b", &["b1", "b2"]);
    // Cambian las dims sin poder releer: lo que hay hasta hoy queda con las de antes.
    store.set_source_dims("jsonl:/g", &["file".to_string()]).unwrap();
    // b se relee (sus rid, los más altos, se borran) y entra con las dims nuevas.
    store.delete_source_file_events("jsonl:/g", "b").unwrap();
    add_logs(&mut store, "jsonl:/g", "b", &["b1", "b2"]);
    let stale: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE source_ref = (SELECT ref FROM sources WHERE id = ?1)
               AND rid <= CAST((SELECT value FROM meta WHERE key = 'stale_dims:' || ?1) AS INTEGER)",
            ["jsonl:/g"],
            |r| r.get(0),
        )
        .unwrap();
    // Antes: 3 (los b releídos reutilizaban los rid 2 y 3, bajo la marca).
    assert_eq!(stale, 1);
}
