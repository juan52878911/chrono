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

/// El error de una herramienta, o un panic con lo que devolvió si no falló.
fn error_de(store: &Store, tool: &str, args: Value) -> String {
    match crate::run_tool_text(store, tool, &args) {
        Err(e) => e.to_string(),
        Ok(t) => panic!("{tool} {args} debía fallar y devolvió:\n{}", &t[..t.len().min(300)]),
    }
}

/// Un `since`/`until` que no es una fecha no puede ignorarse (la consulta
/// usaba toda la historia sin decirlo): error que nombra el argumento y el
/// formato, en todas las herramientas que tienen ventana.
#[test]
fn since_y_until_invalidos_son_error_en_todas_las_herramientas() {
    let (_db, store) = fixture("since");
    let mut probadas = 0;
    for def in crate::tools::tool_defs() {
        let name = def["name"].as_str().unwrap();
        let props = &def["inputSchema"]["properties"];
        for k in ["since", "until"] {
            if props.get(k).is_none() {
                continue;
            }
            for malo in ["ayer", "2024-02-30", "2024-13-01", "10:00"] {
                let mut args = json!({k: malo});
                // Los requeridos, con un valor válido: el error tiene que ser el de la fecha.
                for r in def["inputSchema"]["required"].as_array().into_iter().flatten() {
                    let r = r.as_str().unwrap();
                    args[r] = json!(if r == "attr" { "dur" } else if r == "dim" { "level" } else { "x" });
                }
                let e = error_de(&store, name, args);
                assert!(e.contains(&format!("'{k}'")) && e.contains("YYYY-MM-DD"), "{name} {k}={malo}: {e}");
                probadas += 1;
            }
        }
    }
    assert!(probadas >= 40, "{probadas}");
    // Ventana vacía: como en el CLI.
    let e = error_de(&store, "events", json!({"since": "2024-01-02", "until": "2024-01-01"}));
    assert!(e.contains("'until'") && e.contains("'since'"), "{e}");
    // Una fecha válida sigue valiendo.
    crate::run_tool_text(&store, "events", &json!({"since": "2023-11-14T22:13:20Z", "until": "2030-01-01"})).unwrap();
}

/// Un filtro de tipo equivocado se descartaba en silencio (contaba todo):
/// error que nombra el argumento y lo que espera.
#[test]
fn argumento_de_tipo_equivocado_es_error() {
    let (_db, store) = fixture("tipos");
    let casos = [
        ("events", json!({"level": 5}), "level"),
        ("events", json!({"entity": {}}), "entity"),
        ("events", json!({"attr": [1]}), "attr"),
        ("events", json!({"grep": true}), "grep"),
        ("top", json!({"dim": "level", "source": ["git"]}), "source"),
        ("events", json!({"infer_level": "quizá"}), "infer_level"),
        ("events", json!({"infer_level": 1}), "infer_level"),
        ("events", json!({"limit": "abc"}), "limit"),
        ("events", json!({"limit": 0}), "limit"),
        ("events", json!({"limit": -3}), "limit"),
        ("events", json!({"limit": 2.5}), "limit"),
        ("events", json!({"limit": true}), "limit"),
        ("timeline", json!({"bucket_secs": "una hora"}), "bucket_secs"),
        ("timeline", json!({"bucket_secs": i64::MAX}), "bucket_secs"),
        ("events", json!({"format": "xml"}), "format"),
        ("events", json!({"depth": "todo"}), "depth"),
        ("events", json!({"order": 1}), "order"),
        ("trace", json!({"id": "e1", "depth": "hondo"}), "depth"),
        ("precede", json!({"level": "ERROR", "by_origin": "sí"}), "by_origin"),
        ("informe", json!({"tipo": "metricas"}), "tipo"),
        ("hotspots", json!({"since": 2024}), "since"),
    ];
    for (tool, args, k) in casos {
        let e = error_de(&store, tool, args.clone());
        assert!(e.contains(&format!("'{k}'")), "{tool} {args}: {e}");
    }
    // Los argumentos que no son un objeto.
    for args in [json!([]), json!("x"), json!(5)] {
        let e = error_de(&store, "events", args.clone());
        assert!(e.contains("objeto"), "{args}: {e}");
    }
}

/// Un argumento que la herramienta no tiene se ignoraba (`lvl` en vez de
/// `level`, `format` en `hotspots`): error con los que admite.
#[test]
fn argumento_desconocido_es_error() {
    let (_db, store) = fixture("desconocido");
    let e = error_de(&store, "events", json!({"lvl": "ERROR"}));
    assert!(e.contains("'lvl'") && e.contains("level"), "{e}");
    let e = error_de(&store, "hotspots", json!({"format": "json"}));
    assert!(e.contains("'format'") && e.contains("since"), "{e}");
    // `null` es «no lo paso», como si faltara.
    crate::run_tool_text(&store, "events", &json!({"level": null, "limit": null})).unwrap();
}

