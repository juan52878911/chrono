//! Tests del dispatch JSON-RPC (`handle`) sin proceso real: se le pasan
//! líneas JSON tal cual llegarían por stdin y se inspecciona la respuesta.
//! El fixture de índice reutiliza el mismo patrón que
//! `chrono-metrics/src/tests.rs` (eventos sintéticos sobre un `Store`
//! temporal).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono_core::{Actor, Event, Touch};
use chrono_store::Store;
use serde_json::{json, Value};

use crate::{handle, State};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fichero temporal único; borra `.db`, `.db-wal` y `.db-shm` al hacer `Drop`.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "chrono-mcp-test-{name}-{}-{}.db",
            std::process::id(),
            n
        ));
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

fn touch(entity: &str, added: i64, deleted: i64) -> Touch {
    Touch {
        entity: entity.to_string(),
        entity_type: "file".to_string(),
        weight: added + deleted,
        attrs: [
            ("added", added.to_string()),
            ("deleted", deleted.to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
    }
}

fn actor(key: &str, name: &str) -> Actor {
    Actor {
        name: name.to_string(),
        key: key.to_string(),
    }
}

fn event(id: &str, at_epoch: i64, who: Actor, title: &str, touches: Vec<Touch>) -> Event {
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
        simhash: 0,
        is_bulk: false,
    }
}

/// Índice con un puñado de commits sobre "src/hot.rs" (bastante tocado para
/// que salga como primer hotspot).
fn build_fixture() -> (TempDb, Store) {
    let db = TempDb::new("fixture");
    let mut store = Store::open(db.path()).unwrap();
    let alice = actor("alice@example.com", "Alice");

    let e1 = event(
        "e1",
        1000,
        alice.clone(),
        "fix parser overflow",
        vec![touch("src/hot.rs", 10, 2)],
    );
    let e2 = event(
        "e2",
        2000,
        alice.clone(),
        "improve parser again",
        vec![touch("src/hot.rs", 5, 0)],
    );
    let e3 = event(
        "e3",
        3000,
        alice,
        "add tests for parser",
        vec![touch("src/hot.rs", 2, 2)],
    );

    {
        let mut w = store.writer().unwrap();
        for ev in [&e1, &e2, &e3] {
            w.add_event(ev).unwrap();
        }
        w.commit().unwrap();
    }
    store.rebuild_fts().unwrap();
    store.set_entity_size("src/hot.rs", 100).unwrap();

    (db, store)
}

fn state_with_fixture() -> (TempDb, State) {
    let (db, store) = build_fixture();
    (db, State::with_store(Some(store)))
}

fn call(state: &State, id: i64, method: &str, params: Value) -> Value {
    let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let raw = handle(&req.to_string(), state).expect("se esperaba una respuesta");
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn initialize_devuelve_protocol_version_y_server_info() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 1, "initialize", json!({}));
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(resp["result"]["serverInfo"]["name"], "chrono");
    assert!(resp["result"]["serverInfo"]["version"].is_string());
    assert!(resp["result"]["capabilities"]["tools"].is_object());
}

#[test]
fn notifications_initialized_no_responde() {
    let (_db, state) = state_with_fixture();
    let req = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert!(handle(&req.to_string(), &state).is_none());
}

#[test]
fn ping_responde_objeto_vacio() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 2, "ping", json!({}));
    assert_eq!(resp["result"], json!({}));
}

#[test]
fn tools_list_incluye_los_catorce_tools() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 3, "tools/list", json!({}));
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools debe ser array");
    assert_eq!(tools.len(), 14);

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "hotspots", "coupling", "owners", "churn", "search", "similar", "bugs", "tickets",
        "phases", "branches", "prs", "timeline", "top", "correlate",
    ] {
        assert!(
            names.contains(&expected),
            "falta el tool {expected:?} en tools/list: {names:?}"
        );
    }

    // Cada tool trae su JSON-schema de input.
    let coupling = tools.iter().find(|t| t["name"] == "coupling").unwrap();
    assert_eq!(coupling["inputSchema"]["type"], "object");
    assert!(coupling["inputSchema"]["properties"]["file"].is_object());
    assert_eq!(coupling["inputSchema"]["required"], json!(["file"]));
}

#[test]
fn tools_call_hotspots_devuelve_entidades_con_datos() {
    let (_db, state) = state_with_fixture();
    let resp = call(
        &state,
        4,
        "tools/call",
        json!({"name": "hotspots", "arguments": {}}),
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text debe ser texto");
    let payload: Value = serde_json::from_str(text).unwrap();
    let hotspots = payload["hotspots"]
        .as_array()
        .expect("hotspots debe ser array");
    assert!(!hotspots.is_empty());
    assert_eq!(hotspots[0]["entity"], "src/hot.rs");
    assert_eq!(hotspots[0]["changes"], 3);
    assert_eq!(hotspots[0]["size"], 100);
}

#[test]
fn tools_call_sin_indice_pide_chrono_init() {
    let state = State::with_store(None);
    let resp = call(
        &state,
        5,
        "tools/call",
        json!({"name": "hotspots", "arguments": {}}),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("chrono init"),
        "texto sin índice no menciona 'chrono init': {text:?}"
    );

    // tools/list debe seguir funcionando igual sin índice.
    let list = call(&state, 6, "tools/list", json!({}));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 14);
}

#[test]
fn tools_call_sin_argumento_requerido_devuelve_error_jsonrpc() {
    let (_db, state) = state_with_fixture();
    let resp = call(
        &state,
        7,
        "tools/call",
        json!({"name": "coupling", "arguments": {}}),
    );
    assert_eq!(resp["error"]["code"], -32000);
    assert!(resp["result"].is_null());
}

#[test]
fn metodo_desconocido_devuelve_error_jsonrpc() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 8, "no/existe", json!({}));
    assert_eq!(resp["error"]["code"], -32601);
    assert!(resp["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no/existe"));
}

#[test]
fn metodo_desconocido_sin_id_no_responde() {
    let (_db, state) = state_with_fixture();
    let req = json!({"jsonrpc": "2.0", "method": "no/existe"});
    assert!(handle(&req.to_string(), &state).is_none());
}

#[test]
fn linea_json_invalida_se_ignora() {
    let (_db, state) = state_with_fixture();
    assert!(handle("esto no es json", &state).is_none());
    assert!(handle("", &state).is_none());
}
