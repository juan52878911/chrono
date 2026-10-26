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

#[test]
fn reindex_con_menos_eventos_avisa_y_conserva_la_copia() {
    let tmp = std::env::temp_dir().join(format!("chrono-reindex-menos-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let dir = TempDir(std::fs::canonicalize(&tmp).unwrap());
    let root = &dir.0;
    let db = root.join(".chrono/index.db");
    let db_s = db.to_string_lossy().into_owned();
    write(&root.join("logs/app.jsonl"), LINEAS);
    let out = run(root, &["add", "logs/app.jsonl", "--db", &db_s]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(sql(&db, "SELECT COUNT(*) FROM events"), ["2"]);

    // El log se truncó (rotado) desde la última ingesta.
    write(&root.join("logs/app.jsonl"), LINEAS.lines().next().unwrap());
    sql(&db, "UPDATE meta SET value = '5' WHERE key = 'schema_version'");
    let out = run(root, &["reindex"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("fewer events than before (2 → 1)"), "{err}");
    assert_eq!(sql(&db, "SELECT COUNT(*) FROM events"), ["1"]);
    let bak = root.join(".chrono/index.db.v5.bak");
    assert!(bak.exists(), "la copia con los 2 eventos debe quedarse");
    assert_eq!(sql(&bak, "SELECT COUNT(*) FROM events"), ["2"]);
}

/// Raíz temporal con un log grande (la reindexación tarda) y otro pequeño.
fn dos_fuentes(tag: &str, lineas: usize) -> (TempDir, PathBuf) {
    let tmp = std::env::temp_dir().join(format!("chrono-reindex-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let dir = TempDir(std::fs::canonicalize(&tmp).unwrap());
    let root = dir.0.clone();
    let db = root.join(".chrono/index.db");
    let grande: String = (0..lineas).map(|i| format!("{{\"ts\":\"2026-01-01T00:00:00Z\",\"msg\":\"linea {i}\"}}\n")).collect();
    write(&root.join("logs/grande.jsonl"), &grande);
    write(&root.join("logs/app.jsonl"), LINEAS);
    for f in ["logs/grande.jsonl", "logs/app.jsonl"] {
        let out = run(&root, &["add", f, "--db", &db.to_string_lossy()]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    (dir, db)
}

fn baks(db: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.contains(".bak") || n.ends_with(".reindexing"))
        .collect();
    v.sort();
    v
}

#[test]
fn dos_reindex_a_la_vez_no_pierden_fuentes() {
    let (dir, db) = dos_fuentes("a-la-vez", 60_000);
    let n_antes = sql(&db, "SELECT COUNT(*) FROM events");
    let db_s = db.to_string_lossy().into_owned();
    // Antes: el segundo apartaba el índice a medias del primero y se perdía una fuente.
    let hijos: Vec<_> = (0..2)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_chrono"))
                .args(["reindex", "--db", &db_s])
                .current_dir(&dir.0)
                .env("CHRONO_LANG", "en")
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for h in hijos {
        let out = h.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(sql(&db, "SELECT COUNT(*) FROM sources"), ["2"]);
    assert_eq!(sql(&db, "SELECT COUNT(*) FROM events"), n_antes);
    assert!(baks(&db).is_empty(), "{:?}", baks(&db));
}

#[test]
fn un_reindex_cortado_se_dice_y_el_siguiente_lo_reanuda_desde_la_copia() {
    let (dir, db) = dos_fuentes("cortado", 10);
    let root = &dir.0;
    let db_s = db.to_string_lossy().into_owned();
    let n_antes = sql(&db, "SELECT COUNT(*) FROM events");
    // Una copia que alguien conservó de antes no se pisa.
    let vieja = PathBuf::from(format!("{db_s}.v7.bak"));
    std::fs::write(&vieja, "copia de otro día").unwrap();

    // Como un reindex muerto a medias: la copia buena aparte, la marca puesta y
    // un índice con solo una de las dos fuentes.
    let copia = PathBuf::from(format!("{db_s}.v7.2.bak"));
    std::fs::copy(&db, &copia).unwrap();
    sql(&db, "DELETE FROM events WHERE source_ref = (SELECT ref FROM sources WHERE id LIKE '%app.jsonl')");
    sql(&db, "DELETE FROM sources WHERE id LIKE '%app.jsonl'");
    std::fs::write(format!("{db_s}.reindexing"), copia.to_string_lossy().as_bytes()).unwrap();

    // Las consultas lo avisan y las ingestas no tocan el índice a medias.
    let out = run(root, &["sources", "--db", &db_s]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("did not finish") && err.contains(&*copia.to_string_lossy()), "{err}");
    let out = run(root, &["sync", "--db", &db_s]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("did not finish"));

    // `reindex` reanuda desde la copia buena (antes tomaba el índice a medias por bueno).
    let out = run(root, &["reindex", "--db", &db_s]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.contains("resuming"), "{err}");
    assert_eq!(sql(&db, "SELECT COUNT(*) FROM sources"), ["2"]);
    assert_eq!(sql(&db, "SELECT COUNT(*) FROM events"), n_antes);
    assert_eq!(baks(&db), ["index.db.v7.bak"], "solo queda la copia de otro día");
    assert_eq!(std::fs::read_to_string(&vieja).unwrap(), "copia de otro día");

    // Un reindex normal con esa copia ahí tampoco la pisa.
    let out = run(root, &["reindex", "--db", &db_s]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&vieja).unwrap(), "copia de otro día");
    assert_eq!(sql(&db, "SELECT COUNT(*) FROM events"), n_antes);
}

#[test]
fn una_consulta_sobre_una_ruta_sin_indice_no_crea_uno_vacio() {
    let tmp = std::env::temp_dir().join(format!("chrono-reindex-sin-indice-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let dir = TempDir(tmp);
    let db = dir.0.join("nada.db");
    let out = run(&dir.0, &["top", "level", "--db", &db.to_string_lossy()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("chrono init"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!db.exists(), "antes quedaba un .db vacío");
}
