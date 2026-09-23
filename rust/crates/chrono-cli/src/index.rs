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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono_classify_rules::RulesClassifier;
use chrono_core::{Classifier, CoreError, Cursor, Registry, Source, SourceConfig, Watermark};
use chrono_source_git::{blob_lines, ls_tree, GitSource};
use chrono_store::Store;
use chrono_tracker_github as tracker;

use crate::glob;
use crate::i18n::t;
use crate::timeutil::{epoch_to_iso8601, now_epoch};

/// `source_id` canónico de una fuente: `<kind>:<ruta absoluta canónica>`. Es el
/// mismo valor que los adaptadores ponen en `events.source_id` (git usa la raíz
/// del repo; las fuentes de fichero, el fichero), así que sirve de clave estable
/// en la tabla `sources` y para `correlate`/consultas por fuente.
fn canonical_source_id(kind: &str, ingest_path: &Path) -> String {
    let abs = std::fs::canonicalize(ingest_path).unwrap_or_else(|_| ingest_path.to_path_buf());
    format!("{kind}:{}", abs.to_string_lossy())
}

/// Marca de tiempo ISO-8601 UTC del momento actual (para `sources.last_sync_at`).
fn now_iso() -> String {
    epoch_to_iso8601(now_epoch())
}

/// Nº máximo de ficheros a los que se les calcula el tamaño (los más cambiados).
const SIZE_CAP: usize = 4000;
/// Granularidad de los rollups Drain-light (1 min). `timeline`/`patterns` a
/// escala se apoyan en estos buckets. Ver `docs/DESIGN-GENERAL-CORE.md §6`.
const ROLLUP_BUCKET_SECS: i64 = 60;
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
        format!("I don't recognize this source at {}; expected a git repo, a .jsonl/.csv file, or a text log (syslog/nginx).", path.display()),
        format!("no reconozco esta fuente en {}; se esperaba un repo git, un fichero .jsonl/.csv o un log de texto (syslog/nginx).", path.display()),
    )
    .into()
}

