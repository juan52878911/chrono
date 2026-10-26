//! Ids de evento únicos por fuente: dos fuentes de fichero con la misma línea en
//! el mismo offset (la misma cabecera, dos réplicas del mismo log) no pueden dar
//! el mismo id, o el segundo evento se pierde en silencio (`INSERT OR IGNORE`).
//! Y lo contrario: releer la MISMA fuente no duplica, y lo que deduplica a
//! propósito por `uid_field` (sesiones reanudadas) sigue deduplicando. El `.db`
//! se lee con el módulo `sqlite3` de python3 (chrono-cli no depende de rusqlite).

use std::path::{Path, PathBuf};
use std::process::Command;

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
        let p = std::env::temp_dir().join(format!("chrono-ids-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        let root = TempDir(std::fs::canonicalize(&p).unwrap());
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
    /// `chrono <args> --db <db>`: (¿salió bien?, stdout+stderr).
    fn run(&self, args: &[&str]) -> (bool, String) {
        let db = self.db();
        let mut a = args.to_vec();
        a.extend(["--db", &db]);
        let out = Command::new(env!("CARGO_BIN_EXE_chrono")).args(&a).current_dir(self.dir()).env("CHRONO_LANG", "en").output().unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
    /// Como [`Env::run`], pero falla si sale con error.
    fn ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(ok, "chrono {args:?}: {text}");
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
    fn events(&self) -> usize {
        self.sql("SELECT COUNT(*) FROM events")[0].parse().unwrap()
    }
    fn file(&self, rel: &str) -> String {
        self.dir().join(rel).to_string_lossy().into_owned()
    }
}

const CFG: &str = r#"{"version":2}"#;

const JSONL: &str = concat!(
    r#"{"ts":"2026-01-01T00:00:00Z","level":"ERROR","msg":"falla A"}"#,
    "\n",
    r#"{"ts":"2026-01-01T00:00:01Z","level":"INFO","msg":"va bien"}"#,
    "\n",
    r#"{"ts":"2026-01-01T00:00:02Z","level":"WARN","msg":"lento"}"#,
    "\n"
);
const CSV: &str = "ts,level,msg\n2026-01-01T00:00:00Z,ERROR,falla A\n2026-01-01T00:00:01Z,INFO,va bien\n";
const SYSLOG: &str = "Jan  2 10:00:00 web sshd[12]: Accepted publickey for juan\nJan  2 10:00:01 web sshd[12]: session opened\n";
const CHANGELOG: &str = "# Changelog\n\n## [1.1.0] - 2026-02-01\n\n- Otra cosa\n\n## [1.0.0] - 2026-01-01\n\n- Primera\n";

#[test]
fn dos_ficheros_identicos_como_fuentes_distintas_no_se_pisan() {
    for (name, contents, per_file) in [("app.jsonl", JSONL, 3), ("app.csv", CSV, 2), ("syslog", SYSLOG, 2), ("CHANGELOG.md", CHANGELOG, 2)] {
        let env = Env::new(&format!("dos-{}", name.replace('.', "-")), CFG);
        write(env.dir(), &format!("a/{name}"), contents);
        write(env.dir(), &format!("b/{name}"), contents);
        env.ok(&["add", &env.file(&format!("a/{name}"))]);
        assert_eq!(env.events(), per_file, "{name}: una fuente");
        env.ok(&["add", &env.file(&format!("b/{name}"))]);
        // Antes: los mismos `per_file` (los de b chocaban con los de a y se perdían).
        assert_eq!(env.events(), 2 * per_file, "{name}: dos réplicas son dos fuentes");
        assert_eq!(env.sql("SELECT COUNT(DISTINCT source_ref) FROM events"), ["2"], "{name}");
    }
}

#[test]
fn releer_la_misma_fuente_no_duplica() {
    let cfg = |dims: &str| format!(r#"{{"version":2,"source_options":{{"app.jsonl":{{{dims}}}}}}}"#);
    let env = Env::new("releer", &cfg(""));
    write(env.dir(), "app.jsonl", JSONL);
    env.ok(&["add", &env.file("app.jsonl")]);
    env.ok(&["sync"]);
    env.ok(&["sync"]);
    assert_eq!(env.events(), 3, "sync tras sync sin cambios");
    append(env.dir(), "app.jsonl", "{\"ts\":\"2026-01-01T00:00:03Z\",\"msg\":\"nueva\",\"host\":\"h1\"}\n");
    env.ok(&["sync"]);
    assert_eq!(env.events(), 4, "solo entra el delta");
    // Cambiar las dims relee la fuente entera: borra antes, no duplica.
    env.set_config(&cfg(r#""dims":"host""#));
    let out = env.ok(&["sync"]);
    assert!(out.contains("reindexing"), "{out}");
    assert_eq!(env.events(), 4, "{out}");
    // `add` otra vez re-ingiere: tampoco duplica, y los ids son los mismos.
    let before = env.sql("SELECT id FROM events ORDER BY id");
    env.ok(&["add", &env.file("app.jsonl")]);
    assert_eq!(env.sql("SELECT id FROM events ORDER BY id"), before);
    // Una divergencia (fichero reescrito más corto) relee sin duplicar.
    write(env.dir(), "app.jsonl", &JSONL[..JSONL.find('\n').unwrap() + 1]);
    env.ok(&["sync"]);
    assert_eq!(env.events(), 1);
}

const USE_LINE: &str = r#"{"timestamp":"2024-01-01T00:00:00Z","type":"assistant","sessionId":"s1","cwd":"/w","uuid":"u1","message":{"content":[{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"ls"}}]}}"#;
const RESULT_LINE: &str = r#"{"timestamp":"2024-01-01T00:00:01Z","type":"user","sessionId":"s1","uuid":"u2","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_9","is_error":true,"content":"Exit code 1"}]}}"#;
const MORE_LINE: &str = r#"{"timestamp":"2024-01-01T00:00:02Z","type":"user","sessionId":"s2","uuid":"u3","message":{"content":"sigue"}}"#;

/// Una sesión reanudada copia las líneas de la anterior: el preset
/// claude-sessions las deduplica por `uid_field`, en una glob y entre fuentes.
#[test]
fn las_sesiones_reanudadas_siguen_deduplicando_por_uid() {
    let env = Env::new("reanudadas", CFG);
    write(env.dir(), "solo/s2.jsonl", &format!("{USE_LINE}\n{RESULT_LINE}\n{MORE_LINE}\n"));
    env.ok(&["add", &env.file("solo/s2.jsonl"), "--preset", "claude-sessions"]);
    let alone = env.events();
    assert!(alone >= 2, "{alone}");

    let env = Env::new("reanudadas-glob", CFG);
    write(env.dir(), "p/s1.jsonl", &format!("{USE_LINE}\n{RESULT_LINE}\n"));
    write(env.dir(), "p/s2.jsonl", &format!("{USE_LINE}\n{RESULT_LINE}\n{MORE_LINE}\n"));
    env.ok(&["add", &env.file("p/*.jsonl"), "--as", "jsonl", "--preset", "claude-sessions"]);
    assert_eq!(env.events(), alone, "glob: la sesión reanudada no duplica");

    // Como dos fuentes de un fichero: el uid es global, también deduplica.
    let env = Env::new("reanudadas-dos", CFG);
    write(env.dir(), "p/s1.jsonl", &format!("{USE_LINE}\n{RESULT_LINE}\n"));
    write(env.dir(), "p/s2.jsonl", &format!("{USE_LINE}\n{RESULT_LINE}\n{MORE_LINE}\n"));
    env.ok(&["add", &env.file("p/s1.jsonl"), "--preset", "claude-sessions"]);
    env.ok(&["add", &env.file("p/s2.jsonl"), "--preset", "claude-sessions"]);
    assert_eq!(env.events(), alone, "dos fuentes: la sesión reanudada no duplica");
}

#[test]
fn dos_globs_con_los_mismos_nombres_relativos_no_se_pisan() {
    let env = Env::new("dos-globs", CFG);
    write(env.dir(), "srv1/logs/app.jsonl", JSONL);
    write(env.dir(), "srv2/logs/app.jsonl", JSONL);
    env.ok(&["add", &env.file("srv1/logs/*.jsonl"), "--as", "jsonl", "--name", "srv1"]);
    env.ok(&["add", &env.file("srv2/logs/*.jsonl"), "--as", "jsonl", "--name", "srv2"]);
    // Antes: 3 (la sal era solo `app.jsonl`, la misma en las dos globs).
    assert_eq!(env.events(), 6);
}

/// Índice de antes de los ids por fuente: sin la marca `ids:2` y con los ids
/// sin sal. El primer `sync` reindexa la fuente UNA vez (borrando antes: no
/// duplica) y recupera lo que el choque se había comido; el siguiente ya no.
#[test]
fn un_indice_de_antes_reindexa_una_vez_y_recupera_lo_perdido() {
    let env = Env::new("legado", CFG);
    write(env.dir(), "a/app.jsonl", JSONL);
    write(env.dir(), "b/app.jsonl", JSONL);
    env.ok(&["add", &env.file("a/app.jsonl")]);
    env.ok(&["add", &env.file("b/app.jsonl")]);
    // Como lo dejaba la versión de antes: ids sin sal, y los de b perdidos.
    let b = format!("jsonl:{}", env.file("b/app.jsonl"));
    env.sql(&format!("DELETE FROM events WHERE source_ref = (SELECT ref FROM sources WHERE id = '{b}')"));
    env.sql("UPDATE events SET id = substr(id, 10) WHERE id GLOB '????????.*'");
    env.sql("DELETE FROM source_dims WHERE key GLOB 'ids:*'");
    assert_eq!(env.events(), 3);

    let out = env.ok(&["sync"]);
    assert!(out.contains("reindexing"), "{out}");
    assert_eq!(env.events(), 6, "{out}");
    assert_eq!(env.sql("SELECT COUNT(*) FROM events WHERE id NOT GLOB '????????.*'"), ["0"]);
    // Las relaciones (touches, labels) cuelgan del rid: siguen enteras.
    assert_eq!(env.sql("SELECT COUNT(DISTINCT event_rid) FROM touches"), ["6"]);
    let quiet = env.ok(&["sync"]);
    assert!(!quiet.contains("reindexing"), "una sola vez: {quiet}");
    // La marca no es una dim que se pueda consultar.
    assert!(!env.ok(&["sources"]).contains("ids:"));
    append(env.dir(), "a/app.jsonl", "{\"ts\":\"2026-01-01T00:00:03Z\",\"msg\":\"nueva\"}\n");
    env.ok(&["sync"]);
    assert_eq!(env.events(), 7);
}

/// Una fuente que ya no se puede releer (fichero borrado) no se reindexa por la
/// marca: conserva sus eventos.
#[test]
fn un_indice_de_antes_con_el_fichero_borrado_conserva_sus_eventos() {
    let env = Env::new("legado-borrado", CFG);
    write(env.dir(), "app.jsonl", JSONL);
    env.ok(&["add", &env.file("app.jsonl")]);
    env.sql("UPDATE events SET id = substr(id, 10) WHERE id GLOB '????????.*'");
    env.sql("DELETE FROM source_dims WHERE key GLOB 'ids:*'");
    std::fs::remove_file(env.dir().join("app.jsonl")).unwrap();
    // El `sync` acaba en error (la fuente no se puede leer), pero no la borra.
    let (_, out) = env.run(&["sync"]);
    assert!(out.contains("not reindexing"), "{out}");
    assert_eq!(env.events(), 3, "{out}");
}
