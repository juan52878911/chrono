//! Tests de `chrono-tracker-github`.
//!
//! `fetch` (la ruta de red que invoca `gh` de verdad) NO se testea aquí: para
//! ejercitarlo con fidelidad haría falta `gh` instalado, autenticado y un
//! repo real en GitHub — no es un fixture reproducible en CI. En su lugar:
//! - `check` se prueba contra un repo temporal SIN remoto GitHub (`NoRemote`).
//! - El *parseo* de la salida `gh --json` se prueba por separado, alimentando
//!   JSON de ejemplo directamente a la función de parseo interna.
//! - `store_issues` se prueba contra un `chrono_store::Store` temporal.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{check, parse_items, slug, store_issues, Item, Status};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Directorio temporal único, borrado al hacer `Drop`.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir()
            .join(format!("chrono-tracker-github-test-{name}-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("crear dir temporal");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Único fichero SQLite temporal, borrado (db + -wal/-shm) al hacer `Drop`.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir()
            .join(format!("chrono-tracker-github-test-{name}-{}-{}.db", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        TempDb { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// Repo git temporal con un remoto `origin` que NO es github.com.
fn repo_sin_remoto_github(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    let status = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .arg("init")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git init");
    assert!(status.success(), "git init falló");

    let status = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["remote", "add", "origin", "https://example.com/x"])
        .status()
        .expect("git remote add");
    assert!(status.success(), "git remote add falló");

    dir
}

fn gh_instalado() -> bool {
    Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[test]
fn slug_sin_remoto_github_devuelve_none() {
    let repo = repo_sin_remoto_github("slug-none");
    assert_eq!(slug(repo.path()), None);
}

// NOTA: `check` en el caso `NoGh` no se prueba aquí: simularlo exigiría
// vaciar el PATH del proceso de test, que es un estado GLOBAL compartido con
// todos los hilos de test que corren en paralelo (incl. los que invocan
// `git`/`gh` en otros tests de este mismo archivo) — mutarlo de forma segura
// necesitaría serializar toda la suite. Queda cubierto solo por inspección:
// `check` delega en `gh_in_path`, que ya se ejerce indirectamente por
// `check_sin_remoto_github_es_no_remote_si_hay_gh` cuando `gh` SÍ está
// instalado (si no lo estuviera, ese mismo test vería `NoGh` y lo reporta).
#[test]
fn check_sin_remoto_github_es_no_remote_si_hay_gh() {
    let repo = repo_sin_remoto_github("check-noremote");
    let status = check(repo.path());
    if !gh_instalado() {
        assert_eq!(status, Status::NoGh, "sin `gh` en PATH, check debe ser NoGh");
        eprintln!("`gh` no está instalado en este entorno; se omite la aserción de NoRemote.");
        return;
    }
    assert_eq!(status, Status::NoRemote);
}

// --- Parseo de la salida `gh --json` -------------------------------------

const PRS_JSON: &str = r#"[
  {"number": 10, "title": "Arregla el parser", "state": "MERGED",
   "mergedAt": "2026-01-05T10:00:00Z",
   "labels": [{"name": "bug"}, {"name": "rust"}]},
  {"number": 11, "title": "Refactor menor", "state": "OPEN",
   "mergedAt": "",
   "labels": []}
]"#;

const ISSUES_JSON: &str = r#"[
  {"number": 3, "title": "Crash al abrir", "state": "CLOSED",
   "closedAt": "2026-02-01T00:00:00Z",
   "labels": [{"name": "bug"}]},
  {"number": 4, "title": "Pregunta de uso", "state": "OPEN",
   "closedAt": "",
   "labels": [{"name": "question"}]}
]"#;

#[test]
fn parse_items_prs_produce_items_correctos() {
    let items = parse_items(PRS_JSON.as_bytes(), "pr").expect("parsea PRs");
    assert_eq!(items.len(), 2);

    assert_eq!(items[0].number, 10);
    assert_eq!(items[0].kind, "pr");
    assert_eq!(items[0].title, "Arregla el parser");
    assert_eq!(items[0].state, "MERGED");
    assert!(items[0].merged);
    assert_eq!(items[0].closed_at, "2026-01-05T10:00:00Z");
    assert_eq!(items[0].labels, vec!["bug".to_string(), "rust".to_string()]);

    assert_eq!(items[1].number, 11);
    assert!(!items[1].merged);
    assert!(items[1].labels.is_empty());
}

#[test]
fn parse_items_issues_produce_items_correctos() {
    let items = parse_items(ISSUES_JSON.as_bytes(), "issue").expect("parsea issues");
    assert_eq!(items.len(), 2);

    assert_eq!(items[0].number, 3);
    assert_eq!(items[0].kind, "issue");
    assert!(!items[0].merged);
    assert_eq!(items[0].closed_at, "2026-02-01T00:00:00Z");
    assert_eq!(items[0].labels, vec!["bug".to_string()]);

    assert_eq!(items[1].number, 4);
    assert_eq!(items[1].labels, vec!["question".to_string()]);
    assert_eq!(items[1].closed_at, "");
}

#[test]
fn parse_items_json_invalido_devuelve_error() {
    let err = parse_items(b"no es json", "pr").unwrap_err();
    assert!(err.to_string().contains("parseando salida"));
}

// --- store_issues ----------------------------------------------------------

fn item(number: i64, kind: &str, title: &str, labels: &[&str], merged: bool) -> Item {
    Item {
        number,
        kind: kind.to_string(),
        title: title.to_string(),
        state: "OPEN".to_string(),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        merged,
        closed_at: String::new(),
    }
}

#[test]
fn store_issues_inserta_y_marca_is_bug_segun_labels() {
    let db = TempDb::new("store-basic");
    let store = chrono_store::Store::open(db.path()).expect("abre store");

    let items = vec![
        item(1, "pr", "Fix crash", &["bug", "rust"], true),
        item(2, "issue", "Nice to have", &["enhancement"], false),
        item(3, "issue", "Otro bug", &["defecto"], false),
    ];
    let bug_labels = vec!["bug".to_string(), "defecto".to_string()];

    store_issues(&store, &items, &bug_labels).expect("store_issues");

    let conn = store.conn();
    let mut stmt = conn
        .prepare("SELECT number, kind, title, labels, merged, is_bug FROM issues ORDER BY kind, number")
        .unwrap();
    let rows: Vec<(i64, String, String, String, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(rows.len(), 3);

    let issue2 = rows.iter().find(|r| r.0 == 2 && r.1 == "issue").unwrap();
    assert_eq!(issue2.5, 0, "enhancement no es bug");

    let issue3 = rows.iter().find(|r| r.0 == 3 && r.1 == "issue").unwrap();
    assert_eq!(issue3.5, 1, "'defecto' está en bug_labels");

    let pr1 = rows.iter().find(|r| r.0 == 1 && r.1 == "pr").unwrap();
    assert_eq!(pr1.4, 1, "merged=true");
    assert_eq!(pr1.5, 1, "'bug' está en bug_labels");
    assert_eq!(pr1.3, "bug,rust");
}

#[test]
fn store_issues_es_idempotente_upsert_actualiza_no_duplica() {
    let db = TempDb::new("store-upsert");
    let store = chrono_store::Store::open(db.path()).expect("abre store");
    let bug_labels = vec!["bug".to_string()];

    let v1 = vec![item(5, "pr", "Titulo viejo", &[], false)];
    store_issues(&store, &v1, &bug_labels).expect("primer store_issues");

    let v2 = vec![item(5, "pr", "Titulo nuevo", &["bug"], true)];
    store_issues(&store, &v2, &bug_labels).expect("segundo store_issues (upsert)");

    let conn = store.conn();
    let count: i64 = conn.query_row("SELECT count(*) FROM issues", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "el upsert no debe duplicar filas");

    let (title, merged, is_bug): (String, i64, i64) = conn
        .query_row(
            "SELECT title, merged, is_bug FROM issues WHERE kind='pr' AND number=5",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(title, "Titulo nuevo");
    assert_eq!(merged, 1);
    assert_eq!(is_bug, 1);
}