/// Registro de fuentes disponibles para `init`/`sync`. `pick` elige la de
/// mayor confianza vía `Source::detect`.
fn build_registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(GitSource::new()));
    r.register(Box::new(chrono_source_jsonl::JsonlSource::new()));
    r.register(Box::new(chrono_source_csv::CsvSource::new()));
    r.register(Box::new(chrono_source_textlog::TextlogSource::new()));
    r.register(Box::new(chrono_source_changelog::ChangelogSource::new()));
    // R3: registrar aquí más adaptadores (journald…).
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
/// Con `symbols=true` (`--symbols`, solo git), además de la ingesta de commits
/// extrae los símbolos (funciones) tocados por cada hunk (S1).
pub fn init(start: &Path, symbols: bool) -> Result<()> {
    let registry = build_registry();
    let (src, ingest_path, root) = resolve_source(&registry, start)?;

    let dir = root.join(".chrono");
    std::fs::create_dir_all(&dir)?;
    // Todo .chrono/ se ignora en git salvo la config (esa sí conviene versionarla).
    let _ = std::fs::write(dir.join(".gitignore"), "*\n!config.json\n");
    let db = dir.join("index.db");

    let mut store = open_or_recreate(&db)?;
    store.reset()?;
    let (n, wm, manifest) =
        ingest_source(src, &ingest_path, None, &mut store, &root).map_err(|e| e.into_boxed())?;

    // Registra la fuente en la tabla `sources` (primera fuente del índice).
    let source_id = canonical_source_id(src.kind(), &ingest_path);
    store.upsert_source(
        &source_id,
        src.kind(),
        &ingest_path.to_string_lossy(),
        &wm.value,
        &manifest,
        &now_iso(),
    )?;
    // El manifiesto también va a `meta` (compat con el envelope de las consultas,
    // que lee git_version/first_parent/…); `last_watermark` como respaldo legado.
    for (k, v) in &manifest {
        store.set_meta(k, v)?;
    }
    store.set_meta("last_watermark", &wm.value)?;

    eprintln!("{}", t("  building search index…", "  construyendo índice de búsqueda…"));
    store.rebuild_fts()?;
    finalize(&root, &store, &git_path_if_git(src.kind(), &ingest_path))?;
    write_meta(&store, src, &root)?;

    if symbols && src.kind() == "git" {
        ingest_symbols(&ingest_path, &store, None)?;
    }

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

/// Raíz del índice a partir de la ruta del `.db` (`<root>/.chrono/index.db` →
/// `<root>`). Ahí vive `.chrono/config.json`, compartido por TODAS las fuentes.
fn index_root_of(db: &Path) -> PathBuf {
    db.parent()
        .and_then(|chrono_dir| chrono_dir.parent())
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `chrono add <path>`: añade una fuente MÁS al índice existente (git + logs +
/// deploys en un mismo `.chrono/`, correlacionables por tiempo). No borra lo ya
/// ingerido. Si la fuente ya estaba, la re-ingiere (borra sus eventos primero),
/// así `add` es idempotente.
pub fn add(db: &Path, start: &Path, symbols: bool) -> Result<()> {
    let mut store = Store::open(db)?;
    let index_root = index_root_of(db);
    let registry = build_registry();
    let (src, ingest_path, _root) = resolve_source(&registry, start)?;
    let source_id = canonical_source_id(src.kind(), &ingest_path);

    let already = store.list_sources()?.iter().any(|s| s.id == source_id);
    if already {
        eprintln!("{}", t(
            format!("chrono: source {source_id} already indexed — re-ingesting"),
            format!("chrono: la fuente {source_id} ya estaba — se re-ingiere"),
        ));
        store.delete_source_events(&source_id)?;
    }

    let (n, wm, manifest) = ingest_source(src, &ingest_path, None, &mut store, &index_root)
        .map_err(|e| e.into_boxed())?;
    store.upsert_source(
        &source_id,
        src.kind(),
        &ingest_path.to_string_lossy(),
        &wm.value,
        &manifest,
        &now_iso(),
    )?;

    eprintln!("{}", t("  building search index…", "  construyendo índice de búsqueda…"));
    store.rebuild_fts()?;
    finalize(&index_root, &store, &git_path_if_git(src.kind(), &ingest_path))?;

    if symbols && src.kind() == "git" {
        ingest_symbols(&ingest_path, &store, None)?;
    }

    let total = store.list_sources()?.len();
    eprintln!("{}", t(
        format!("chrono: added {} source ({n} events). Index now has {total} sources.", src.kind()),
        format!("chrono: añadida fuente {} ({n} eventos). El índice tiene ya {total} fuentes.", src.kind()),
    ));
    Ok(())
}

/// `chrono sync [path]`: sincroniza TODAS las fuentes registradas (o solo la de
/// `path`, si se pasa), ingiriendo el delta desde el watermark de cada una. Si
/// una fuente diverge (rebase/force-push, log rotado), reindexa SOLO esa fuente.
pub fn sync(db: &Path, source_arg: Option<&Path>) -> Result<()> {
    let mut store = Store::open(db)?;
    let index_root = index_root_of(db);
    let registry = build_registry();

    let mut sources = store.list_sources()?;
    // Compat: índice anterior a multi-fuente (sin filas en `sources`). Se
    // reconstruye una fuente desde `meta` (repo_path/source_path + kind) para
    // que el primer `sync` tras actualizar se auto-cure.
    if sources.is_empty() {
        if let Some((id, kind, path, wm)) = legacy_source_from_meta(&store)? {
            store.upsert_source(&id, &kind, &path, &wm, &BTreeMap::new(), &now_iso())?;
            sources = store.list_sources()?;
        } else {
            return Err(t(
                "no sources in the index — run 'chrono init' first",
                "no hay fuentes en el índice — ejecuta 'chrono init' primero",
            )
            .into());
        }
    }

    // Filtro opcional: si se pasa un path, solo se sincroniza esa fuente.
    let filter_id: Option<String> = match source_arg {
        Some(p) => match registry.pick(p) {
            Some(src) => Some(canonical_source_id(src.kind(), p)),
            None => Some(canonical_source_id("", p)), // no reconocida: no casará ninguna → aviso abajo.
        },
        None => None,
    };

    let symbols_enabled = store.meta("symbols")?.as_deref() == Some("1");
    let now = now_iso();
    let mut total = 0usize;
    let mut synced_any = false;
    let mut matched_any = false;
    // Solo las fuentes git que REALMENTE reingirieron necesitan refrescar
    // tamaños/tags/forge; si git no cambió (atajo de HEAD), no se re-tira del
    // forge de GitHub aunque otra fuente de log sí traiga eventos nuevos.
    let mut changed_git_paths: Vec<String> = Vec::new();
    for s in &sources {
        if let Some(ref want) = filter_id {
            if &s.id != want {
                continue;
            }
        }
        matched_any = true;
        let Some(adapter) = registry.by_kind(&s.kind) else {
            eprintln!("{}", t(
                format!("  warning: no adapter for source kind '{}' ({}) — skipping", s.kind, s.id),
                format!("  aviso: no hay adaptador para la fuente '{}' ({}) — se omite", s.kind, s.id),
            ));
            continue;
        };
        let path = PathBuf::from(&s.path);

        // Atajo de git: si el HEAD no cambió, no se abre el cursor.
        if s.kind == "git" {
            if let Ok(head) = chrono_source_git::head_sha(&path) {
                if s.watermark == format!("sha:{head}") {
                    continue;
                }
            }
        }

        let wm = if s.watermark.is_empty() {
            None
        } else {
            Some(Watermark { kind: s.kind.clone(), value: s.watermark.clone() })
        };

        let (n, new_wm) = match ingest_source(adapter, &path, wm, &mut store, &index_root) {
            Ok((n, new_wm, _manifest)) => (n, new_wm),
            Err(IngestError::Diverged) => {
                eprintln!("{}", t(
                    format!("  divergence in {} — reindexing that source", s.id),
                    format!("  divergencia en {} — se reindexa esa fuente", s.id),
                ));
                store.delete_source_events(&s.id)?;
                ingest_source(adapter, &path, None, &mut store, &index_root)
                    .map(|(n, new_wm, _)| (n, new_wm))
                    .map_err(|e| e.into_boxed())?
            }
            Err(e) => return Err(e.into_boxed()),
        };
        store.set_source_watermark(&s.id, &new_wm.value, &now)?;
        if s.kind == "git" {
            changed_git_paths.push(s.path.clone());
            // Recomputar símbolos del delta (old HEAD..HEAD) si el índice los usa.
            if symbols_enabled {
                let since_sha = s.watermark.strip_prefix("sha:").filter(|s| !s.is_empty());
                ingest_symbols(&path, &store, since_sha)?;
            }
        }
        total += n;
        synced_any = true;
    }

    if filter_id.is_some() && !matched_any {
        eprintln!("{}", t(
            "sync: no matching source in the index",
            "sync: ninguna fuente del índice coincide",
        ));
        return Ok(());
    }

    // Nada nuevo en ninguna fuente: no se reconstruye FTS ni se compacta.
    if !synced_any {
        eprintln!("{}", t("sync: 0 new events", "sync: 0 eventos nuevos"));
        return Ok(());
    }

    eprintln!("{}", t("  building search index…", "  construyendo índice de búsqueda…"));
    store.rebuild_fts()?;
    finalize(&index_root, &store, &changed_git_paths)?;
    eprintln!("{}", t(format!("sync: {total} new events"), format!("sync: {total} eventos nuevos")));
    Ok(())
}

/// Reconstruye una fila de fuente desde `meta` para índices anteriores a la
/// tabla `sources`. Devuelve `(source_id, kind, path, watermark)` o `None`.
fn legacy_source_from_meta(store: &Store) -> Result<Option<(String, String, String, String)>> {
    let kind = store.meta("source_kind")?.filter(|s| !s.is_empty());
    let path = store
        .meta("repo_path")?
        .filter(|s| !s.is_empty())
        .or(store.meta("source_path")?.filter(|s| !s.is_empty()));
    match (kind, path) {
        (Some(kind), Some(path)) => {
            let id = format!("{kind}:{path}");
            let wm = store.meta("last_watermark")?.unwrap_or_default();
            Ok(Some((id, kind, path, wm)))
        }
        _ => Ok(None),
    }
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
) -> std::result::Result<(usize, Watermark, BTreeMap<String, String>), IngestError> {
    let cfg = chrono_classify_rules::load(repo_for_config);
    // Opciones del adaptador desde .chrono/config.json (columnas CSV, preset/year
    // de textlog…), resueltas por ruta/nombre de la fuente. Se toman antes de
    // mover `cfg` al clasificador.
    let source_cfg = SourceConfig { options: cfg.source_options_for(path) };
    let classifier = RulesClassifier::new(cfg);

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
                let _ = write!(stderr, "\r  {count}…");
                let _ = stderr.flush();
            }
        }
        if count >= PROGRESS_EVERY {
            let _ = writeln!(stderr, "\r  {count}");
        }
        w.commit()?;
    }

    let wm = cur.watermark();
    let manifest = cur.manifest();
    Ok((count, wm, manifest))
}

