//! Tests de `chrono-metrics` sobre un índice `chrono-store` temporal, con
//! eventos sintéticos controlados (fechas, touches, autores, simhash).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono_core::{Actor, Event, Touch};
use chrono_store::Store;

use crate::{churn, coupling, hotspots, owners, search, similar};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fichero temporal único; borra `.db`, `.db-wal` y `.db-shm` al hacer `Drop`.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir()
            .join(format!("chrono-metrics-test-{name}-{}-{}.db", std::process::id(), n));
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

fn touch(entity: &str, added: i64, deleted: i64) -> Touch {
    Touch {
        entity: entity.to_string(),
        entity_type: "file".to_string(),
        weight: added + deleted,
        attrs: attrs(&[("added", &added.to_string()), ("deleted", &deleted.to_string())]),
    }
}

fn actor(key: &str, name: &str) -> Actor {
    Actor { name: name.to_string(), key: key.to_string() }
}

fn event(
    id: &str,
    at_epoch: i64,
    who: Actor,
    title: &str,
    touches: Vec<Touch>,
    simhash: u64,
    is_bulk: bool,
) -> Event {
    Event {
        id: id.to_string(),
        source_id: "git:/repo".to_string(),
        kind: "commit".to_string(),
        at: format!("1970-01-01T00:00:{at_epoch:02}Z"),
        at_epoch,
        actor: who,
        title: title.to_string(),
        body: format!("cuerpo de {id}"),
        level: String::new(),
        attrs: BTreeMap::new(),
        touches,
        links: vec![],
        simhash,
        is_bulk,
    }
}

/// Construye el índice de fixtures compartido por todos los tests de este
/// módulo. Ver comentario en cada test para la aritmética esperada.
///
/// Entidades bajo "src/": hot.rs (muy tocado), warm.rs, cold.rs, bulk_only.rs.
/// Fuera de "src/": docs/readme.md.
/// Autores: Alice (alice@example.com), Bob (bob@example.com).
/// Ventana de referencia en los tests: since_epoch = 1000 (excluye e4@500).
fn build_fixture() -> (TempDb, Store) {
    let db = TempDb::new("fixture");
    let mut store = Store::open(db.path()).unwrap();

    let alice = actor("alice@example.com", "Alice");
    let bob = actor("bob@example.com", "Bob");

    let e1 = event(
        "e1",
        1000,
        alice.clone(),
        "fix parser overflow",
        vec![touch("src/hot.rs", 10, 2), touch("src/warm.rs", 1, 1)],
        0,
        false,
    );
    let e2 = event(
        "e2",
        2000,
        alice.clone(),
        "improve parser again",
        vec![touch("src/hot.rs", 5, 0)],
        1,
        false,
    );
    let e3 = event(
        "e3",
        3000,
        bob.clone(),
        "add tests for parser",
        vec![touch("src/hot.rs", 2, 2), touch("src/cold.rs", 1, 0)],
        u64::MAX,
        false,
    );
    // Antes de la ventana de referencia (since_epoch=1000): debe quedar fuera.
    let e4 = event("e4", 500, bob.clone(), "old warm work", vec![touch("src/warm.rs", 100, 100)], 123, false);
    // Evento masivo: coupling debe ignorarlo por completo.
    let e5 = event(
        "e5",
        2500,
        alice.clone(),
        "bulk reformat",
        vec![touch("src/hot.rs", 1, 1), touch("src/bulk_only.rs", 1, 1)],
        999,
        true,
    );
    let e6 = event(
        "e6",
        4000,
        bob.clone(),
        "update docs readme",
        vec![touch("docs/readme.md", 3, 1)],
        0x0F0F_0F0F_0F0F_0F0F,
        false,
    );
    let e7 = event(
        "e7",
        3500,
        alice.clone(),
        "fix: parser \"edge case\" handling",
        vec![touch("src/hot.rs", 0, 0)],
        2,
        false,
    );

    {
        let mut w = store.writer().unwrap();
        for ev in [&e1, &e2, &e3, &e4, &e5, &e6, &e7] {
            w.add_event(ev).unwrap();
        }
        w.commit().unwrap();
    }
    store.rebuild_fts().unwrap();

    store.set_entity_size("src/hot.rs", 100).unwrap();
    store.set_entity_size("src/warm.rs", 50).unwrap();
    store.set_entity_size("src/cold.rs", 10).unwrap();
    store.set_entity_size("docs/readme.md", 20).unwrap();
    store.set_entity_size("src/bulk_only.rs", 5).unwrap();

    (db, store)
}

#[test]
fn hotspots_ordena_por_fichero_mas_cambiado_y_respeta_ventana() {
    let (_db, store) = build_fixture();
    let conn = store.conn();

    // Ventana desde 1000: excluye e4 (500), así que warm.rs solo tiene 1 touch.
    let hs = hotspots(conn, 1000, 10).unwrap();
    // hot.rs: 5 touches (e1,e2,e3,e5,e7) * size 100 = raw 500 (máximo -> score 1.0).
    assert_eq!(hs[0].entity, "src/hot.rs");
    assert_eq!(hs[0].changes, 5);
    assert_eq!(hs[0].size, 100);
    assert!((hs[0].score - 1.0).abs() < 1e-9);
    // warm.rs: 1 touch (solo e1) * size 50 = raw 50 -> score 50/500 = 0.1.
    let warm = hs.iter().find(|h| h.entity == "src/warm.rs").unwrap();
    assert_eq!(warm.changes, 1);
    assert!((warm.score - 0.1).abs() < 1e-9);

    // Sin ventana (since_epoch=0): e4 entra y warm.rs pasa a 2 touches.
    let hs_all = hotspots(conn, 0, 10).unwrap();
    let warm_all = hs_all.iter().find(|h| h.entity == "src/warm.rs").unwrap();
    assert_eq!(warm_all.changes, 2);
    // hot.rs sigue siendo el hotspot número uno.
    assert_eq!(hs_all[0].entity, "src/hot.rs");
}

