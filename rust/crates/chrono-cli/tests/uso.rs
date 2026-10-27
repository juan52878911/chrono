//! Fallos de uso que daban una salida vacía o engañosa en vez de decir qué
//! pasa: comando mal escrito, `--source` que no existe, ventana vacía, flags
//! que se ignoraban (`search --limit`, `informe --level`) o desconocidas.

use std::path::{Path, PathBuf};
use std::process::Command;

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_chrono")).args(args).current_dir(dir).env("CHRONO_LANG", "en").output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Índice con un log de 30 líneas (10 ERROR) en `<tmp>/.chrono/index.db`.
fn indice(tag: &str) -> TempDir {
    let p = std::env::temp_dir().join(format!("chrono-uso-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(p.join(".chrono")).unwrap();
    let dir = TempDir(std::fs::canonicalize(&p).unwrap());
    let mut lines = String::new();
    for i in 0..30 {
        let level = if i % 3 == 0 { "ERROR" } else { "INFO" };
        lines.push_str(&format!("{{\"ts\":\"2026-01-01T00:{i:02}:00Z\",\"level\":\"{level}\",\"msg\":\"evento {i}\"}}\n"));
    }
    std::fs::write(dir.0.join("app.jsonl"), lines).unwrap();
    let (ok, _, err) = run(&dir.0, &["add", "app.jsonl", "--db", ".chrono/index.db"]);
    assert!(ok, "{err}");
    dir
}

#[test]
fn comando_mal_escrito_sugiere_sin_buscar_el_indice() {
    let (ok, _, err) = run(&std::env::temp_dir(), &["topp", "--db", "/no/existe.db"]);
    assert!(!ok);
    assert!(err.contains("unknown command \"topp\"") && err.contains("Did you mean \"top\"?"), "{err}");
    assert!(!err.contains("Usage:") && !err.contains("no index"), "{err}");
}

#[test]
fn source_inexistente_y_ventana_vacia_fallan_con_motivo() {
    let d = indice("source");
    let (ok, _, err) = run(&d.0, &["events", "--source", "nada"]);
    assert!(!ok && err.contains("no source matches --source \"nada\"") && err.contains("app.jsonl"), "{err}");
    // Por kind o por id sigue valiendo.
    assert!(run(&d.0, &["events", "--source", "jsonl", "--limit", "1"]).0);
    let (ok, _, err) = run(&d.0, &["events", "--since", "2026-01-02", "--until", "2026-01-01"]);
    assert!(!ok && err.contains("window is empty"), "{err}");
}

#[test]
fn flags_que_se_ignoraban() {
    let d = indice("flags");
    let (ok, out, _) = run(&d.0, &["search", "evento", "--limit", "2", "--compact"]);
    assert!(ok && out.contains("results (2)"), "{out}");
    let (ok, out, err) = run(&d.0, &["informe", "--level", "ERROR"]);
    assert!(ok, "{err}");
    assert!(out.contains("total=10 ") && out.contains("level=ERROR"), "{out}");
    let (ok, _, err) = run(&d.0, &["top", "level", "--levl", "ERROR"]);
    assert!(ok && err.contains("unknown flag --levl"), "{err}");
    let (ok, _, err) = run(&d.0, &["timeline", "--by", "entity"]);
    assert!(!ok && err.contains("use level or kind"), "{err}");
}

#[test]
fn flags_que_no_aplican_al_comando_avisan() {
    let d = indice("no-aplica");
    let (ok, _, err) = run(&d.0, &["top", "level", "--by", "kind"]);
    assert!(ok && err.contains("--by does not apply to top"), "{err}");
    let (ok, _, err) = run(&d.0, &["search", "evento", "--since", "2026-01-01"]);
    assert!(ok && err.contains("--since does not apply to search"), "{err}");
    // Las que sí aplican no avisan.
    let (ok, _, err) = run(&d.0, &["top", "level", "--since", "2026-01-01", "--level", "ERROR", "--limit", "3", "--compact"]);
    assert!(ok && err.is_empty(), "{err}");
    let (ok, _, err) = run(&d.0, &["timeline", "--by", "level", "--bucket", "1h", "--grep", "evento"]);
    assert!(ok && err.is_empty(), "{err}");
}

#[test]
fn events_order_desc_da_lo_mas_reciente() {
    let d = indice("order");
    let (ok, out, err) = run(&d.0, &["events", "--order", "desc", "--limit", "2", "--compact"]);
    assert!(ok, "{err}");
    let (a, b) = (out.find("evento 29").unwrap_or(usize::MAX), out.find("evento 28").unwrap_or(0));
    assert!(a < b && !out.contains("evento 0\n"), "{out}");
    let (ok, _, err) = run(&d.0, &["events", "--order", "nuevo"]);
    assert!(!ok && err.contains("invalid --order"), "{err}");
}

#[test]
fn sources_y_phases_no_se_dan_por_truncadas() {
    let d = indice("sources");
    for cmd in ["sources", "phases"] {
        let (ok, out, err) = run(&d.0, &[cmd]);
        assert!(ok, "{err}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let tb = &v["token_budget"];
        assert_eq!((tb["max_items"].clone(), tb["truncated"].clone()), (tb["returned"].clone(), false.into()), "{cmd}: {tb}");
    }
}

/// Índice con un log de 3 líneas con tiempo (con `ms` y `region`) y una sin tiempo.
fn indice_attrs(tag: &str) -> TempDir {
    let p = std::env::temp_dir().join(format!("chrono-uso-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(p.join(".chrono")).unwrap();
    let dir = TempDir(std::fs::canonicalize(&p).unwrap());
    let lines = [
        r#"{"ts":"2026-01-01T00:00:00Z","level":"ERROR","msg":"uno","ms":10,"region":"db-1"}"#,
        r#"{"ts":"2026-01-01T00:10:00Z","level":"INFO","msg":"dos","ms":30,"region":"db-2"}"#,
        r#"{"ts":"2026-01-01T00:20:00Z","level":"INFO","msg":"tres","ms":20,"region":"db-1"}"#,
        r#"{"level":"ERROR","msg":"sin tiempo"}"#,
    ];
    std::fs::write(dir.0.join("app.jsonl"), lines.join("\n") + "\n").unwrap();
    let (ok, _, err) = run(&dir.0, &["add", "app.jsonl", "--db", ".chrono/index.db"]);
    assert!(ok, "{err}");
    dir
}

#[test]
fn vacios_que_ahora_dicen_por_que() {
    let d = indice_attrs("vacios");
    // top attr: con una clave que no existe: vacío, sin error, con aviso.
    let (ok, out, err) = run(&d.0, &["top", "attr:contenedor", "--compact"]);
    assert!(ok, "{err}");
    assert!(out.starts_with("hints: no event has the attribute \"contenedor\"; attributes: ") && out.contains("region"), "{out}");
    let (ok, out, _) = run(&d.0, &["top", "attr:region", "--compact"]);
    assert!(ok && !out.contains("hints") && out.contains("db-1\t2"), "{out}");
    // --level mal escrito: aviso con los niveles que hay.
    let (ok, _, err) = run(&d.0, &["events", "--level", "eror"]);
    assert!(ok && err.contains("no event has level eror (levels: ERROR,INFO)"), "{err}");
    let (ok, _, err) = run(&d.0, &["top", "kind", "--level", "error,info"]);
    assert!(ok && err.is_empty(), "{err}");
    // stats de una attr que no existe o no es numérica.
    let (_, out, _) = run(&d.0, &["stats", "latencia", "--compact"]);
    assert!(out.contains("hints: no event has the attribute \"latencia\""), "{out}");
    let (_, out, _) = run(&d.0, &["stats", "region", "--compact"]);
    assert!(out.contains("hints: the attribute \"region\" has no numeric values"), "{out}");
    let (_, out, _) = run(&d.0, &["stats", "ms", "--attr", "region:db-1", "--compact"]);
    assert!(!out.contains("hints") && out.contains("\t2\t10\t"), "{out}");
    // timeline: los eventos sin tiempo se dicen.
    let (_, out, _) = run(&d.0, &["timeline", "--compact"]);
    assert!(out.contains("1 matching events have no time") && out.contains("untimed: 1"), "{out}");
    let (_, out, _) = run(&d.0, &["timeline", "--level", "INFO", "--compact"]);
    assert!(!out.contains("untimed"), "{out}");
    // hotspots sin git y session_patterns sin Nod.
    let (_, out, _) = run(&d.0, &["hotspots", "--compact"]);
    assert!(out.starts_with("hints: hotspots needs a git source") && out.contains("hotspots: -"), "{out}");
    let (_, out, _) = run(&d.0, &["session_patterns", "--compact"]);
    assert!(out.starts_with("hints: no Nod events in the index"), "{out}");
    // correlate por tiempo con una sola fuente sugiere --via.
    let (_, ev, _) = run(&d.0, &["events", "--limit", "1"]);
    let v: serde_json::Value = serde_json::from_str(&ev).unwrap();
    let id = v["result"]["events"][0]["id"].as_str().unwrap().to_string();
    let (ok, out, err) = run(&d.0, &["correlate", &id, "--compact"]);
    assert!(ok && out.contains("hints: correlate by time only looks at OTHER sources") && out.contains("--via"), "{out}{err}");
}

#[test]
fn errores_de_gaps_en_el_idioma_pedido() {
    let d = indice("gaps-lang");
    let args = ["gaps", "--from", "app.jsonl", "--to", "app.jsonl", "--via", "tikcet"];
    let (ok, _, err) = run(&d.0, &args);
    assert!(!ok && err.contains("unknown relation key") && !err.contains("clave"), "{err}");
    let (ok, _, err) = run(&d.0, &[&args[..], &["--lang", "es"]].concat());
    assert!(!ok && err.contains("clave de relación desconocida"), "{err}");
}
