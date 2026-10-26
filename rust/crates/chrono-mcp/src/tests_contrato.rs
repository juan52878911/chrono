//! La cabecera de contrato (`total`, `truncated`) de cada herramienta que la
//! lleva tiene que cuadrar con lo que devuelve: un `total=0` con grupos, o un
//! total menor que lo listado, hace que el modelo cite cifras falsas.

use std::path::PathBuf;

use chrono_core::{Event, Touch};
use chrono_store::Store;
use serde_json::{json, Value};

struct TempDb(PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

/// 30 logs de dos servicios, uno de cada tres ERROR, con `dur` numérico y la
/// dim declarada `cwd`.
fn fixture(name: &str) -> (TempDb, Store) {
    let db = TempDb(std::env::temp_dir().join(format!("chrono-mcp-test-contrato-{name}-{}.db", std::process::id())));
    let _ = std::fs::remove_file(&db.0);
    let mut store = Store::open(&db.0).unwrap();
    let mut w = store.writer().unwrap();
    w.set_dims(vec!["cwd".into()]);
    for i in 0..30i64 {
        let at_epoch = 1_700_000_000_000 + i * 60_000;
        let svc = if i % 2 == 0 { "svc-a" } else { "svc-b" };
        let mut e = Event {
            id: format!("e{i}"),
            source_id: "jsonl:/app".into(),
            kind: "log".into(),
            at: crate::contract::iso(at_epoch),
            at_epoch,
            level: if i % 3 == 0 { "ERROR" } else { "INFO" }.into(),
            title: if i % 3 == 0 { format!("fallo al conectar {i}") } else { format!("petición servida {i}") },
            touches: vec![Touch { entity: svc.into(), entity_type: "service".into(), weight: 1, attrs: Default::default() }],
            ..Default::default()
        };
        e.attrs.insert("dur".into(), (i % 7).to_string());
        e.attrs.insert("cwd".into(), format!("/p{}", i % 3));
        w.add_event(&e).unwrap();
    }
    w.commit().unwrap();
    store.rebuild_fts().unwrap();
    // Como `chrono init`/`sync`: patterns y top salen de los rollups.
    store.build_rollups(60_000).unwrap();
    (db, store)
}

fn call(store: &Store, tool: &str, mut args: Value) -> Value {
    args["format"] = json!("json");
    let text = crate::run_tool_text(store, tool, &args).unwrap_or_else(|e| panic!("{tool}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{tool}: {e}: {text}"))
}

fn sum(v: &Value, list: &str, field: &str) -> i64 {
    v[list].as_array().unwrap_or_else(|| panic!("sin {list}: {v}")).iter().map(|x| x[field].as_i64().unwrap_or(0)).sum()
}

type Caso = (&'static str, Value, fn(&Value) -> i64);

#[test]
fn cabecera_cuadra_con_lo_devuelto_en_todas_las_herramientas() {
    let (_db, store) = fixture("todas");
    // (herramienta, argumentos, lo que debe sumar exactamente `total`)
    let casos: Vec<Caso> = vec![
        ("top", json!({"dim": "level"}), |v| sum(v, "top", "count")),
        ("patterns", json!({}), |v| sum(v, "patterns", "count")),
        ("events", json!({"limit": 100}), |v| v["events"].as_array().unwrap().len() as i64),
        ("timeline", json!({"bucket_secs": 600}), |v| sum(v, "buckets", "count")),
        ("stats", json!({"attr": "dur", "by": "level"}), |v| sum(v, "groups", "count")),
        ("precede", json!({"level": "ERROR", "gap_secs": 60}), |v| sum(v, "episodes", "anchors")),
        ("informe", json!({}), |_| 30),
    ];
    // Toda herramienta con cabecera está aquí: una nueva obliga a añadir su caso.
    for def in crate::tools::tool_defs() {
        let name = def["name"].as_str().unwrap();
        if crate::contract::main_list(name).is_some() {
            assert!(casos.iter().any(|(t, _, _)| *t == name), "falta el caso de {name}");
        }
    }
    for (tool, args, esperado) in casos {
        let v = call(&store, tool, args);
        let total = v["meta"]["total"].as_i64().unwrap_or_else(|| panic!("{tool} sin meta.total: {v}"));
        assert!(total > 0, "{tool}: total=0 con resultado: {v}");
        assert_eq!(total, esperado(&v), "{tool}: total no cuadra con lo devuelto: {v}");
        assert_eq!(v["meta"]["truncated"], false, "{tool}: {v}");
    }
    // Con filtros, el total sigue siendo el de lo que casa.
    let v = call(&store, "stats", json!({"attr": "dur", "level": "ERROR"}));
    assert_eq!(v["meta"]["total"].as_i64(), Some(sum(&v, "groups", "count")), "{v}");
    assert_eq!(v["meta"]["total"], 10, "{v}");
}

#[test]
fn top_por_mcp_acepta_la_dim_declarada_sin_prefijo() {
    let (_db, store) = fixture("dim");
    let a = call(&store, "top", json!({"dim": "cwd"}));
    let b = call(&store, "top", json!({"dim": "attr:cwd"}));
    assert_eq!(a["top"], b["top"], "{a}");
    assert_eq!(sum(&a, "top", "count"), 30, "{a}");
    let err = crate::run_tool_text(&store, "top", &json!({"dim": "nada"})).unwrap_err().to_string();
    assert!(err.contains("level, kind, actor, entity") && err.contains("cwd"), "{err}");
}
