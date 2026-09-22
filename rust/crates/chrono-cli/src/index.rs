//! `init` y `sync`: orquestación de la ingesta (Source → Store), la
//! clasificación (Level-0 reglas) de cada evento, el enriquecimiento final
//! (tamaños de HEAD, borrados, exclusiones, FTS, meta) y el forge (PRs/issues
//! de GitHub vía `gh`, si está disponible). Réplica de
//! `ingest.Run`/`ingest.Sync`/`finalize` del Go (R2: incluye forge).
//!
//! R3: la ingesta (`ingest_source`) es fuente-agnóstica — funciona con
//! cualquier `chrono_core::Source`, no solo git. Los pasos que solo tienen
//! sentido para git (tamaños de fichero, tags→markers, forge de GitHub)
//! quedan gateados por `source.kind() == "git"` en `finalize`.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono_classify_rules::RulesClassifier;
use chrono_core::{Classifier, CoreError, Cursor, Registry, Source, SourceConfig, Watermark};
use chrono_source_git::{blob_lines, ls_tree, GitSource};
use chrono_store::Store;
use chrono_tracker_github as tracker;

use crate::glob;
use crate::i18n::t;

/// Nº máximo de ficheros a los que se les calcula el tamaño (los más cambiados).
const SIZE_CAP: usize = 4000;
/// Cada cuántos eventos se refresca el indicador de progreso (como el Go).
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

/// Error accionable cuando ninguna fuente registrada reconoce `path`.
fn unrecognized_source_err(path: &Path) -> Box<dyn std::error::Error + Send + Sync> {
    t(
        format!("I don't recognize this source at {}; is it a git repo or a .jsonl file?", path.display()),
        format!("no reconozco esta fuente en {}; ¿es un repo git o un .jsonl?", path.display()),
    )
    .into()
}

/// Registro de fuentes disponibles para `init`/`sync`. `pick` elige la de
/// mayor confianza vía `Source::detect`.
fn build_registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(GitSource::new()));
    r.register(Box::new(chrono_source_jsonl::JsonlSource::new()));
    // R3: registrar aquí más adaptadores (csv, textlog, journald…).
    r
}

/// Dada una fuente ya elegida y el `start` que pasó el usuario, resuelve:
/// - `ingest_path`: lo que se le pasa a `Source::open` (para git, el
///   toplevel del repo; para una fuente de fichero, el fichero mismo).
/// - `root`: dónde vive `.chrono/` y desde dónde se carga la config de
///   clasificación (para git, el mismo toplevel; para una fuente de
///   fichero, su directorio contenedor, o cwd si no tiene).
fn resolve_paths(source: &dyn Source, start: &Path) -> Result<(PathBuf, PathBuf)> {
    if source.kind() == "git" {
        let root = repo_root(start)?;
        Ok((root.clone(), root))
    } else {
        let abs = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
        let root = match abs.parent().filter(|p| !p.as_os_str().is_empty()) {
            Some(p) => p.to_path_buf(),
            None => std::env::current_dir()?,
        };
        Ok((abs, root))
    }
}

/// Elige la fuente para `start` en `registry` y resuelve sus rutas (ver
/// [`resolve_paths`]). Único punto usado por `init` y `sync` para saber
/// "qué fuente es esto y dónde vive su índice".
fn resolve_source<'a>(registry: &'a Registry, start: &Path) -> Result<(&'a dyn Source, PathBuf, PathBuf)> {
    let src = registry.pick(start).ok_or_else(|| unrecognized_source_err(start))?;
    let (ingest_path, root) = resolve_paths(src, start)?;
    Ok((src, ingest_path, root))
}

