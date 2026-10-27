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
fn tools_list_incluye_todos_los_tools() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 3, "tools/list", json!({}));
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools debe ser array");
    assert_eq!(tools.len(), 25);

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "hotspots", "coupling", "owners", "churn", "search", "similar", "bugs", "tickets",
        "phases", "branches", "explain", "prs", "timeline", "top", "correlate", "patterns", "events", "trace",
        "session_patterns", "sources", "stats", "precede", "gaps", "informe", "delega",
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
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 25);
}

#[test]
fn tools_call_sin_argumento_requerido_devuelve_is_error() {
    let (_db, state) = state_with_fixture();
    let resp = call(
        &state,
        7,
        "tools/call",
        json!({"name": "coupling", "arguments": {}}),
    );
    // Error de la herramienta: resultado con `isError`, no error JSON-RPC.
    assert!(resp["error"].is_null(), "{resp}");
    assert_eq!(resp["result"]["isError"], true, "{resp}");
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap_or("").contains("'file'"), "{resp}");
    // Sin `name` sí es un error de protocolo.
    let resp = call(&state, 9, "tools/call", json!({"arguments": {}}));
    assert_eq!(resp["error"]["code"], -32000, "{resp}");
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

#[test]
fn tools_call_session_patterns_sin_eventos_nod_avisa() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 9, "tools/call", json!({"name": "session_patterns", "arguments": {}}));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{resp}"));
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["basis"]["events_scanned"], 0);
    assert_eq!(payload["sessions_total"], 0);
    assert_eq!(payload["basis"]["hotspots"][0], "src/hot.rs");
    assert!(payload["notes"][0].as_str().unwrap().contains("no Nod events"));
}

#[test]
fn consultas_de_eventos_exponen_los_filtros_comunes() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 30, "tools/list", json!({}));
    let tools = resp["result"]["tools"].as_array().unwrap();
    for name in ["top", "patterns", "timeline", "events", "stats", "precede"] {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        for prop in ["since", "until", "level", "entity", "source", "kind", "grep", "attr", "join"] {
            assert!(t["inputSchema"]["properties"].get(prop).is_some(), "{name} no expone '{prop}'");
        }
    }
}

#[test]
fn initialize_trae_instrucciones_de_cuando_usar_chrono() {
    let (_db, state) = state_with_fixture();
    let resp = call(&state, 31, "initialize", json!({}));
    let text = resp["result"]["instructions"].as_str().unwrap();
    assert!(text.contains("sources") && text.contains("level"), "{text}");
}

/// Pruebas P1-P3 en una fuente y el resultado de P1 en otra, unidos por el link
/// `prueba`; las pruebas son ERROR separadas una hora (tres episodios). El
/// resultado de P2 es ANTERIOR a la prueba: solo cubre con `any_order`.
fn relation_state() -> (TempDb, State) {
    let db = TempDb::new("relaciones");
    let mut store = Store::open(db.path()).unwrap();
    let mk = |id: &str, src: &str, secs: i64, level: &str, key: &str| Event {
        id: id.into(),
        source_id: src.into(),
        kind: "log".into(),
        at_epoch: secs * 1000,
        level: level.into(),
        title: format!("{id} falla"),
        links: vec![chrono_core::Link { rel: "prueba".into(), target: key.into() }],
        ..Default::default()
    };
    {
        let mut w = store.writer().unwrap();
        for (i, k) in ["P1", "P2", "P3"].iter().enumerate() {
            w.add_event(&mk(&format!("p{i}"), "jsonl:/pruebas", 3600 * (i as i64 + 1), "ERROR", k)).unwrap();
        }
        w.add_event(&mk("r0", "jsonl:/resultados", 3700, "", "P1")).unwrap();
        w.add_event(&mk("r1", "jsonl:/resultados", 100, "", "P2")).unwrap();
        w.commit().unwrap();
    }
    (db, State::with_store(Some(store)))
}

fn tool_json(state: &State, name: &str, args: Value) -> Value {
    let resp = call(state, 7, "tools/call", json!({"name": name, "arguments": args}));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{resp}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}: {text}"))
}

