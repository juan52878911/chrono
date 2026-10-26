//! Índice de otra versión de esquema: el error lo dice en lenguaje de usuario
//! con el comando exacto, y `chrono reindex` lo reconstruye con las mismas
//! fuentes (también las añadidas con `add`, que `init` perdía). El `.db` se
//! toca con el módulo `sqlite3` de python3 (chrono-cli no depende de rusqlite).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(p: &Path, contents: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, contents).unwrap();
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_chrono")).args(args).current_dir(dir).env("CHRONO_LANG", "en").output().unwrap()
}

fn sql(db: &Path, sql: &str) -> Vec<String> {
    let py = "import sqlite3,sys\n\
              c=sqlite3.connect(sys.argv[1])\n\
              for r in c.execute(sys.argv[2]):\n print('|'.join('' if v is None else str(v) for v in r))\n\
              c.commit()\n";
    let out = Command::new("python3").args(["-I", "-c", py]).arg(db).arg(sql).output().expect("python3");
    assert!(out.status.success(), "sql {sql}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

const LINEAS: &str = concat!(
    r#"{"ts":"2026-01-01T00:00:00Z","level":"ERROR","msg":"falla A"}"#,
    "\n",
    r#"{"ts":"2026-01-01T00:00:01Z","level":"INFO","msg":"va bien"}"#,
    "\n"
);

#[test]
fn esquema_viejo_dice_el_comando_y_reindex_recupera_todas_las_fuentes() {
    let tmp = std::env::temp_dir().join(format!("chrono-reindex-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let dir = TempDir(std::fs::canonicalize(&tmp).unwrap());
    let root = &dir.0;
    let db = root.join(".chrono/index.db");
    let db_s = db.to_string_lossy().into_owned();
    write(&root.join("logs/app.jsonl"), LINEAS);
    write(&root.join("sesiones/a/1.jsonl"), LINEAS);
    write(&root.join("sesiones/b/2.jsonl"), LINEAS);
    let glob = format!("{}/sesiones/**/*.jsonl", root.display());
    for args in [vec!["add", "logs/app.jsonl"], vec!["add", glob.as_str(), "--name", "ses"]] {
        let mut a = args.clone();
        a.extend(["--db", &db_s]);
        let out = run(root, &a);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let antes = sql(&db, "SELECT id FROM sources ORDER BY ref");
    assert_eq!(antes.len(), 2);
    let n_antes = sql(&db, "SELECT COUNT(*) FROM events");

    sql(&db, "UPDATE meta SET value = '5' WHERE key = 'schema_version'");
    let out = run(root, &["sources", "--db", &db_s]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("schema v5") && err.contains("needs v7"), "{err}");
    assert!(err.contains(&format!("chrono --db {db_s} reindex")), "{err}");
    assert!(!err.contains("Some(") && !err.contains("Store::reset"), "detalles internos: {err}");
    // Sin --db (autodescubierto), el comando también va sin él.
    let out = run(root, &["sources"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("chrono reindex\n"), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run(root, &["reindex"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(sql(&db, "SELECT value FROM meta WHERE key = 'schema_version'"), ["7"]);
    assert_eq!(sql(&db, "SELECT id FROM sources ORDER BY ref"), antes);
    assert_eq!(sql(&db, "SELECT COUNT(*) FROM events"), n_antes);
    // El alias de la fuente glob se conserva.
    assert!(sql(&db, "SELECT manifest_json FROM sources").iter().any(|m| m.contains("\"alias\":\"ses\"")));
    // Acabado bien, no queda la copia del índice viejo.
    let restos: Vec<_> = std::fs::read_dir(root.join(".chrono")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".bak")).collect();
    assert!(restos.is_empty(), "{restos:?}");
    assert!(run(root, &["sources"]).status.success());
}
