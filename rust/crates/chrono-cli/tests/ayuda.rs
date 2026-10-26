//! `-h`/`--help` en cualquier subcomando imprime su ayuda y sale con 0 sin
//! abrir ni crear el índice (antes `top --help` tomaba `--help` como dim,
//! `mcp --help` arrancaba el servidor e `init --help` indexaba el cwd).

use std::process::Command;

const COMANDOS: &[&str] = &[
    "init", "add", "sync", "config", "tail", "bench", "hotspots", "coupling", "owners", "bugs", "churn", "tickets", "prs",
    "branches", "phases", "sources", "search", "similar", "show", "explain", "timeline", "stats", "top", "patterns", "trace",
    "events", "informe", "precede", "correlate", "gaps", "session_patterns", "mcp", "hook",
];

#[test]
fn ayuda_de_cada_subcomando_sin_abrir_el_indice() {
    let dir = std::env::temp_dir().join(format!("chrono-ayuda-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("no-existe.db");
    for cmd in COMANDOS {
        for flag in ["--help", "-h"] {
            let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
                .args([cmd, flag, "--db"])
                .arg(&db)
                .current_dir(&dir)
                .env("CHRONO_LANG", "en")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{cmd} {flag}: {stderr}");
            assert!(stdout.starts_with(&format!("chrono {cmd}\n")), "{cmd} {flag}: sin ayuda propia:\n{stdout}");
            assert!(stdout.contains(&format!("  {cmd}")), "{cmd} {flag}:\n{stdout}");
            assert!(stderr.is_empty(), "{cmd} {flag}: {stderr}");
        }
    }
    // Ni el índice ni `.chrono/` se crearon.
    assert!(!db.exists() && !dir.join(".chrono").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn help_comando_y_filtros_solo_donde_aplican() {
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_chrono")).args(args).env("CHRONO_LANG", "en").output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let top = run(&["help", "top"]);
    assert_eq!(top, run(&["top", "--help"]));
    assert!(top.contains("Filters:") && top.contains("--level"), "{top}");
    // gaps tiene sus propios lados, no los filtros comunes.
    assert!(!run(&["gaps", "--help"]).contains("Filters:"));
    // Sin comando, la ayuda general.
    assert!(run(&["--help"]).contains("Usage:"));
    assert!(run(&["help"]).contains("Usage:"));
}
