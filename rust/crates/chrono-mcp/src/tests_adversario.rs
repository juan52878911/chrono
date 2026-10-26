//! Argumentos de tipo equivocado o fuera de rango en TODAS las herramientas:
//! nunca un panic (el binario aborta con `panic = "abort"` y el servidor MCP
//! se cae con él); un error claro o el valor por defecto.

use std::path::PathBuf;

use chrono_core::{Event, Link, Touch};
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

fn fixture(name: &str) -> (TempDb, Store) {
    let db = TempDb(std::env::temp_dir().join(format!("chrono-mcp-test-adversario-{name}-{}.db", std::process::id())));
    let _ = std::fs::remove_file(&db.0);
    let mut store = Store::open(&db.0).unwrap();
    let mut w = store.writer().unwrap();
    w.set_dims(vec!["cwd".into()]);
    for i in 0..40i64 {
        let at_epoch = 1_700_000_000_000 + i * 60_000;
        let mut e = Event {
            id: format!("e{i}"),
            source_id: "jsonl:/app".into(),
            kind: if i % 5 == 0 { "commit" } else { "log" }.into(),
            at: crate::contract::iso(at_epoch),
            at_epoch,
            level: if i % 3 == 0 { "ERROR" } else { "INFO" }.into(),
            title: format!("fallo {i} ñ 😀 al conectar"),
            touches: vec![Touch { entity: format!("src/f{}.rs", i % 4), entity_type: "file".into(), weight: 1, attrs: Default::default() }],
            links: vec![Link { rel: "session".into(), target: format!("s{}", i % 3) }],
            ..Default::default()
        };
        e.attrs.insert("dur".into(), (i % 7).to_string());
        e.attrs.insert("cwd".into(), format!("/p{}", i % 3));
        w.add_event(&e).unwrap();
    }
    w.commit().unwrap();
    store.rebuild_fts().unwrap();
    store.build_rollups(60_000).unwrap();
    (db, store)
}

/// Valores raros para cualquier argumento.
fn raros() -> Vec<Value> {
    vec![
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(i64::MIN),
        json!(i64::MAX),
        json!(u64::MAX),
        json!(1e308),
        json!(-0.5),
        json!(""),
        json!(" "),
        json!("10"),
        json!("-5"),
        json!("abc"),
        json!("😀ñ"),
        json!("' OR 1=1 --"),
        json!("%_*"),
        json!("*"),
        json!("@"),
        json!("=:,"),
        json!("k=v:w,x"),
        json!("a".repeat(100_000)),
        json!("2024-02-30"),
        json!("9999-12-31T23:59:59.999Z"),
        json!("\u{0}"),
        json!([]),
        json!([1, "a"]),
        json!({}),
        json!({"a": 1}),
    ]
}

#[test]
fn ninguna_herramienta_hace_panic_con_argumentos_raros() {
    let (_db, store) = fixture("panic");
    let defs = crate::tools::tool_defs();
    let mut casos = 0;
    let mut panics = Vec::new();
    for def in &defs {
        let name = def["name"].as_str().unwrap().to_string();
        let props: Vec<String> = def["inputSchema"]["properties"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
        // Argumentos que no son un objeto.
        let mut variantes: Vec<Value> = vec![json!(null), json!([]), json!("x"), json!(5), json!({})];
        // Cada propiedad con cada valor raro, sola y con las demás vacías.
        for p in &props {
            for v in raros() {
                let mut a = json!({});
                a[p] = v;
                variantes.push(a);
            }
        }
        // Todas a la vez con el mismo valor raro.
        for v in raros() {
            let mut a = json!({});
            for p in &props {
                a[p] = v.clone();
            }
            variantes.push(a);
        }
        for args in variantes {
            casos += 1;
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::run_tool_text(&store, &name, &args)));
            if r.is_err() {
                let mut s = args.to_string();
                s.truncate(200);
                panics.push(format!("{name} {s}"));
            }
        }
    }
    assert!(panics.is_empty(), "{} panics de {casos} casos:\n{}", panics.len(), panics.join("\n"));
    assert!(casos > 1000, "{casos}");
}

