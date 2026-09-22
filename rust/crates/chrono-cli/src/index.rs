//! `init` y `sync`: orquestación de la ingesta (Source → Store), la
//! clasificación (Level-0 reglas) de cada evento, el enriquecimiento final
//! (tamaños de HEAD, borrados, exclusiones, FTS, meta) y el forge (PRs/issues
//! de GitHub vía `gh`, si está disponible). Réplica de
//! `ingest.Run`/`ingest.Sync`/`finalize` del Go (R2: incluye forge).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono_classify_rules::RulesClassifier;
use chrono_core::{Classifier, CoreError, Cursor, Source, SourceConfig, Watermark};
use chrono_source_git::{blob_lines, ls_tree, GitSource};
use chrono_store::Store;
use chrono_tracker_github as tracker;

use crate::glob;
use crate::i18n::t;

/// Nº máximo de ficheros a los que se les calcula el tamaño (los más cambiados).
const SIZE_CAP: usize = 4000;
/// Cada cuántos commits se refresca el indicador de progreso (como el Go).
const PROGRESS_EVERY: usize = 2000;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Raíz del repo git que contiene `start` (`git rev-parse --show-toplevel`).
pub fn repo_root(start: &Path) -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !out.status.success() {
        return Err(t(
            format!("{} is not inside a git repo", start.display()),
            format!("{} no está dentro de un repo git", start.display()),
        )
        .into());
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// `chrono init [repo]`: índice completo desde cero en `<repo>/.chrono/index.db`.
pub fn init(start: &Path) -> Result<()> {
    let src = GitSource::new();
    if src.detect(start) == 0 {
        return Err(t(
            format!("{} is not inside a git repo", start.display()),
            format!("{} no está dentro de un repo git", start.display()),
        )
        .into());
    }
    let root = repo_root(start)?;
    let dir = root.join(".chrono");
    std::fs::create_dir_all(&dir)?;
    // Todo .chrono/ se ignora en git salvo la config (esa sí conviene versionarla).
    let _ = std::fs::write(dir.join(".gitignore"), "*\n!config.json\n");
    let db = dir.join("index.db");

    let mut store = open_or_recreate(&db)?;
    store.reset()?;
    let cfg = chrono_classify_rules::load(&root);
    let classifier = RulesClassifier::new(cfg.clone());
    let n = ingest(&src, &root, &mut store, None, &classifier).map_err(|e| e.into_boxed())?;
    finalize(&root, &store, &cfg.bug_labels)?;
    eprintln!("{}", t(
        format!("chrono: index ready at .chrono/index.db ({n} commits)."),
        format!("chrono: índice listo en .chrono/index.db ({n} commits)."),
    ));
    eprintln!("{}", t(
        "Now, inside the repo: chrono hotspots · chrono bugs · chrono sync",
        "Ahora, dentro del repo: chrono hotspots · chrono bugs · chrono sync",
    ));
    Ok(())
}

/// `chrono sync [repo]`: ingiere solo el delta desde el último watermark.
/// Si la fuente diverge (rebase/force-push), avisa y reindexa entero.
pub fn sync(db: &Path, repo_arg: Option<&Path>) -> Result<()> {
    let mut store = Store::open(db)?;
    let root: PathBuf = match repo_arg {
        Some(p) => repo_root(p)?,
        None => match store.meta("repo_path")? {
            Some(rp) if !rp.is_empty() => PathBuf::from(rp),
            _ => {
                return Err(t(
                    "don't know which repo to sync (no argument and no repo_path in the index)",
                    "no sé qué repo sincronizar (ni argumento ni repo_path en el índice)",
                )
                .into())
            }
        },
    };
    let src = GitSource::new();
    let wm = store.meta("last_watermark")?.map(|v| Watermark { kind: "git".into(), value: v });

    let cfg = chrono_classify_rules::load(&root);
    let classifier = RulesClassifier::new(cfg.clone());

    let head = chrono_source_git::head_sha(&root)?;
    if let Some(ref w) = wm {
        if w.value == format!("sha:{head}") {
            eprintln!("{}", t("sync: 0 new commits", "sync: 0 commits nuevos"));
            return Ok(());
        }
    }

    let n = match ingest(&src, &root, &mut store, wm.clone(), &classifier) {
        Ok(n) => n,
        Err(IngestError::Diverged) => {
            eprintln!("{}", t(
                "chrono: divergence detected (rebase/force-push) -> full reindex",
                "chrono: divergencia detectada (rebase/force-push) -> reindex completo",
            ));
            store.reset()?;
            ingest(&src, &root, &mut store, None, &classifier).map_err(|e| e.into_boxed())?
        }
        Err(e) => return Err(e.into_boxed()),
    };
    finalize(&root, &store, &cfg.bug_labels)?;
    eprintln!("{}", t(format!("sync: {n} new commits"), format!("sync: {n} commits nuevos")));
    Ok(())
}

enum IngestError {
    Diverged,
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl IngestError {
    fn into_boxed(self) -> Box<dyn std::error::Error + Send + Sync> {
        match self {
            IngestError::Diverged => "fuente divergente".into(),
            IngestError::Other(e) => e,
        }
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for IngestError {
    fn from(e: Box<dyn std::error::Error + Send + Sync>) -> Self {
        IngestError::Other(e)
    }
}

/// Abre el índice; si su `schema_version` no es la actual (índice v1 del Go,
/// por ejemplo), lo borra y lo crea de nuevo: reindex avisado, no migración.
fn open_or_recreate(db: &Path) -> Result<Store> {
    match Store::open(db) {
        Ok(s) => Ok(s),
        Err(e) if db.exists() => {
            eprintln!("{}", t(
                format!("chrono: incompatible index ({e}); rebuilding from scratch"),
                format!("chrono: índice incompatible ({e}); se reconstruye desde cero"),
            ));
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
            }
            Ok(Store::open(db)?)
        }
        Err(e) => Err(e),
    }
}

/// Drena el `Cursor` de la fuente al `Writer` en una sola transacción.
/// Devuelve el nº de eventos ingeridos.
fn ingest(
    src: &GitSource,
    root: &Path,
    store: &mut Store,
    watermark: Option<Watermark>,
    classifier: &RulesClassifier,
) -> std::result::Result<usize, IngestError> {
    let cfg = SourceConfig::default();
    let mut cur: Box<dyn Cursor> = match src.open(root, watermark, &cfg) {
        Ok(c) => c,
        Err(CoreError::Diverged(_)) => return Err(IngestError::Diverged),
        Err(e) => return Err(IngestError::Other(e.to_string().into())),
    };

    let mut count = 0usize;
    let mut stderr = std::io::stderr();
    {
        let mut w = store.writer()?;
        loop {
            let ev = match cur.next() {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(e) => return Err(IngestError::Other(e.to_string().into())),
            };
            w.add_event(&ev)?;
            let labels = classifier.classify(&ev);
            w.add_labels(&ev.id, &labels)?;
            count += 1;
            if count.is_multiple_of(PROGRESS_EVERY) {
                let _ = write!(stderr, "\r  {count} commits…");
                let _ = stderr.flush();
            }
        }
        if count >= PROGRESS_EVERY {
            let _ = writeln!(stderr, "\r  {count} commits");
        }
        w.commit()?;
    }

    let wm = cur.watermark();
    store.set_meta("last_watermark", &wm.value)?;
    for (k, v) in cur.manifest() {
        store.set_meta(&k, &v)?;
    }
    Ok(count)
}

/// Enriquecimiento tras la ingesta: borrados, tamaños de HEAD (top más
/// cambiados, con caché por OID), exclusiones, FTS, forge (PRs/issues de
/// GitHub vía `gh`, si está disponible), `meta` y compactado.
fn finalize(root: &Path, store: &Store, bug_labels: &[String]) -> Result<()> {
    eprintln!("{}", t("  computing file sizes…", "  calculando tamaños de fichero…"));
    let tree = ls_tree(root)?;
    let path_oid: HashMap<&str, &str> =
        tree.iter().map(|(p, o)| (p.as_str(), o.as_str())).collect();

    let conn = store.conn();

    // 1. Borrados: toda entidad `file` que no esté en HEAD.
    {
        let tx = conn.unchecked_transaction()?;
        tx.execute("UPDATE entities SET deleted = 1 WHERE type = 'file'", [])?;
        let mut stmt = tx.prepare("UPDATE entities SET deleted = 0 WHERE key = ?1")?;
        for (p, _) in &tree {
            stmt.execute([p.as_str()])?;
        }
        drop(stmt);
        tx.commit()?;
    }

    // 2. Tamaños: solo los SIZE_CAP más cambiados que siguen en HEAD.
    let top = store.top_changed_entities(SIZE_CAP)?;
    let mut oid_set: HashSet<String> = HashSet::new();
    for p in &top {
        if let Some(oid) = path_oid.get(p.as_str()) {
            oid_set.insert((*oid).to_string());
        }
    }
    let oids: Vec<String> = oid_set.into_iter().collect();
    let mut cached = store.get_blob_lines(&oids)?;
    let missing: Vec<String> = oids.iter().filter(|o| !cached.contains_key(*o)).cloned().collect();
    if !missing.is_empty() {
        let fresh = blob_lines(root, &missing)?;
        store.put_blob_lines(&fresh)?;
        for (o, n) in fresh {
            cached.insert(o, n);
        }
    }
    {
        let tx = conn.unchecked_transaction()?;
        for p in &top {
            if let Some(oid) = path_oid.get(p.as_str()) {
                if let Some(&n) = cached.get(*oid) {
                    tx.execute("UPDATE entities SET size = ?1 WHERE key = ?2", (n, p.as_str()))?;
                }
            }
        }
        tx.commit()?;
    }

    // 3. Exclusiones de ruido (globs por defecto del Go; sin config en R1).
    {
        let tx = conn.unchecked_transaction()?;
        tx.execute("UPDATE entities SET excluded = 0", [])?;
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id, key FROM entities")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            let mut ids = Vec::new();
            for row in rows {
                let (id, key) = row?;
                if glob::match_any(glob::DEFAULT_EXCLUDE_GLOBS, &key) {
                    ids.push(id);
                }
            }
            ids
        };
        let mut stmt = tx.prepare("UPDATE entities SET excluded = 1 WHERE id = ?1")?;
        for id in ids {
            stmt.execute([id])?;
        }
        drop(stmt);
        tx.commit()?;
    }

    // 3b. Tags de git → markers (para `phases`).
    {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "for-each-ref",
                "--sort=creatordate",
                "--format=%(refname:short)\x1f%(creatordate:short)\x1f%(objectname:short)",
                "refs/tags",
            ])
            .output();
        if let Ok(out) = out {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                let src = format!("git:{}", root.display());
                let conn = store.conn();
                for line in text.lines() {
                    let mut it = line.split('\u{1f}');
                    let name = it.next().unwrap_or("");
                    let at = it.next().unwrap_or("");
                    let refv = it.next().unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    conn.execute(
                        "INSERT OR REPLACE INTO markers(source_id,name,at,ref,kind) VALUES (?1,?2,?3,?4,'tag')",
                        (src.as_str(), name, at, refv),
                    )?;
                }
            }
        }
    }

    eprintln!("{}", t("  building search index…", "  construyendo índice de búsqueda…"));
    store.rebuild_fts()?;

    // 4. Forge (opcional): PRs/issues de GitHub vía `gh`, si está disponible.
    // Nunca falla el índice: si `gh` no está, no hay remoto o no está
    // autenticado, se avisa con precisión y se sigue.
    match tracker::check(root) {
        tracker::Status::NoGh => {
            eprintln!("{}", t(
                "  warning: 'gh' not found — skipping PRs/issues (bug-by-label). Install GitHub CLI to enable: https://cli.github.com",
                "  aviso: no está 'gh' — se omiten PRs/issues (bug por label). Instala GitHub CLI para activarlo: https://cli.github.com",
            ));
            store.set_meta("forge_status", "no_gh")?;
        }
        tracker::Status::NoRemote => {
            eprintln!("{}", t(
                "  warning: no GitHub remote (upstream/origin) — skipping PRs/issues.",
                "  aviso: no hay remoto GitHub (upstream/origin) — se omiten PRs/issues.",
            ));
            store.set_meta("forge_status", "no_remote")?;
        }
        tracker::Status::NoAuth => {
            eprintln!("{}", t(
                "  warning: 'gh' is not authenticated — skipping PRs/issues. Run: gh auth login",
                "  aviso: 'gh' no está autenticado — se omiten PRs/issues. Ejecuta: gh auth login",
            ));
            store.set_meta("forge_status", "no_auth")?;
        }
        tracker::Status::Ok => {
            eprintln!("{}", t("  fetching PRs/issues…", "  trayendo PRs/issues…"));
            match tracker::fetch(root, 1000) {
                Ok((items, nwo)) => match tracker::store_issues(store, &items, bug_labels) {
                    Ok(()) => {
                        eprintln!("{}", t(
                            format!("  forge: {} PRs/issues from {nwo}", items.len()),
                            format!("  forge: {} PRs/issues de {nwo}", items.len()),
                        ));
                        store.set_meta("forge_status", "ok")?;
                    }
                    Err(e) => {
                        eprintln!("{}", t(
                            format!("  warning: could not store PRs/issues: {e}"),
                            format!("  aviso: no se pudieron guardar PRs/issues: {e}"),
                        ));
                        store.set_meta("forge_status", "store_error")?;
                    }
                },
                Err(e) => {
                    eprintln!("{}", t(
                        format!("  note: forge skipped: {e}"),
                        format!("  aviso: forge omitido: {e}"),
                    ));
                    store.set_meta("forge_status", "fetch_error")?;
                }
            }
        }
    }

    let abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    store.set_meta("schema_version", &chrono_store::SCHEMA_VERSION.to_string())?;
    store.set_meta("repo_path", &abs.to_string_lossy())?;
    store.set_meta("source_id", &format!("git:{}", abs.to_string_lossy()))?;
    store.optimize()?;
    Ok(())
}