/// Enriquecimiento tras la ingesta (multi-fuente): exclusiones de ruido
/// (genérico), y —para cada fuente git registrada— borrados/tamaños de HEAD,
/// tags→markers y forge (PRs/issues de GitHub vía `gh`, si está disponible).
/// Cierra compactando. `index_root` es la raíz del índice (de ahí se lee la
/// config de clasificación, compartida por todas las fuentes).
///
/// Nota: con MÁS DE UNA fuente git en el mismo índice, el cálculo de borrados
/// (`deleted`) del último repo pisaría al del anterior (las entidades `file`
/// son globales, no por fuente). El caso soportado es 1 git + N fuentes de log
/// (cuyas entidades no son `type='file'`, así que no se ven afectadas).
fn finalize(index_root: &Path, store: &Store, git_paths: &[String]) -> Result<()> {
    for path in git_paths {
        finalize_git_sizes_and_deletions(Path::new(path), store)?;
        finalize_git_tags(Path::new(path), store)?;
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

    if !git_paths.is_empty() {
        let cfg = chrono_classify_rules::load(index_root);
        for path in git_paths {
            finalize_git_forge(Path::new(path), store, &cfg.bug_labels)?;
        }
    }

    // Rollups Drain-light (plantillas + conteos por bucket) para `patterns`.
    // Reconstrucción completa: barata en índices normales, O(eventos) a escala.
    eprintln!("{}", t("  clustering log templates…", "  agrupando plantillas…"));
    store.build_rollups(ROLLUP_BUCKET_SECS)?;
    store.set_meta("rollup_bucket_secs", &ROLLUP_BUCKET_SECS.to_string())?;

    store.optimize()?;
    Ok(())
}

/// S1 · Extrae los símbolos (funciones) tocados por cada hunk y los guarda como
/// `Touch{type:"symbol"}` (clave `file#func`), keyeados por el `sha` de commits
/// ya ingeridos. `since_sha=None` recomputa TODO (borra los símbolos previos);
/// `Some(sha)` añade solo el rango `sha..HEAD` (sync incremental). NO toca el
/// cursor de ingesta de git — la paridad de commits queda intacta.
fn ingest_symbols(repo: &Path, store: &Store, since_sha: Option<&str>) -> Result<()> {
    eprintln!("{}", t("  extracting symbols…", "  extrayendo símbolos…"));
    if since_sha.is_none() {
        store.delete_entities_of_type("symbol")?;
    }
    let mut on_commit = |sha: &str, syms: Vec<crate::symbols::SymbolTouch>| -> crate::symbols::Result<()> {
        if syms.is_empty() {
            return Ok(());
        }
        let touches: Vec<(String, String, i64, String)> = syms
            .iter()
            .map(|s| (s.entity_key(), "symbol".to_string(), s.added + s.deleted, symbol_attrs_json(s)))
            .collect();
        store.add_touches(sha, &touches).map_err(|e| e.to_string().into())
    };
    crate::symbols::stream_symbols(repo, since_sha, &mut on_commit).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })?;
    store.set_meta("symbols", "1")?;
    store.set_meta("symbols_rules_hash", &crate::symbols::rules_hash())?;
    Ok(())
}