/// `limit` como cadena (los modelos lo mandan así a menudo) vale como número.
#[test]
fn limit_como_cadena_es_un_numero() {
    let (_db, store) = fixture("limit");
    let text = crate::run_tool_text(&store, "events", &json!({"limit": "3", "format": "json"})).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), 3, "{text}");
}

/// Una `attr` con clave inválida no puede borrarse del filtro sin decirlo
/// (contaba TODOS los eventos); y `k=v:w` es la clave `k` con valor `v:w`.
#[test]
fn attr_invalida_es_error_y_valor_con_dos_puntos() {
    let (_db, store) = fixture("attr");
    let r = crate::run_tool_text(&store, "top", &json!({"dim": "level", "attr": "mal clave:x", "format": "json"}));
    assert!(r.is_err(), "{r:?}");
    let text = crate::run_tool_text(&store, "events", &json!({"attr": "cwd=/p1:x", "format": "json"})).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), 0, "{text}");
    let text = crate::run_tool_text(&store, "events", &json!({"attr": "cwd=/p1", "limit": 100, "format": "json"})).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), 13, "{text}");
}

/// `%`, `_` y `\` en `grep`, en el prefijo de `kind` y en el de una `attr` son
/// literales, por todos los caminos (eventos, plantillas, cubo).
#[test]
fn comodines_de_like_son_literales() {
    let db = TempDb(std::env::temp_dir().join(format!("chrono-mcp-test-adversario-like-{}.db", std::process::id())));
    let _ = std::fs::remove_file(&db.0);
    let mut store = Store::open(&db.0).unwrap();
    let mut w = store.writer().unwrap();
    let filas = [
        ("user_id ok", "a_b", "x_1"),
        ("user-id ok", "axb", "xy1"),
        ("100% done", "a_b", "x_2"),
        ("1000 done", "axb", "xz2"),
        (r"c:\tmp ok", "axb", r"x\3"),
        ("c:xtmp ok", "axb", "xx3"),
    ];
    for (i, (title, kind, p)) in filas.iter().enumerate() {
        let at_epoch = 1_700_000_000_000 + i as i64 * 60_000;
        let mut e = Event {
            id: format!("l{i}"),
            source_id: "jsonl:/app".into(),
            kind: kind.to_string(),
            at: crate::contract::iso(at_epoch),
            at_epoch,
            level: "INFO".into(),
            title: title.to_string(),
            ..Default::default()
        };
        e.attrs.insert("p".into(), p.to_string());
        w.add_event(&e).unwrap();
    }
    w.commit().unwrap();
    store.rebuild_fts().unwrap();
    store.build_rollups(60_000).unwrap();
    let total = |tool: &str, args: Value| -> i64 {
        let mut args = args;
        args["format"] = json!("json");
        let text = crate::run_tool_text(&store, tool, &args).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        v["meta"]["total"].as_i64().unwrap_or_else(|| panic!("{tool}: {text}"))
    };
    for tool in ["events", "top", "timeline"] {
        let base = if tool == "top" { json!({"dim": "level"}) } else { json!({}) };
        let with = |k: &str, v: &str| {
            let mut a = base.clone();
            a[k] = json!(v);
            a
        };
        assert_eq!(total(tool, with("grep", "user_id")), 1, "{tool} grep user_id");
        assert_eq!(total(tool, with("grep", "100%")), 1, "{tool} grep 100%");
        assert_eq!(total(tool, with("grep", r"c:\t")), 1, "{tool} grep c:\\t");
        assert_eq!(total(tool, with("kind", "a_*")), 2, "{tool} kind a_*");
        assert_eq!(total(tool, with("attr", "p:x_*")), 2, "{tool} attr p:x_*");
        assert_eq!(total(tool, with("attr", r"p:x\*")), 1, "{tool} attr p:x\\*");
    }
    let text = crate::run_tool_text(&store, "patterns", &json!({"grep": "user_id", "format": "json"})).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let n: i64 = v["patterns"].as_array().unwrap().iter().map(|p| p["count"].as_i64().unwrap()).sum();
    assert_eq!(n, 1, "patterns grep user_id: {text}");
}
