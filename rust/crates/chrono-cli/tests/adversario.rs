//! Ningún subcomando puede hacer panic con argumentos raros: el binario lleva
//! `panic = "abort"`, así que un panic es un aborto (señal) o un `panicked at`
//! en stderr. Se recorren todos los comandos con cada flag y valores inválidos.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

enum Fin {
    Ok,
    Panic(String),
    Colgado,
}

fn run(dir: &Path, args: &[String]) -> Fin {
    let mut child = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(args)
        .current_dir(dir)
        .env("CHRONO_LANG", "en")
        .env("CHRONO_NOW", "1767312000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut err = String::new();
            child.stderr.take().unwrap().read_to_string(&mut err).ok();
            return if status.code().is_none() || err.contains("panicked at") {
                err.truncate(600);
                Fin::Panic(format!("{status}: {err}"))
            } else {
                Fin::Ok
            };
        }
        if t0.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            return Fin::Colgado;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Índice pequeño con un log jsonl en `<tmp>/.chrono/index.db`.
fn indice() -> TempDir {
    let p = std::env::temp_dir().join(format!("chrono-adversario-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(p.join(".chrono")).unwrap();
    let dir = TempDir(std::fs::canonicalize(&p).unwrap());
    let mut lines = String::new();
    for i in 0..30 {
        let level = if i % 3 == 0 { "ERROR" } else { "INFO" };
        lines.push_str(&format!(
            "{{\"ts\":\"2026-01-01T00:{i:02}:00Z\",\"level\":\"{level}\",\"msg\":\"evento {i} ñ 😀\",\"sessionId\":\"s{}\",\"svc\":\"a{}\"}}\n",
            i % 4,
            i % 2
        ));
    }
    std::fs::write(dir.0.join("app.jsonl"), lines).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["add", "app.jsonl", "--db", ".chrono/index.db"])
        .current_dir(&dir.0)
        .env("CHRONO_LANG", "en")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    dir
}

const VALORES: &[&str] = &[
    "", " ", "-1", "0", "1", "99999999999999999999", "-9223372036854775808", "1e9", "abc", "😀ñ", "2024-02-30",
    "2024-01-01T10:00:00.123+02:00", "now-1h", "%_\\", "*", "@", "k=v:w", "=", ":", ",", "' OR 1=1 --", "\u{7f}",
];

const FLAGS: &[&str] = &[
    "--since", "--until", "--bucket", "--by", "--limit", "--order", "--gap", "--window", "--before-levels", "--delta",
    "--kind", "--via", "--from", "--to", "--depth", "--task", "--session", "--hotspots", "--entity", "--source", "--grep",
    "--level", "--attr", "--join", "--lang",
];

/// Comandos de consulta con un argumento posicional razonable.
const CONSULTAS: &[(&str, &str)] = &[
    ("hotspots", ""), ("coupling", "app.jsonl"), ("owners", "src"), ("bugs", ""), ("churn", ""), ("tickets", "1"),
    ("prs", ""), ("branches", ""), ("phases", ""), ("sources", ""), ("search", "evento"), ("similar", "x"),
    ("timeline", ""), ("stats", "dur"), ("top", "level"), ("patterns", ""), ("events", ""), ("explain", "app.jsonl:1-3"),
    ("informe", ""), ("precede", ""), ("correlate", "x"), ("gaps", ""), ("trace", "x"), ("session_patterns", ""),
    ("show", "x"),
];

#[test]
fn ningun_comando_hace_panic() {
    let d = indice();
    let mut casos: Vec<Vec<String>> = Vec::new();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    // Sin argumentos, con posicionales raros y con flags sueltas sin valor.
    let todos = [
        "version", "help", "config", "hotspots", "coupling", "owners", "bugs", "churn", "tickets", "prs", "branches", "phases",
        "sources", "search", "similar", "timeline", "stats", "top", "patterns", "events", "explain", "informe", "precede",
        "correlate", "gaps", "trace", "session_patterns", "show", "nada",
    ];
    for c in todos {
        casos.push(s(&[c]));
        for v in VALORES {
            casos.push(s(&[c, v]));
        }
        casos.push(s(&[c, "--limit"]));
        casos.push(s(&[c, "--since", "--until"]));
        casos.push(s(&[c, "--", "--limit", "5"]));
        casos.push(s(&[c, "a", "b", "c", "d"]));
        casos.push(s(&[c, "--help"]));
        casos.push(s(&[c, "-h"]));
        casos.push(s(&[c, "--compact", "--compact"]));
    }
    // Cada consulta con cada flag y cada valor raro.
    for (c, pos) in CONSULTAS {
        for f in FLAGS {
            for v in VALORES.iter().step_by(2) {
                let mut a = vec![c.to_string()];
                if !pos.is_empty() {
                    a.push(pos.to_string());
                }
                a.push(f.to_string());
                a.push(v.to_string());
                casos.push(a);
            }
        }
    }
    // Explain con rangos raros.
    for t in ["app.jsonl:", "app.jsonl:0-0", "app.jsonl:5-1", "app.jsonl:-3", "app.jsonl:99999999999999999999", ":1-2", "app.jsonl:a-b", "😀:1"] {
        casos.push(s(&["explain", t]));
    }
    // Comandos que leen stdin (vacío).
    // `tail` sigue el índice sin fin: solo su ayuda.
    casos.push(s(&["tail", "--help"]));
    casos.push(s(&["hook"]));
    casos.push(s(&["hook", "nada"]));
    casos.push(s(&["mcp", "--db", "/no/existe.db"]));
    // --db raro.
    for db in ["", "/", "/no/existe.db", "app.jsonl", ".chrono", "😀.db"] {
        casos.push(s(&["sources", "--db", db]));
    }
    let total = casos.len();
    let casos = Mutex::new(casos);
    let siguiente = AtomicUsize::new(0);
    let fallos = Mutex::new(Vec::new());
    std::thread::scope(|sc| {
        for _ in 0..16 {
            sc.spawn(|| loop {
                let i = siguiente.fetch_add(1, Ordering::Relaxed);
                let Some(args) = casos.lock().unwrap().get(i).cloned() else { break };
                match run(&d.0, &args) {
                    Fin::Ok => {}
                    Fin::Panic(e) => fallos.lock().unwrap().push(format!("PANIC {args:?}: {e}")),
                    Fin::Colgado => fallos.lock().unwrap().push(format!("COLGADO {args:?}")),
                }
            });
        }
    });
    let fallos = fallos.into_inner().unwrap();
    assert!(fallos.is_empty(), "{} de {total} casos:\n{}", fallos.len(), fallos.join("\n"));
    assert!(total > 5000, "{total}");
}