#[test]
fn tools_call_gaps_correlate_via_y_precede_con_cifras_de_todo() {
    let (_db, state) = relation_state();
    let g = tool_json(&state, "gaps", json!({"from": "jsonl:/pruebas", "to": "jsonl:/resultados", "via": "prueba", "limit": 1}));
    assert_eq!((g["units"].as_i64(), g["covered"].as_i64(), g["missing"].as_i64()), (Some(3), Some(1), Some(2)), "{g}");
    assert_eq!(g["missing_sample"].as_array().unwrap().len(), 1, "limit solo acota la muestra: {g}");
    assert_eq!(g["order"], "to_after_first_from", "{g}");
    let g = tool_json(&state, "gaps", json!({"from": "jsonl:/pruebas", "to": "jsonl:/resultados", "via": "prueba", "any_order": true}));
    assert_eq!((g["covered"].as_i64(), g["order"].as_str()), (Some(2), Some("any")), "{g}");
    let c = tool_json(&state, "correlate", json!({"id": "p0", "via": "prueba"}));
    assert_eq!(c["related_total"], 1, "{c}");
    assert_eq!(c["related"][0]["id"], "r0", "{c}");
    let p = tool_json(&state, "precede", json!({"level": "ERROR", "limit": 1, "format": "json"}));
    assert_eq!((p["episodes_total"].as_i64(), p["episodes_returned"].as_i64()), (Some(3), Some(1)), "{p}");
    let e = tool_json(&state, "events", json!({"join": "prueba=P1", "format": "json"}));
    let ids: Vec<&str> = e["events"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["p0", "r0"], "{e}");
    // order=desc: lo más reciente primero.
    let e = tool_json(&state, "events", json!({"order": "desc", "limit": 2, "format": "json"}));
    let ids: Vec<&str> = e["events"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["p2", "p1"], "{e}");
    // Una clave de join que no existe: error, no vacío.
    let resp = call(&state, 8, "tools/call", json!({"name": "events", "arguments": {"join": "pruebaa=P1"}}));
    assert_eq!(resp["result"]["isError"], true, "{resp}");
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap_or("").contains("clave de relación desconocida"), "{resp}");
}

/// Un log con tres líneas con tiempo (una ERROR) y una ERROR sin tiempo, con
/// la attr `container` solo en dos eventos.
fn logs_state() -> (TempDb, State) {
    let db = TempDb::new("logs");
    let mut store = Store::open(db.path()).unwrap();
    let mk = |id: &str, secs: i64, level: &str, container: Option<&str>| Event {
        id: id.into(),
        source_id: "jsonl:/app.jsonl".into(),
        kind: "log".into(),
        at_epoch: secs * 1000,
        level: level.into(),
        title: format!("{id} pasa"),
        attrs: container
            .map(|c| [("container".to_string(), c.to_string()), ("ms".to_string(), format!("{}", secs / 360))].into_iter().collect())
            .unwrap_or_default(),
        ..Default::default()
    };
    {
        let mut w = store.writer().unwrap();
        w.add_event(&mk("a", 3600, "ERROR", Some("db-1"))).unwrap();
        w.add_event(&mk("b", 7200, "INFO", Some("db-2"))).unwrap();
        w.add_event(&mk("c", 0, "ERROR", None)).unwrap();
        w.add_event(&mk("d", 7300, "INFO", None)).unwrap();
        w.commit().unwrap();
    }
    (db, State::with_store(Some(store)))
}

fn tool_text(state: &State, name: &str, args: Value) -> String {
    let resp = call(state, 7, "tools/call", json!({"name": name, "arguments": args}));
    resp["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{resp}")).to_string()
}

