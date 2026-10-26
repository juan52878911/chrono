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
