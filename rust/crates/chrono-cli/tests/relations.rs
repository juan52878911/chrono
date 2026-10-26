//! Ingesta de `relations` (config v2) y de `entity_via` (preset `claude-sessions`)
//! con el binario `chrono` real. El `.db` se lee con SQL a través del módulo
//! `sqlite3` de python3 (chrono-cli no depende de rusqlite).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir(tag: &str) -> TempDir {
    let p = std::env::temp_dir().join(format!("chrono-rel-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    TempDir(std::fs::canonicalize(&p).unwrap())
}

fn write(base: &Path, rel: &str, contents: &str) {
    let p = base.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, contents).unwrap();
}

fn append(base: &Path, rel: &str, contents: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(base.join(rel)).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
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
    /// Filas de `sql`, columnas separadas por `|`.
    fn sql(&self, sql: &str) -> Vec<String> {
        let py = "import sqlite3,sys\n\
                  c=sqlite3.connect(sys.argv[1])\n\
                  for r in c.execute(sys.argv[2]):\n print('|'.join('' if v is None else str(v) for v in r))\n";
        let out = Command::new("python3").args(["-I", "-c", py, &self.db(), sql]).output().expect("python3");
        assert!(out.status.success(), "sql {sql}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    }
    fn links(&self, rel: &str) -> Vec<String> {
        self.sql(&format!("SELECT DISTINCT target FROM links WHERE rel = '{rel}' ORDER BY target"))
    }
}

fn path_s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn dir_con_patron_saca_la_clave_del_fichero() {
    let env = Env::new(
        "dir",
        r#"{"version":2,"relations":{"prueba":[{"source":"dir","field":"file","pattern":"prueba-(\\d+)"}]}}"#,
    );
    let data = env.dir().join("datos");
    write(&data, "prueba-001.txt", "a");
    write(&data, "sub/prueba-002_resultado.csv", "b");
    write(&data, "otro.txt", "c");
    env.ok(&["add", &path_s(&data), "--as", "dir"]);
    assert_eq!(env.links("prueba"), ["001", "002"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM links WHERE rel = 'prueba'"), ["2"]);
}

const A_JSONL: &str = concat!(
    r#"{"ts":"2026-01-01T00:00:00Z","msg":"abre","ticket":"ABC-12","id":"abcdefghijk"}"#,
    "\n",
    r#"{"ts":"2026-01-01T00:00:01Z","msg":"otra","ticket":"ZZZ-1","id":"xyz"}"#,
    "\n"
);
const B_JSONL: &str = concat!(r#"{"ts":"2026-01-02T00:00:00Z","msg":"cierra ABC-12 ya"}"#, "\n");

#[test]
fn dos_fuentes_misma_clave_comparten_target() {
    let env = Env::new(
        "dos",
        r#"{"version":2,"relations":{"ticket":[
            {"source":"a.jsonl","field":"ticket"},
            {"source":"b.jsonl","field":"title","pattern":"([A-Z]+-\\d+)"}]}}"#,
    );
    let d = env.dir().join("datos");
    write(&d, "a.jsonl", A_JSONL);
    write(&d, "b.jsonl", B_JSONL);
    env.ok(&["add", &path_s(&d.join("a.jsonl"))]);
    env.ok(&["add", &path_s(&d.join("b.jsonl"))]);
    let ids = env.sql("SELECT DISTINCT lt.id FROM link_targets lt WHERE lt.rel='ticket' AND lt.target='ABC-12'");
    assert_eq!(ids.len(), 1, "{ids:?}");
    // Eventos de las dos fuentes enlazados a ese mismo id.
    let srcs = env.sql(
        "SELECT DISTINCT e.source_ref FROM events e JOIN event_links el ON el.event_rid=e.rid \
         JOIN link_targets lt ON lt.id=el.target_id WHERE lt.rel='ticket' AND lt.target='ABC-12' ORDER BY 1",
    );
    assert_eq!(srcs.len(), 2, "{srcs:?}");
}

#[test]
fn len_recorta_el_id() {
    let env = Env::new("len", r#"{"version":2,"relations":{"corto":[{"source":"a.jsonl","field":"id","len":7}]}}"#);
    let d = env.dir().join("datos");
    write(&d, "a.jsonl", A_JSONL);
    env.set_config(
        r#"{"version":2,"source_options":{"a.jsonl":{"id_field":"id"}},"relations":{"corto":[{"source":"a.jsonl","field":"id","len":7}]}}"#,
    );
    env.ok(&["add", &path_s(&d.join("a.jsonl"))]);
    assert_eq!(env.links("corto"), ["abcdefg", "xyz"]);
}

#[test]
fn cambiar_la_relacion_reindexa_y_sin_cambios_no() {
    let cfg = |pat: &str| {
        format!(r#"{{"version":2,"relations":{{"prueba":[{{"source":"dir","field":"file","pattern":"{pat}"}}]}}}}"#)
    };
    let env = Env::new("sync", &cfg("prueba-(\\\\d+)"));
    let data = env.dir().join("datos");
    write(&data, "prueba-001.txt", "a");
    write(&data, "sub/prueba-002_resultado.csv", "b");
    env.ok(&["add", &path_s(&data), "--as", "dir"]);
    assert_eq!(env.links("prueba"), ["001", "002"]);
    let events = env.sql("SELECT COUNT(*) FROM events");

    let quiet = env.ok(&["sync"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), events);
    assert_eq!(env.links("prueba"), ["001", "002"], "{quiet}");

    env.set_config(&cfg("(resultado)"));
    let out = env.ok(&["sync"]);
    assert_eq!(env.links("prueba"), ["resultado"], "{out}");
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), events);
    // Y de nuevo sin cambios: estable.
    env.ok(&["sync"]);
    assert_eq!(env.links("prueba"), ["resultado"]);
}

#[test]
fn config_check_rechaza_un_patron_invalido() {
    let env = Env::new("check", r#"{"version":2,"relations":{"mala":[{"source":"dir","field":"file","pattern":"(a"}]}}"#);
    let out = env.run(&["config", "check", "--db", &env.db()]);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("pattern"), "{text}");
    // Y una válida sale con 0.
    env.set_config(r#"{"version":2,"relations":{"ok":[{"source":"dir","field":"file","pattern":"(a)"}]}}"#);
    let out = env.run(&["config", "check", "--db", &env.db()]);
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

const USE_LINE: &str = r#"{"timestamp":"2024-01-01T00:00:00Z","type":"assistant","sessionId":"s1","cwd":"/w","message":{"content":[{"type":"text","text":"voy"},{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"cat secreto.txt"}}]}}"#;
const RESULT_LINE: &str = r#"{"timestamp":"2024-01-01T00:00:01Z","type":"user","sessionId":"s1","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_9","is_error":true,"content":"Exit code 1\ncat: secreto.txt: No such file"}]}}"#;

fn tool_result_entities(env: &Env) -> Vec<String> {
    env.sql(
        "SELECT DISTINCT en.key FROM events e JOIN touches t ON t.event_rid=e.rid \
         JOIN entities en ON en.id=t.entity_id WHERE e.kind='tool_result' ORDER BY 1",
    )
}

#[test]
fn entity_via_con_el_sync_cortado_entre_uso_y_resultado() {
    let env = Env::new("via", r#"{"version":2}"#);
    let d = env.dir().join("datos");
    write(&d, "s.jsonl", &format!("{USE_LINE}\n"));
    let f = path_s(&d.join("s.jsonl"));
    env.ok(&["add", &f, "--preset", "claude-sessions"]);
    assert!(tool_result_entities(&env).is_empty());
    append(&d, "s.jsonl", &format!("{RESULT_LINE}\n"));
    env.ok(&["sync"]);
    assert_eq!(tool_result_entities(&env), ["Bash"]);
}

#[test]
fn entity_via_con_uso_y_resultado_en_ficheros_distintos() {
    let env = Env::new("via-glob", r#"{"version":2}"#);
    let d = env.dir().join("datos");
    write(&d, "uso.jsonl", &format!("{USE_LINE}\n"));
    let pat = path_s(&d.join("*.jsonl"));
    env.ok(&["add", &pat, "--as", "jsonl", "--preset", "claude-sessions"]);
    write(&d, "res.jsonl", &format!("{RESULT_LINE}\n"));
    env.ok(&["sync"]);
    assert_eq!(tool_result_entities(&env), ["Bash"]);
}
