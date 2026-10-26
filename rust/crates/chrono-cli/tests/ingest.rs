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
    // La firma va junto a las dims, pero `sources` solo enseña las dims.
    let src = env.ok(&["sources"]);
    assert!(src.contains("sessionId") && !src.contains("opt:"), "{src}");

    // Un índice de antes de la firma de opciones no la tiene: se adopta sin reindexar.
    env.sql("DELETE FROM source_dims WHERE key LIKE 'opt:%'");
    let out = env.ok(&["sync"]);
    assert!(!out.contains("reindexing"), "{out}");
    assert_eq!(env.sql("SELECT COUNT(*) FROM source_dims WHERE key LIKE 'opt:%'"), ["1"]);
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Prueba", "-c", "user.email=p@x", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn init_en_una_rama_huerfana_no_dice_que_el_repo_este_vacio() {
    let env = Env::new("huerfana", r#"{"version":2}"#);
    git(env.dir(), &["init", "-q", "-b", "main"]);
    git(env.dir(), &["commit", "-q", "--allow-empty", "-m", "primero"]);
    git(env.dir(), &["switch", "-q", "--orphan", "nueva"]);
    let out = env.ok(&["init"]);
    // Antes: «the repo has no commits yet», con main llena.
    assert!(!out.contains("the repo has no commits"), "{out}");
    assert!(out.contains("HEAD points to no commit (branch nueva has no commits yet), but 1 branch(es) do"), "{out}");
}

