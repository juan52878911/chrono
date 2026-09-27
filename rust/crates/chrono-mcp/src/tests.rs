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
fn tools_list_incluye_los_dieciseis_tools() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 3, "tools/list", json!({}));
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools debe ser array");
    assert_eq!(tools.len(), 16);

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "hotspots", "coupling", "owners", "churn", "search", "similar", "bugs", "tickets",
        "phases", "branches", "prs", "timeline", "top", "correlate", "patterns", "repos",
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
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 16);
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

// --- Multi-repo: argumento `repo` (docs/CONTRACTS.md) ---

/// Directorio temporal que se borra al hacer `Drop`.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("chrono-mcp-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `<dir>` con `.git/HEAD` en `branch` e índice real en `.chrono/index.db`:
/// dos commits que tocan los mismos ficheros (acoplados entre sí).
fn repo_with_index(dir: &Path, branch: &str, files: &[&str]) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
    std::fs::create_dir_all(dir.join(".chrono")).unwrap();
    let mut store = Store::open(&dir.join(".chrono/index.db")).unwrap();
    store.set_meta("repo_path", &dir.to_string_lossy()).unwrap();
    let who = actor("ana@x", "Ana");
    {
        let mut w = store.writer().unwrap();
        for (i, id) in ["a1", "a2"].iter().enumerate() {
            let t: Vec<Touch> = files.iter().map(|f| touch(f, 3, 1)).collect();
            w.add_event(&event(id, 1000 + i as i64, who.clone(), &format!("fix cosa {id}"), t)).unwrap();
        }
        w.commit().unwrap();
    }
}

fn payload(resp: &Value) -> (Value, bool) {
    let text = resp["result"]["content"][0]["text"].as_str().expect("content[0].text");
    let is_err = resp["result"]["isError"].as_bool().unwrap_or(false);
    (serde_json::from_str(text).unwrap_or_else(|_| json!({"text": text})), is_err)
}

fn tool_call(state: &State, name: &str, args: Value) -> (Value, bool) {
    payload(&call(state, 1, "tools/call", json!({"name": name, "arguments": args})))
}

#[test]
fn repo_por_nombre_etiqueta_ruta_y_errores() {
    let tmp = TempDir::new("multi");
    let root = tmp.0.join("repos");
    repo_with_index(&root.join("alpha"), "main", &["alpha.rs", "shared.rs"]);
    repo_with_index(&root.join("beta"), "dev", &["beta.rs", "other.rs"]);
    std::fs::create_dir_all(root.join("gamma/.git")).unwrap();
    std::fs::write(root.join("gamma/.git/HEAD"), "ref: refs/heads/main\n").unwrap();

    let def = Store::open(&root.join("alpha/.chrono/index.db")).unwrap();
    let state = State::new(Some(def), Some(root.clone()));

    // Sin repo: el de siempre, etiquetado.
    let (out, _) = tool_call(&state, "hotspots", json!({}));
    assert_eq!(out["repo"], "alpha@main");
    assert!(!out["hotspots"].as_array().unwrap().is_empty());

    // Por nombre y por nombre@rama: otro índice, otras respuestas.
    let (out, _) = tool_call(&state, "churn", json!({"repo": "beta"}));
    assert_eq!(out["repo"], "beta@dev");
    assert!(out.to_string().contains("beta.rs"), "{out}");
    let (out, _) = tool_call(&state, "hotspots", json!({"repo": "beta@dev"}));
    assert_eq!(out["repo"], "beta@dev");

    // Ruta absoluta.
    let (out, _) = tool_call(&state, "hotspots", json!({"repo": root.join("beta").to_string_lossy()}));
    assert_eq!(out["repo"], "beta@dev");

    // Sin índice: error claro con la orden de init.
    let (out, is_err) = tool_call(&state, "hotspots", json!({"repo": "gamma"}));
    assert!(is_err);
    let msg = out["text"].as_str().unwrap();
    assert!(msg.contains(&format!("chrono init {}", root.join("gamma").display())), "{msg}");

    // Rama que no está en disco, y repo que no existe.
    assert!(tool_call(&state, "hotspots", json!({"repo": "beta@main"})).1);
    assert!(tool_call(&state, "hotspots", json!({"repo": "nope"})).1);

    // path#symbol: el símbolo se ignora en respuestas a nivel de fichero.
    let (out, _) = tool_call(&state, "coupling", json!({"repo": "alpha", "file": "alpha.rs#parse"}));
    assert_eq!(out["for"], "alpha.rs");
    assert_eq!(out["coupled"][0]["b"], "shared.rs");
    let abs = format!("{}#X", root.join("alpha/alpha.rs").display());
    let (out, _) = tool_call(&state, "owners", json!({"path": abs}));
    assert_eq!(out["path"], "alpha.rs");
    assert_eq!(out["bus_factor"], 1);

    // repos: lista lo que hay bajo la raíz.
    let (out, _) = tool_call(&state, "repos", json!({}));
    let names: Vec<String> = out["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| format!("{}:{}", e["repo"].as_str().unwrap(), e["indexed"]))
        .collect();
    assert_eq!(names, vec!["alpha@main:true", "beta@dev:true", "gamma@main:false"]);
}

#[test]
fn repo_sin_raiz_pide_configurarla() {
    let state = State::new(None, None);
    let (out, is_err) = tool_call(&state, "hotspots", json!({"repo": "x"}));
    assert!(is_err);
    assert!(out["text"].as_str().unwrap().contains("CHRONO_REPOS"));
}

#[test]
fn todos_los_tools_aceptan_repo() {
    for t in crate::tools::tool_defs() {
        if t["name"] == "repos" {
            continue;
        }
        assert!(t["inputSchema"]["properties"]["repo"].is_object(), "{} sin repo", t["name"]);
    }
}