/// Lo que ya valía sigue valiendo: números (y booleanos) en una cadena.
#[test]
fn numeros_y_booleanos_como_cadena_valen() {
    let (_db, store) = fixture("cadenas");
    let text = crate::run_tool_text(&store, "events", &json!({"limit": " 4 ", "infer_level": "false", "format": "json"})).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), 4, "{text}");
    let text = crate::run_tool_text(&store, "timeline", &json!({"bucket_secs": "600", "limit": 10.0, "format": "json"})).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["bucket_ms"], json!(600_000), "{text}");
}

/// `informe` no descarta lo que no usa: `tipo=codigo` no filtra por nivel ni
/// servicio, y `tipo=logs` no tiene `target`.
#[test]
fn informe_dice_lo_que_no_aplica() {
    let (_db, store) = fixture("informe");
    let e = error_de(&store, "informe", json!({"tipo": "codigo", "target": "src/f1.rs", "level": "ERROR"}));
    assert!(e.contains("'level'") && e.contains("codigo"), "{e}");
    let e = error_de(&store, "informe", json!({"target": "src/f1.rs"}));
    assert!(e.contains("'target'"), "{e}");
}

/// Por MCP, el error de una herramienta es un resultado con `isError: true`
/// (el modelo lo lee y corrige la llamada), no un error de JSON-RPC.
#[test]
fn por_mcp_el_error_de_argumentos_es_is_error() {
    let (_db, store) = fixture("iserror");
    let state = crate::State::with_store(Some(store));
    let r = crate::tools::call_tool(&state, &json!({"name": "events", "arguments": {"since": "ayer"}})).unwrap();
    assert_eq!(r["isError"], json!(true), "{r}");
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("since") && text.contains("YYYY-MM-DD"), "{text}");
    let r = crate::tools::call_tool(&state, &json!({"name": "events", "arguments": {"limit": 2}})).unwrap();
    assert!(r.get("isError").is_none(), "{r}");
}

/// `correlate` por tiempo descartaba `kind` (solo lo usaba con `via`).
#[test]
fn correlate_por_tiempo_respeta_kind() {
    let db = TempDb(std::env::temp_dir().join(format!("chrono-mcp-test-adversario-corr-{}.db", std::process::id())));
    let _ = std::fs::remove_file(&db.0);
    let mut store = Store::open(&db.0).unwrap();
    let mut w = store.writer().unwrap();
    for (i, (src, kind)) in [("jsonl:/a", "log"), ("git:/r", "commit"), ("git:/r", "log"), ("git:/r", "log")].iter().enumerate() {
        let at_epoch = 1_700_000_000_000 + i as i64 * 60_000;
        let e = Event {
            id: format!("c{i}"),
            source_id: src.to_string(),
            kind: kind.to_string(),
            at: crate::contract::iso(at_epoch),
            at_epoch,
            title: format!("evento {i}"),
            ..Default::default()
        };
        w.add_event(&e).unwrap();
    }
    w.commit().unwrap();
    let text = crate::run_tool_text(&store, "correlate", &json!({"id": "c0", "kind": "commit"})).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let ids: Vec<&str> = v["related"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["c1"], "{text}");
    // Con `via`, `delta_secs` no se usa: error, no ignorarlo.
    let e = error_de(&store, "correlate", json!({"id": "c0", "via": "session", "delta_secs": 60}));
    assert!(e.contains("'delta_secs'") && e.contains("'via'"), "{e}");
}

/// `delega` pasa por la misma validación que las demás: argumento de más,
/// tipo equivocado, `format` fuera de su lista y ventana imposible son error.
#[test]
fn delega_valida_sus_argumentos() {
    let (_db, store) = fixture("delega");
    for (args, k) in [
        (json!({"profile": "x"}), "profile"),
        (json!({"limit": "diez"}), "limit"),
        (json!({"stale_hours": 0}), "stale_hours"),
        (json!({"orchestrators": ["a"]}), "orchestrators"),
        (json!({"format": "yaml"}), "format"),
        (json!({"since": "2026-02-30"}), "since"),
    ] {
        let e = error_de(&store, "delega", args.clone());
        assert!(e.contains(&format!("'{k}'")), "{args}: {e}");
    }
    crate::run_tool_text(&store, "delega", &json!({"format": "json", "limit": "3", "stale_hours": 2})).unwrap();
}