/// `chrono init [repo]`: índice completo desde cero en `<root>/.chrono/index.db`.
pub fn init(start: &Path) -> Result<()> {
    let registry = build_registry();
    let (src, ingest_path, root) = resolve_source(&registry, start)?;

    let dir = root.join(".chrono");
    std::fs::create_dir_all(&dir)?;
    // Todo .chrono/ se ignora en git salvo la config (esa sí conviene versionarla).
    let _ = std::fs::write(dir.join(".gitignore"), "*\n!config.json\n");
    let db = dir.join("index.db");

    let mut store = open_or_recreate(&db)?;
    store.reset()?;
    let cfg = chrono_classify_rules::load(&root);
    let (n, _wm) =
        ingest_source(src, &ingest_path, None, &mut store, &root).map_err(|e| e.into_boxed())?;
    finalize(&root, &store, src, &cfg.bug_labels)?;
    let unit = if src.kind() == "git" { ("commits", "commits") } else { ("events", "eventos") };
    eprintln!("{}", t(
        format!("chrono: index ready at .chrono/index.db ({n} {}).", unit.0),
        format!("chrono: índice listo en .chrono/index.db ({n} {}).", unit.1),
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
    let registry = build_registry();

    let start: PathBuf = match repo_arg {
        Some(p) => p.to_path_buf(),
        None => {
            let stored = store
                .meta("repo_path")?
                .filter(|s| !s.is_empty())
                .or(store.meta("source_path")?.filter(|s| !s.is_empty()));
            match stored {
                Some(s) => PathBuf::from(s),
                None => {
                    return Err(t(
                        "don't know which repo to sync (no argument and no repo_path in the index)",
                        "no sé qué repo sincronizar (ni argumento ni repo_path en el índice)",
                    )
                    .into())
                }
            }
        }
    };

    let (src, ingest_path, root) = resolve_source(&registry, &start)?;
    let wm = store.meta("last_watermark")?.map(|v| Watermark { kind: src.kind().to_string(), value: v });

    let cfg = chrono_classify_rules::load(&root);

    // Atajo de git: si el HEAD no cambió desde el último watermark, ni
    // siquiera se abre el cursor (evita spawnear `git log` para nada).
    if src.kind() == "git" {
        let head = chrono_source_git::head_sha(&ingest_path)?;
        if let Some(ref w) = wm {
            if w.value == format!("sha:{head}") {
                eprintln!("{}", t("sync: 0 new commits", "sync: 0 commits nuevos"));
                return Ok(());
            }
        }
    }

    let n = match ingest_source(src, &ingest_path, wm.clone(), &mut store, &root) {
        Ok((n, _wm)) => n,
        Err(IngestError::Diverged) => {
            eprintln!("{}", t(
                "chrono: divergence detected (rebase/force-push) -> full reindex",
                "chrono: divergencia detectada (rebase/force-push) -> reindex completo",
            ));
            store.reset()?;
            ingest_source(src, &ingest_path, None, &mut store, &root)
                .map(|(n, _wm)| n)
                .map_err(|e| e.into_boxed())?
        }
        Err(e) => return Err(e.into_boxed()),
    };
    finalize(&root, &store, src, &cfg.bug_labels)?;
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

/// Ingesta genérica: drena el `Cursor` de CUALQUIER `Source` al `Writer` en
/// una sola transacción, clasificando cada evento (Level-0 reglas) al vuelo.
/// Vale para git y para cualquier otro adaptador futuro (jsonl, csv…).
///
/// `repo_for_config` es de dónde se carga `.chrono/config.json` (reglas de
/// clasificación): para git es el propio repo; para una fuente de fichero,
/// la raíz del índice (ver [`resolve_paths`]).
///
/// Devuelve `(nº de eventos ingeridos, watermark final)`.
fn ingest_source(
    source: &dyn Source,
    path: &Path,
    watermark: Option<Watermark>,
    store: &mut Store,
    repo_for_config: &Path,
) -> std::result::Result<(usize, Watermark), IngestError> {
    let cfg = chrono_classify_rules::load(repo_for_config);
    let classifier = RulesClassifier::new(cfg);

    let source_cfg = SourceConfig::default();
    let mut cur: Box<dyn Cursor> = match source.open(path, watermark, &source_cfg) {
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
    eprintln!("{}", t("  building search index…", "  construyendo índice de búsqueda…"));
    store.rebuild_fts()?;
    Ok((count, wm))
}

/// Enriquecimiento tras la ingesta: exclusiones de ruido (genérico), y —solo
/// para git— borrados/tamaños de HEAD, tags→markers y forge (PRs/issues de
/// GitHub vía `gh`, si está disponible). Cierra con `meta` genérico y
/// compactado.
fn finalize(root: &Path, store: &Store, source: &dyn Source, bug_labels: &[String]) -> Result<()> {
    if source.kind() == "git" {
        finalize_git_sizes_and_deletions(root, store)?;
        finalize_git_tags(root, store)?;
    }

    // Exclusiones de ruido (globs por defecto del Go; sin config en R1).
    // Genérico: aplica a cualquier `entities.key`, tenga o no sentido de
    // fichero para la fuente en cuestión (si no hay coincidencias, no hace nada).
    {
        let conn = store.conn();
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

    if source.kind() == "git" {
        finalize_git_forge(root, store, bug_labels)?;
    }

    write_meta(store, source, root)?;
    store.optimize()?;
    Ok(())
}

/// SOLO-git: borrados (toda entidad `file` que no esté en HEAD) y tamaños
/// (top `SIZE_CAP` más cambiados que siguen en HEAD, con caché por OID).
fn finalize_git_sizes_and_deletions(root: &Path, store: &Store) -> Result<()> {
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
    Ok(())
}

/// SOLO-git: tags de git → `markers` (para `phases`).
fn finalize_git_tags(root: &Path, store: &Store) -> Result<()> {
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
    Ok(())
}

/// SOLO-git: forge (PRs/issues de GitHub vía `gh`, si está disponible).
/// Nunca falla el índice: si `gh` no está, no hay remoto o no está
/// autenticado, se avisa con precisión y se sigue.
fn finalize_git_forge(root: &Path, store: &Store, bug_labels: &[String]) -> Result<()> {
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
    Ok(())
}

/// `meta` genérico: `schema_version`, `repo_path` (git) o `source_path`
/// (cualquier otra fuente), `source_id` y `source_kind`. `last_watermark` y
/// el `manifest()` de la fuente ya los escribe [`ingest_source`].
fn write_meta(store: &Store, source: &dyn Source, root: &Path) -> Result<()> {
    let abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let kind = source.kind();
    store.set_meta("schema_version", &chrono_store::SCHEMA_VERSION.to_string())?;
    if kind == "git" {
        store.set_meta("repo_path", &abs.to_string_lossy())?;
    } else {
        store.set_meta("source_path", &abs.to_string_lossy())?;
    }
    store.set_meta("source_id", &format!("{kind}:{}", abs.to_string_lossy()))?;
    store.set_meta("source_kind", kind)?;
    Ok(())
}