#[test]
fn timeline_total_cuadra_y_dice_los_eventos_sin_tiempo() {
    let (_db, state) = logs_state();
    let t = tool_json(&state, "timeline", json!({"format": "json"}));
    let sum: i64 = t["buckets"].as_array().unwrap().iter().map(|b| b["count"].as_i64().unwrap()).sum();
    assert_eq!(t["meta"]["total"].as_i64(), Some(sum), "{t}");
    assert_eq!(t["untimed"], 1, "{t}");
    let text = tool_text(&state, "timeline", json!({}));
    assert!(text.contains("⚠ 1 eventos que casan con los filtros no tienen tiempo"), "{text}");
    // Con un filtro que no casa la línea sin tiempo, no se dice.
    let text = tool_text(&state, "timeline", json!({"level": "INFO"}));
    assert!(!text.contains("sin tiempo") && !text.contains("no tienen tiempo"), "{text}");
}

#[test]
fn top_attr_y_level_que_no_existen_avisan() {
    let (_db, state) = logs_state();
    let text = tool_text(&state, "top", json!({"dim": "attr:contenedor"}));
    assert!(text.contains("⚠ ningún evento tiene la attr \"contenedor\"; las que hay: container, ms"), "{text}");
    let text = tool_text(&state, "events", json!({"level": "eror"}));
    assert!(text.contains("⚠ ningún evento tiene el nivel eror; los que hay: ERROR,INFO"), "{text}");
    // Un nivel que existe no avisa; con infer_level, los deducibles tampoco.
    let text = tool_text(&state, "top", json!({"dim": "kind", "level": "error"}));
    assert!(!text.contains("⚠"), "{text}");
    let text = tool_text(&state, "top", json!({"dim": "kind", "level": "FATAL", "infer_level": true}));
    assert!(!text.contains("ningún evento tiene el nivel"), "{text}");
}

#[test]
fn top_attr_resto_solo_con_la_attr_y_sin_valor_aparte() {
    let (_db, state) = logs_state();
    let t = tool_json(&state, "top", json!({"dim": "attr:container", "limit": 1, "format": "json"}));
    let rows: Vec<(String, i64)> =
        t["top"].as_array().unwrap().iter().map(|r| (r["value"].as_str().unwrap().to_string(), r["count"].as_i64().unwrap())).collect();
    // 3 con tiempo: a y b con container, d sin él.
    assert_eq!(rows[1..], [("(resto)".to_string(), 1), ("(sin valor)".to_string(), 1)], "{t}");
    let t = tool_json(&state, "top", json!({"dim": "level", "limit": 1, "format": "json"}));
    let last = t["top"].as_array().unwrap().last().unwrap().clone();
    assert_eq!((last["value"].as_str(), last["count"].as_i64()), (Some("(resto)"), Some(1)), "{t}");
}

#[test]
fn stats_filtra_con_where_attr_y_explica_el_vacio() {
    let (_db, state) = logs_state();
    let t = tool_json(&state, "stats", json!({"attr": "ms", "where_attr": "container:db-2", "format": "json"}));
    assert_eq!(t["groups"][0]["count"], 1, "{t}");
    assert_eq!(t["groups"][0]["max"], 20.0, "{t}");
    assert_eq!(t["meta"]["total"], 1, "{t}");
    let text = tool_text(&state, "stats", json!({"attr": "latencia"}));
    assert!(text.contains("⚠ ningún evento tiene la attr \"latencia\""), "{text}");
    let text = tool_text(&state, "stats", json!({"attr": "container"}));
    assert!(text.contains("⚠ la attr \"container\" no tiene valores numéricos"), "{text}");
}

#[test]
fn correlate_de_una_sola_fuente_sugiere_via_y_hotspots_sin_git_avisa() {
    let (_db, state) = logs_state();
    let c = tool_json(&state, "correlate", json!({"id": "a"}));
    assert_eq!(c["related"], json!([]), "{c}");
    assert!(c["hints"][0].as_str().unwrap().contains("via="), "{c}");
    let h = tool_json(&state, "hotspots", json!({}));
    assert_eq!(h["hotspots"], json!([]), "{h}");
    assert!(h["hints"][0].as_str().unwrap().contains("fuente git"), "{h}");
    // Con git, como siempre y sin aviso.
    let (_db2, git) = state_with_fixture();
    let h = tool_json(&git, "hotspots", json!({}));
    assert_eq!(h["hotspots"][0]["entity"], "src/hot.rs", "{h}");
    assert!(h.get("hints").is_none(), "{h}");
}