#[test]
fn coupling_devuelve_cocambio_y_excluye_eventos_bulk() {
    let (_db, store) = build_fixture();
    let conn = store.conn();

    let (pairs, total) = coupling(conn, "src/hot.rs", 1, 1000, 10).unwrap();
    // No-bulk touches de hot.rs en la ventana: e1, e2, e3, e7 = 4.
    assert_eq!(total, 4);

    // warm.rs co-cambia con hot.rs solo en e1 -> support=1, confidence=1/4.
    let warm = pairs.iter().find(|p| p.b == "src/warm.rs").expect("warm.rs debería aparecer");
    assert_eq!(warm.support, 1);
    assert!((warm.confidence - 0.25).abs() < 1e-9);

    // cold.rs co-cambia en e3 -> support=1.
    let cold = pairs.iter().find(|p| p.b == "src/cold.rs").expect("cold.rs debería aparecer");
    assert_eq!(cold.support, 1);

    // bulk_only.rs solo co-cambia con hot.rs en el evento e5, que es is_bulk=1:
    // NO debe contar ni aparecer en absoluto.
    assert!(pairs.iter().all(|p| p.b != "src/bulk_only.rs"));
}

#[test]
fn owners_reparte_shares_y_calcula_bus_factor() {
    let (_db, store) = build_fixture();
    let conn = store.conn();

    let (owns, bus_factor) = owners(conn, "src/", 1000).unwrap();
    // Alice: e1(hot+warm)=2, e2(hot)=1, e5(hot+bulk_only)=2, e7(hot)=1 -> 6.
    // Bob:   e3(hot+cold)=2 -> 2. Total=8.
    assert_eq!(owns[0].name, "Alice");
    assert_eq!(owns[0].commits, 6);
    assert!((owns[0].share - 0.75).abs() < 1e-9);
    assert_eq!(owns[1].name, "Bob");
    assert_eq!(owns[1].commits, 2);
    assert!((owns[1].share - 0.25).abs() < 1e-9);
    // acc=0 < (8+1)/2=4 -> cuenta a Alice (acc pasa a 6); 6 < 4 es falso -> para.
    assert_eq!(bus_factor, 1);
}

#[test]
fn churn_suma_added_deleted_desde_json_de_attrs() {
    let (_db, store) = build_fixture();
    let conn = store.conn();

    let rows = churn(conn, 1000, 10).unwrap();
    // hot.rs: (10,2)+(5,0)+(2,2)+(1,1)+(0,0) = added 18, deleted 5.
    let hot = rows.iter().find(|r| r.entity == "src/hot.rs").unwrap();
    assert_eq!(hot.added, 18);
    assert_eq!(hot.deleted, 5);
    assert_eq!(rows[0].entity, "src/hot.rs"); // mayor (added+deleted) primero.

    // warm.rs en la ventana (sin e4): solo (1,1).
    let warm = rows.iter().find(|r| r.entity == "src/warm.rs").unwrap();
    assert_eq!(warm.added, 1);
    assert_eq!(warm.deleted, 1);
}

#[test]
fn churn_ordena_por_added_mas_deleted_no_solo_por_added() {
    // Regresión: el alias `deleted` colisionaba con `entities.deleted` en el
    // ORDER BY y el orden salía solo por `added`. Aquí "mucho_borrado.rs"
    // tiene menos added pero más churn total y debe ir primero.
    let db = TempDb::new("churn-orden");
    let mut store = Store::open(db.path()).unwrap();
    let ana = actor("ana@example.com", "Ana");
    let e = event(
        "c1",
        1000,
        ana,
        "limpieza",
        vec![touch("src/mucho_borrado.rs", 5, 20), touch("src/solo_added.rs", 10, 0)],
        0,
        false,
    );
    let mut w = store.writer().unwrap();
    w.add_event(&e).unwrap();
    w.commit().unwrap();

    let rows = churn(store.conn(), 0, 10).unwrap();
    assert_eq!(rows[0].entity, "src/mucho_borrado.rs");
    assert_eq!((rows[0].added, rows[0].deleted), (5, 20));
    assert_eq!(rows[1].entity, "src/solo_added.rs");
}

#[test]
fn search_encuentra_por_termino_y_no_rompe_con_dos_puntos_y_comillas() {
    let (_db, store) = build_fixture();
    let conn = store.conn();

    let hits = search(conn, "parser", 10).unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().any(|h| h.id == "e1"));

    // Query con ':' y comillas: no debe devolver Err (ni FTS ni el fallback deben romper).
    let tricky = search(conn, "fix: parser \"edge case\"", 10);
    assert!(tricky.is_ok());
}

#[test]
fn similar_encuentra_eventos_cercanos_y_excluye_los_lejanos() {
    let (_db, store) = build_fixture();
    let conn = store.conn();

    // e1.simhash=0; e2=1 (dist 1), e7=2 (dist 1) están cerca; e3=u64::MAX,
    // e4=123, e5=999, e6=0x0F0F... están lejos (todas > 3 bits de distancia).
    let sims = similar(conn, "e1", 3, 10).unwrap();
    let ids: Vec<&str> = sims.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"e2"));
    assert!(ids.contains(&"e7"));
    assert!(!ids.contains(&"e3"));
    assert!(!ids.contains(&"e4"));
    assert!(!ids.contains(&"e5"));
    assert!(!ids.contains(&"e6"));
    // Orden ascendente por distancia.
    for pair in sims.windows(2) {
        assert!(pair[0].distance <= pair[1].distance);
    }
}