/// JSON compacto de attrs de un touch de símbolo: `added`/`deleted` (para
/// `churn --by symbol`) + `file`/`sym_kind`. Escapa comillas y backslash.
fn symbol_attrs_json(s: &crate::symbols::SymbolTouch) -> String {
    fn esc(v: &str) -> String {
        v.replace('\\', "\\\\").replace('"', "\\\"")
    }
    format!(
        "{{\"added\":{},\"deleted\":{},\"file\":\"{}\",\"sym_kind\":\"{}\"}}",
        s.added,
        s.deleted,
        esc(&s.file),
        esc(&s.sym_kind),
    )
}

/// Helper: `vec![ruta]` si la fuente es git, `vec![]` si no. Para pasar a
/// [`finalize`] las rutas git que hay que refrescar (tamaños/tags/forge).
fn git_path_if_git(kind: &str, ingest_path: &Path) -> Vec<String> {
    if kind == "git" {
        vec![ingest_path.to_string_lossy().into_owned()]
    } else {
        Vec::new()
    }
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
    // Ordenados: el orden de inserción en `blob_lines` es entonces
    // determinista (un `HashSet` no lo garantiza), así el `.db` es
    // byte-idéntico entre dos ingestas iguales, no solo en contenido.
    let mut oids: Vec<String> = oid_set.into_iter().collect();
    oids.sort_unstable();
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