#[test]
fn init_en_un_repo_sin_commits_y_add_y_sync_despues() {
    let env = Env::new("sin-commits", r#"{"version":2}"#);
    git(env.dir(), &["init", "-q"]);
    let out = env.ok(&["init"]);
    assert!(out.contains("0 commits") && out.contains("has no commits yet"), "{out}");
    write(env.dir(), "app.jsonl", "{\"ts\":\"2024-01-01T00:00:00Z\",\"msg\":\"hola\"}\n");
    env.ok(&["add", &path_s(&env.dir().join("app.jsonl"))]);
    env.ok(&["sync"]);
    git(env.dir(), &["commit", "-q", "--allow-empty", "-m", "primero"]);
    let out = env.ok(&["sync"]);
    assert!(out.contains("1 new events"), "{out}");
    assert_eq!(env.sql("SELECT kind, COUNT(*) FROM events GROUP BY kind ORDER BY kind"), ["commit|1", "log|1"]);
}

#[test]
fn un_fichero_reescrito_con_el_mismo_tamano_y_mtime_se_relee() {
    let env = Env::new("huella", r#"{"version":2}"#);
    let d = env.dir().join("datos");
    let line = |msg: &str| format!("{{\"ts\":\"2024-01-01T00:00:00Z\",\"msg\":\"{msg}\"}}\n");
    write(&d, "a.jsonl", &line("alfa uno"));
    write(&d, "b.jsonl", &line("otro fichero"));
    env.ok(&["add", &path_s(&d.join("*.jsonl")), "--as", "jsonl"]);
    let titles = || env.sql("SELECT title FROM events ORDER BY title");
    assert_eq!(titles(), ["alfa uno", "otro fichero"]);

    let a = d.join("a.jsonl");
    let mtime = std::fs::metadata(&a).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&a, line("beta dos")).unwrap();
    std::fs::File::options().write(true).open(&a).unwrap().set_modified(mtime).unwrap();
    assert_eq!(std::fs::metadata(&a).unwrap().modified().unwrap(), mtime);

    env.ok(&["sync"]);
    assert_eq!(titles(), ["beta dos", "otro fichero"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM events_fts WHERE events_fts MATCH 'beta'"), ["1"]);
    // Sin tocar nada más, un sync no relee ni reindexa.
    let out = env.ok(&["sync"]);
    assert!(out.contains("0 new events"), "{out}");
}

#[test]
fn una_edicion_en_medio_de_un_fichero_grande_se_relee() {
    let env = Env::new("huella-medio", r#"{"version":2}"#);
    let d = env.dir().join("datos");
    // ~240 KB: el medio queda fuera de los primeros y últimos 64 KB.
    let line = |i: usize, tag: &str| format!("{{\"ts\":\"2024-01-01T00:00:00Z\",\"msg\":\"linea {i:05} {tag}\"}}\n");
    let body: String = (0..4000).map(|i| line(i, "aa")).collect();
    write(&d, "a.jsonl", &body);
    env.ok(&["add", &path_s(&d.join("*.jsonl")), "--as", "jsonl"]);
    let a = d.join("a.jsonl");
    let has = |msg: &str| env.sql(&format!("SELECT COUNT(*) FROM events WHERE title = '{msg}'"))[0].clone();
    let edit = |from: &str, to: &str| {
        let s = std::fs::read_to_string(&a).unwrap();
        assert!(s.contains(from));
        std::fs::write(&a, s.replacen(from, to, 1)).unwrap();
    };
    let mtime = || std::fs::metadata(&a).unwrap().modified().unwrap();

    // 1. Mismo tamaño, otro mtime.
    std::thread::sleep(std::time::Duration::from_millis(20));
    edit("linea 02000 aa", "linea 02000 bb");
    env.ok(&["sync"]);
    assert_eq!((has("linea 02000 bb"), has("linea 02000 aa")), ("1".into(), "0".into()));

    // 2. Mismo tamaño y el mtime de antes.
    let before = mtime();
    std::thread::sleep(std::time::Duration::from_millis(20));
    edit("linea 02001 aa", "linea 02001 cc");
    std::fs::File::options().write(true).open(&a).unwrap().set_modified(before).unwrap();
    env.ok(&["sync"]);
    assert_eq!((has("linea 02001 cc"), has("linea 02001 aa")), ("1".into(), "0".into()));

    // 3. Edición en medio y una línea añadida: antes solo entraba la añadida.
    edit("linea 02002 aa", "linea 02002 dd");
    let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
    std::io::Write::write_all(&mut f, line(4000, "nueva").as_bytes()).unwrap();
    drop(f);
    env.ok(&["sync"]);
    assert_eq!((has("linea 02002 dd"), has("linea 02002 aa"), has("linea 04000 nueva")), ("1".into(), "0".into(), "1".into()));
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["4001"]);

    // Un append normal sigue siendo incremental y un sync sin cambios no relee nada.
    let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
    std::io::Write::write_all(&mut f, line(4001, "nueva").as_bytes()).unwrap();
    drop(f);
    assert!(env.ok(&["sync"]).contains("sync: 1 new events"));
    assert!(env.ok(&["sync"]).contains("0 new events"));
}

#[test]
fn cambiar_las_dims_cuenta_los_eventos_de_ficheros_fuera_del_estado() {
    let cfg = |dims: &str| format!(r#"{{"version":2,"source_options":{{"logs":{{{dims}}}}}}}"#);
    let env = Env::new("dims-fuera", &cfg(""));
    let d = env.dir().join("datos");
    let line = |host: &str| format!("{{\"ts\":\"2024-01-01T00:00:00Z\",\"msg\":\"hola {host}\",\"host\":\"{host}\"}}\n");
    write(&d, "a.jsonl", &line("h1"));
    write(&d, "b.jsonl", &line("h2"));
    env.ok(&["add", &path_s(&d.join("*.jsonl")), "--as", "jsonl", "--name", "logs"]);
    // b.jsonl desaparece y el estado no lo recuerda (índice de antes, estado perdido).
    std::fs::remove_file(d.join("b.jsonl")).unwrap();
    env.sql("DELETE FROM meta WHERE key LIKE 'glob_state:%'");

    env.set_config(&cfg(r#""dims":"host""#));
    let out = env.ok(&["sync"]);
    // Antes: «reindexing» y el evento de b.jsonl perdido.
    assert!(out.contains("1 of its events can no longer be reread") && out.contains("not reindexing"), "{out}");
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["2"], "{out}");
}

#[test]
fn cambiar_las_dims_con_un_fichero_borrado_no_pierde_sus_eventos() {
    let cfg = |dims: &str| format!(r#"{{"version":2,"source_options":{{"logs":{{{dims}}}}}}}"#);
    let env = Env::new("dims-borrado", &cfg(""));
    let d = env.dir().join("datos");
    let line = |host: &str| format!("{{\"ts\":\"2024-01-01T00:00:00Z\",\"msg\":\"hola\",\"host\":\"{host}\"}}\n");
    write(&d, "a.jsonl", &line("h1"));
    write(&d, "b.jsonl", &line("h2"));
    env.ok(&["add", &path_s(&d.join("*.jsonl")), "--as", "jsonl", "--name", "logs"]);
    std::fs::remove_file(d.join("b.jsonl")).unwrap();
    env.ok(&["sync"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["2"], "un fichero borrado conserva sus eventos");

    env.set_config(&cfg(r#""dims":"host""#));
    let out = env.ok(&["sync"]);
    assert!(out.contains("1 of its events can no longer be reread") && out.contains("not reindexing"), "{out}");
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["2"], "{out}");
    let src = env.ok(&["sources"]);
    assert!(src.contains("stale_dims_events"), "{src}");
    // Lo nuevo entra con la dim; el aviso no se repite.
    write(&d, "c.jsonl", &line("h3"));
    let out = env.ok(&["sync"]);
    assert!(!out.contains("reread") && !out.contains("reindexing"), "{out}");
    assert_eq!(env.sql("SELECT v.value FROM event_dims e JOIN dim_values v ON v.id = e.dim_id WHERE v.key = 'host'"), ["h3"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["3"]);
    // Reindexar a mano (add otra vez) sí lo rehace todo con lo que queda y quita la marca.
    env.ok(&["add", &path_s(&d.join("*.jsonl")), "--as", "jsonl", "--name", "logs"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["2"]);
    assert!(!env.ok(&["sources"]).contains("stale_dims_events"));
}

#[test]
fn init_sobre_un_indice_viejo_no_pierde_las_fuentes_anadidas() {
    let env = Env::new("init-viejo", r#"{"version":2}"#);
    let line = "{\"ts\":\"2024-01-01T00:00:00Z\",\"msg\":\"hola\"}\n";
    write(env.dir(), "app.jsonl", line);
    write(env.dir(), "otra.jsonl", line);
    let app = path_s(&env.dir().join("app.jsonl"));
    env.ok(&["init", &app]);
    env.ok(&["add", &path_s(&env.dir().join("otra.jsonl"))]);
    env.sql("UPDATE meta SET value = '5' WHERE key = 'schema_version'");

    // Con otra fuente además de la de init: no se borra nada y se manda a reindex.
    let out = env.run(&["init", &app, "--db", &env.db()]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("reindex") && err.contains("otra.jsonl") && !err.contains("from scratch"), "{err}");
    assert_eq!(env.sql("SELECT value FROM meta WHERE key = 'schema_version'"), ["5"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM sources"), ["2"]);

    // De un chrono más nuevo: tampoco se toca.
    env.sql("UPDATE meta SET value = '99' WHERE key = 'schema_version'");
    let out = env.run(&["init", &app, "--db", &env.db()]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("newer"));
    assert_eq!(env.sql("SELECT COUNT(*) FROM sources"), ["2"]);

    // Sin más fuentes que la de init, se rehace como siempre.
    env.sql("DELETE FROM sources WHERE id LIKE '%otra.jsonl'");
    env.sql("UPDATE meta SET value = '5' WHERE key = 'schema_version'");
    let out = env.ok(&["init", &app]);
    assert!(!out.contains("reindex"), "{out}");
    assert_eq!(env.sql("SELECT value FROM meta WHERE key = 'schema_version'"), ["7"]);
}

#[test]
fn init_repetido_no_borra_las_fuentes_anadidas() {
    let env = Env::new("init-repetido", r#"{"version":2}"#);
    let line = "{\"ts\":\"2024-01-01T00:00:00Z\",\"msg\":\"hola\"}\n";
    write(env.dir(), "app.jsonl", line);
    write(env.dir(), "logs/a.jsonl", &line.repeat(3));
    let app = path_s(&env.dir().join("app.jsonl"));
    env.ok(&["init", &app]);
    env.ok(&["add", &path_s(&env.dir().join("logs/*.jsonl")), "--as", "jsonl"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["4"]);

    // Antes: 1 fuente y los 3 eventos de la glob perdidos, sin aviso.
    let out = env.run(&["init", &app, "--db", &env.db()]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("would drop") && err.contains("logs/*.jsonl") && err.contains("sync"), "{err}");
    assert_eq!(env.sql("SELECT COUNT(*) FROM sources"), ["2"]);
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["4"]);

    // Con solo su fuente, init se repite como siempre.
    let solo = Env::new("init-repetido-solo", r#"{"version":2}"#);
    write(solo.dir(), "app.jsonl", line);
    let app = path_s(&solo.dir().join("app.jsonl"));
    solo.ok(&["init", &app]);
    solo.ok(&["init", &app]);
    assert_eq!(solo.sql("SELECT COUNT(*) FROM events"), ["1"]);
}

#[test]
fn init_aparta_un_indice_danado_y_no_toca_uno_que_no_puede_abrir() {
    let env = Env::new("init-danado", r#"{"version":2}"#);
    write(env.dir(), "app.jsonl", "{\"ts\":\"2024-01-01T00:00:00Z\",\"msg\":\"hola\"}\n");
    let app = path_s(&env.dir().join("app.jsonl"));
    // Algo que no es un índice: se guarda aparte (no se borra) y se crea otro.
    std::fs::write(env.db(), "esto no es sqlite, pero es mío ".repeat(200)).unwrap();
    let out = env.ok(&["init", &app]);
    assert!(out.contains("corrupt.bak"), "{out}");
    let bak = std::fs::read_to_string(format!("{}.corrupt.bak", env.db())).unwrap();
    assert!(bak.starts_with("esto no es sqlite"));
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["1"]);

    // Sin permisos de lectura (como root se leería igual: no hay nada que probar).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let size = std::fs::metadata(env.db()).unwrap().len();
        std::fs::set_permissions(env.db(), std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(env.db()).is_err() {
            let out = env.run(&["init", &app, "--db", &env.db()]);
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(!out.status.success() && err.contains("Nothing was touched"), "{err}");
            std::fs::set_permissions(env.db(), std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(std::fs::metadata(env.db()).unwrap().len(), size);
            assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["1"]);
        }
        std::fs::set_permissions(env.db(), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}

/// `tail` durante un ciclo y se mata (no termina solo).
fn tail_once(env: &Env, then: impl FnOnce()) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["tail", "--interval", "1s", "--db", &env.db()])
        .current_dir(env.dir())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    then();
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn una_fuente_borrada_no_para_sync_ni_tail() {
    let env = Env::new("fuente-borrada", r#"{"version":2}"#);
    let line = |i: u32| format!(r#"{{"ts":"2024-01-01T00:00:{i:02}Z","msg":"hola {i}"}}"#) + "\n";
    write(env.dir(), "a.jsonl", &(line(1) + &line(2)));
    write(env.dir(), "b.jsonl", &line(11));
    let (a, b) = (env.dir().join("a.jsonl"), env.dir().join("b.jsonl"));
    env.ok(&["add", &path_s(&a)]);
    env.ok(&["add", &path_s(&b)]);
    std::fs::remove_file(&a).unwrap();
    let append = |i: u32| {
        let mut f = std::fs::OpenOptions::new().append(true).open(&b).unwrap();
        std::io::Write::write_all(&mut f, line(i).as_bytes()).unwrap();
    };
    append(12);

    // Antes: error en a.jsonl y b.jsonl sin su línea nueva.
    let out = env.run(&["sync", "--db", &env.db()]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("a.jsonl") && err.contains("are kept"), "{err}");
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["4"], "{err}");
    let src = env.ok(&["sources"]);
    assert_eq!(src.matches("sync_error").count(), 1, "{src}");

    // `tail` tampoco se para: sigue trayendo lo nuevo de b.
    tail_once(&env, || append(13));
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["5"]);

    // Vuelve el fichero: el sync sale bien y el error desaparece de `sources`.
    write(env.dir(), "a.jsonl", &(line(1) + &line(2)));
    env.ok(&["sync"]);
    assert!(!env.ok(&["sources"]).contains("sync_error"));
}

#[test]
fn tail_deja_rollups_plantillas_y_busqueda_como_un_sync() {
    let env = Env::new("tail", r#"{"version":2}"#);
    let log = env.dir().join("app.jsonl");
    let line = |i: u32, msg: &str| format!(r#"{{"ts":"2024-01-01T00:00:{i:02}Z","level":"ERROR","msg":"{msg} {i}"}}"#) + "\n";
    write(env.dir(), "app.jsonl", &line(1, "fallo de disco"));
    env.ok(&["add", &path_s(&log)]);
    tail_once(&env, || {
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut f, (line(2, "fallo de disco") + &line(3, "pinchazo")).as_bytes()).unwrap();
    });
    assert_eq!(env.sql("SELECT COUNT(*) FROM events"), ["3"]);
    let max = env.sql("SELECT MAX(rid) FROM events");
    assert_eq!(env.sql("SELECT value FROM meta WHERE key = 'rollup_max_rowid'"), max, "rollups al día");
    assert_eq!(env.sql("SELECT value FROM meta WHERE key = 'fts_max_rowid'"), max, "búsqueda al día");
    assert_eq!(env.sql("SELECT COUNT(*) FROM events_fts WHERE events_fts MATCH 'pinchazo'"), ["1"]);
    assert_eq!(env.sql("SELECT SUM(count) FROM dim_rollups"), ["3"]);
    let pats = env.ok(&["patterns"]);
    assert!(pats.contains("pinchazo"), "{pats}");
}
