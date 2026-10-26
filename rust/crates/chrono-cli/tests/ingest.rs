//! Robustez de la ingesta con el binario `chrono` real: firma de opciones,
//! `tail` frente a `sync`, huella de los ficheros de una glob, `init` en un repo
//! sin commits y fuentes que ya no se pueden releer enteras. El `.db` se lee con
//! SQL a través del módulo `sqlite3` de python3 (chrono-cli no depende de rusqlite).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir(tag: &str) -> TempDir {
    let p = std::env::temp_dir().join(format!("chrono-ingest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    TempDir(std::fs::canonicalize(&p).unwrap())
}

fn write(base: &Path, rel: &str, contents: &str) {
    let p = base.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, contents).unwrap();
}

/// Raíz de prueba: `<tmp>/.chrono/config.json` y `<tmp>/.chrono/index.db`.
struct Env {
    root: TempDir,
}

impl Env {
    fn new(tag: &str, config: &str) -> Env {
        let root = tempdir(tag);
        write(&root.0, ".chrono/config.json", config);
        Env { root }
    }
    fn dir(&self) -> &Path {
        &self.root.0
    }
    fn db(&self) -> String {
        self.dir().join(".chrono/index.db").to_string_lossy().into_owned()
    }
    fn set_config(&self, config: &str) {
        write(self.dir(), ".chrono/config.json", config);
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args(args)
            .current_dir(self.dir())
            .env("CHRONO_LANG", "en")
            .output()
            .unwrap()
    }
    /// `chrono <args> --db <db>`; falla si sale con error. Devuelve stdout+stderr.
    fn ok(&self, args: &[&str]) -> String {
        let db = self.db();
        let mut a = args.to_vec();
        a.extend(["--db", &db]);
        let out = self.run(&a);
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "chrono {args:?}: {text}");
        text
    }
    /// Filas de `sql`, columnas separadas por `|` (también vale para escribir).
    fn sql(&self, sql: &str) -> Vec<String> {
        let py = "import sqlite3,sys\n\
                  c=sqlite3.connect(sys.argv[1])\n\
                  for r in c.execute(sys.argv[2]):\n print('|'.join('' if v is None else str(v) for v in r))\n\
                  c.commit()\n";
        let out = Command::new("python3").args(["-I", "-c", py, &self.db(), sql]).output().expect("python3");
        assert!(out.status.success(), "sql {sql}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    }
}

fn path_s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

const USE_LINE: &str = r#"{"timestamp":"2024-01-01T00:00:00Z","type":"assistant","sessionId":"s1","cwd":"/w","message":{"content":[{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"ls"}}]}}"#;
const RESULT_LINE: &str = r#"{"timestamp":"2024-01-01T00:00:01Z","type":"user","sessionId":"s1","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_9","is_error":true,"content":"Exit code 1"}]}}"#;

fn tool_result_entities(env: &Env) -> Vec<String> {
    env.sql(
        "SELECT DISTINCT en.key FROM events e JOIN touches t ON t.event_rid=e.rid \
         JOIN entities en ON en.id=t.entity_id WHERE e.kind='tool_result' ORDER BY 1",
    )
}

#[test]
fn activar_entity_via_a_mano_reindexa_la_fuente() {
    let cfg = |via: &str| format!(r#"{{"version":2,"source_options":{{"s.jsonl":{{"preset":"claude-sessions"{via}}}}}}}"#);
    let env = Env::new("via-a-mano", &cfg(r#","entity_via":"""#));
    let d = env.dir().join("datos");
    write(&d, "s.jsonl", &format!("{USE_LINE}\n{RESULT_LINE}\n"));
    env.ok(&["add", &path_s(&d.join("s.jsonl"))]);
    assert_eq!(tool_result_entities(&env), ["unknown"], "sin entity_via el resultado no tiene tool");

    env.set_config(&cfg(""));
    let out = env.ok(&["sync"]);
    assert!(out.contains("reindexing"), "{out}");
    assert_eq!(tool_result_entities(&env), ["Bash"], "{out}");
    let quiet = env.ok(&["sync"]);
    assert!(!quiet.contains("reindexing"), "sin cambios no reindexa: {quiet}");

    // Un índice de antes de la firma de opciones no la tiene: se adopta sin reindexar.
    env.sql("DELETE FROM source_dims WHERE key LIKE 'opt:%'");
    let out = env.ok(&["sync"]);
    assert!(!out.contains("reindexing"), "{out}");
    assert_eq!(env.sql("SELECT COUNT(*) FROM source_dims WHERE key LIKE 'opt:%'"), ["1"]);
}
