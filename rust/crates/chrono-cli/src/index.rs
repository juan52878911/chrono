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
use std::time::Duration;

use chrono_classify_rules::RulesClassifier;
use chrono_core::{Classifier, CoreError, Cursor, Registry, Source, SourceConfig, Watermark};
use chrono_source_git::{blob_lines, ls_tree, GitSource};
use chrono_store::Store;
use chrono_tracker_github as tracker;

use crate::glob;
use crate::i18n::t;

mod glob_source;
mod relations;
use glob_source::GlobSource;
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
/// Granularidad de los rollups Drain-light (1 min, en ms — `at_epoch` es ms).
/// `timeline`/`patterns` a escala se apoyan en estos buckets. Ver §6.
const ROLLUP_BUCKET_MS: i64 = 60_000;
/// Cada cuántos eventos se refresca el indicador de progreso (como el Go).
const PROGRESS_EVERY: usize = 2000;
/// Eventos por lote de ingesta: se commitea cada tantos para acotar el WAL y
/// mantener el caudal plano a escala (ver `ingest_source`).
const INGEST_BATCH: usize = 100_000;

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
        format!("I don't recognize this source at {}; expected a git repo, a .jsonl/.csv file, a Nod event file, or a text log (syslog/nginx). Use --as <kind> to force one.", path.display()),
        format!("no reconozco esta fuente en {}; se esperaba un repo git, un fichero .jsonl/.csv, un fichero de eventos de Nod o un log de texto (syslog/nginx). Usa --as <tipo> para forzarlo.", path.display()),
    )
    .into()
}

/// Registro de fuentes disponibles para `init`/`sync`. `pick` elige la de
/// mayor confianza vía `Source::detect`.
pub(crate) fn build_registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(GitSource::new()));
    r.register(Box::new(chrono_source_jsonl::JsonlSource::new()));
    r.register(Box::new(chrono_source_csv::CsvSource::new()));
    r.register(Box::new(chrono_source_textlog::TextlogSource::new()));
    r.register(Box::new(chrono_source_changelog::ChangelogSource::new()));
    r.register(Box::new(chrono_source_oslog::OsLogSource::new()));
    r.register(Box::new(chrono_source_template::TemplateSource::new()));
    r.register(Box::new(chrono_source_nod::NodSource::new()));
    r.register(Box::new(chrono_source_dir::DirSource::new()));
    r.register(Box::new(chrono_source_delega::DelegaSource::new()));
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
/// [`resolve_paths`]). Si `as_kind` es `Some`, se FUERZA ese adaptador por su
/// `kind` (p.ej. `--as template`, o `"adapter"` del config) en vez de
/// autodetectar; error accionable si ese kind no existe.
fn resolve_source<'a>(
    registry: &'a Registry,
    start: &Path,
    as_kind: Option<&str>,
) -> Result<(&'a dyn Source, PathBuf, PathBuf)> {
    let src = match as_kind.filter(|s| !s.is_empty()) {
        Some(kind) => registry.by_kind(kind).ok_or_else(|| {
            t(
                format!("unknown source kind {kind:?} for --as (try: git, jsonl, csv, textlog, changelog, oslog, template, nod, dir)"),
                format!("kind de fuente desconocido {kind:?} para --as (prueba: git, jsonl, csv, textlog, changelog, oslog, template, nod, dir)"),
            )
        })?,
        None => registry.pick(start).ok_or_else(|| unrecognized_source_err(start))?,
    };
    let (ingest_path, root) = resolve_paths(src, start)?;
    Ok((src, ingest_path, root))
}

/// `chrono init [repo]`: índice completo desde cero en `<root>/.chrono/index.db`.
/// Con `symbols=true` (`--symbols`, solo git), además de la ingesta de commits
/// extrae los símbolos (funciones) tocados por cada hunk (S1).
pub fn init(start: &Path, symbols: bool, as_kind: Option<&str>, db_override: Option<&Path>) -> Result<()> {
    let registry = build_registry();
    let (src, ingest_path, root) = resolve_source(&registry, start, as_kind)?;

    // `--db` manda: el índice va ahí y el repo no se toca (antes se ignoraba y
    // `init` escribía `.chrono/` dentro del repo aunque se pidiera otra ruta).
    // La config se sigue leyendo de `<root>/.chrono/config.json` si existe.
    let db = match db_override {
        Some(p) => {
            if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            p.to_path_buf()
        }
        None => {
            let dir = root.join(".chrono");
            std::fs::create_dir_all(&dir)?;
            // Todo .chrono/ se ignora en git salvo la config (esa sí conviene versionarla).
            let _ = std::fs::write(dir.join(".gitignore"), "*\n!config.json\n");
            dir.join("index.db")
        }
    };

    let _lock = lock_ingest(&db)?;
    let source_id = canonical_source_id(src.kind(), &ingest_path);
    let mut store = open_or_recreate(&db, &source_id, db_override.is_some())?;
    store.reset()?;
    let (n, wm, manifest) =
        ingest_source(src, &ingest_path, None, &mut store, &root, &IngestCtx::default()).map_err(|e| e.into_boxed())?;

    // Registra la fuente en la tabla `sources` (primera fuente del índice).
    store.upsert_source(
        &source_id,
        src.kind(),
        &ingest_path.to_string_lossy(),
        &wm.value,
        &manifest,
        &now_iso(),
    )?;
    mark_ids(&store, src, &source_id)?;
    // El manifiesto también va a `meta` (compat con el envelope de las consultas,
    // que lee git_version/first_parent/…); `last_watermark` como respaldo legado.
    for (k, v) in &manifest {
        store.set_meta(k, v)?;
    }
    store.set_meta("last_watermark", &wm.value)?;

    eprintln!("{}", t("  building search index…", "  construyendo índice de búsqueda…"));
    store.rebuild_fts()?;
    finalize(&root, &store, &git_path_if_git(src.kind(), &ingest_path), false, false)?;
    write_meta(&store, src, &root)?;

    if symbols && src.kind() == "git" {
        ingest_symbols(&ingest_path, &store, None)?;
    }

    match chrono_source_git::unborn_head(&ingest_path).filter(|_| src.kind() == "git") {
        Some((_, 0)) => eprintln!("{}", t(
            "chrono: the repo has no commits yet; `chrono sync` will read them once there are",
            "chrono: el repo aún no tiene commits; `chrono sync` los leerá cuando los haya",
        )),
        // Rama huérfana en un repo con historia: no es que el repo esté vacío.
        Some((branch, n)) => eprintln!("{}", t(
            format!(
                "chrono: HEAD points to no commit (branch {branch} has no commits yet), but {n} branch(es) do; \
                 init reads HEAD's history: switch to one of them and run init again, or `chrono sync` once {branch} has commits"
            ),
            format!(
                "chrono: HEAD no apunta a ningún commit (la rama {branch} aún no tiene commits), pero hay {n} rama(s) con commits; \
                 init lee la historia de HEAD: cambia a una de ellas y repite init, o `chrono sync` cuando {branch} tenga commits"
            ),
        )),
        None => {}
    }
    let unit = if src.kind() == "git" { ("commits", "commits") } else { ("events", "eventos") };
    eprintln!("{}", t(
        format!("chrono: index ready at {} ({n} {}).", db.display(), unit.0),
        format!("chrono: índice listo en {} ({n} {}).", db.display(), unit.1),
    ));
    eprintln!("{}", t(
        "Now, inside the repo: chrono hotspots · chrono bugs · chrono sync",
        "Ahora, dentro del repo: chrono hotspots · chrono bugs · chrono sync",
    ));
    Ok(())
}

/// Raíz del índice a partir de la ruta del `.db` (`<root>/.chrono/index.db` →
/// `<root>`). Ahí vive `.chrono/config.json`, compartido por TODAS las fuentes.
pub(crate) fn index_root_of(db: &Path) -> PathBuf {
    db.parent()
        .and_then(|chrono_dir| chrono_dir.parent())
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `chrono add <path>`: añade una fuente MÁS al índice existente (git + logs +
/// deploys en un mismo `.chrono/`, correlacionables por tiempo). No borra lo ya
/// ingerido. Si la fuente ya estaba, la re-ingiere (borra sus eventos primero),
/// así `add` es idempotente.
pub fn add(db: &Path, start: &Path, symbols: bool, as_kind: Option<&str>, preset: Option<&str>) -> Result<()> {
    let _lock = lock_ingest(db)?;
    let mut store = Store::open(db)?;
    let index_root = index_root_of(db);
    let registry = build_registry();
    // Override del adaptador: `--as` gana; si no, la clave `"adapter"` de las
    // `source_options` de esta ruta en `.chrono/config.json` (así
    // `chrono add miapp.log` usa el adaptador que diga el config, sin --as).
    let cfg = chrono_classify_rules::load(&index_root);
    let config_adapter = cfg.source_options_for(start).get("adapter").cloned();
    let effective_as = as_kind.map(str::to_string).or(config_adapter);
    let (src, ingest_path, _root) = resolve_source(&registry, start, effective_as.as_deref())?;
    let source_id = canonical_source_id(src.kind(), &ingest_path);

    let already = store.list_sources()?.iter().any(|s| s.id == source_id);
    if already {
        eprintln!("{}", t(
            format!("chrono: source {source_id} already indexed — re-ingesting"),
            format!("chrono: la fuente {source_id} ya estaba — se re-ingiere"),
        ));
        store.delete_source_events(&source_id)?;
    }

    let ctx = IngestCtx { preset, ..Default::default() };
    let (n, wm, mut manifest) = ingest_source(src, &ingest_path, None, &mut store, &index_root, &ctx)
        .map_err(|e| e.into_boxed())?;
    if let Some(p) = preset {
        manifest.insert("preset".to_string(), p.to_string());
    }
    store.upsert_source(
        &source_id,
        src.kind(),
        &ingest_path.to_string_lossy(),
        &wm.value,
        &manifest,
        &now_iso(),
    )?;
    mark_ids(&store, src, &source_id)?;

    eprintln!("{}", t("  building search index…", "  construyendo índice de búsqueda…"));
    // Re-ingesta (already): se borraron filas con rowids arbitrarios, así que el
    // watermark incremental ya no vale → reconstrucción completa. Fuente nueva:
    // solo se indexan sus filas (O(delta)), sin re-tocar el resto del índice.
    if already {
        store.rebuild_fts()?;
    } else {
        store.fts_sync()?;
    }
    // Misma lógica para rollups: re-ingesta → reconstrucción completa (los
    // conteos de la fuente borrada ya no valen); fuente nueva → solo el delta.
    finalize(&index_root, &store, &git_path_if_git(src.kind(), &ingest_path), !already, false)?;

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

/// `chrono add '<dir>/**/*.jsonl'`: añade una fuente glob, UNA fila en
/// `sources` para todos los ficheros que casan (ver `glob_source`). `alias`
/// (`--name`) sirve para `--source <alias>` y como clave en `source_options`.
pub fn add_glob(db: &Path, pattern: &str, as_kind: Option<&str>, alias: Option<&str>, preset: Option<&str>) -> Result<()> {
    let _lock = lock_ingest(db)?;
    let mut store = Store::open(db)?;
    let index_root = index_root_of(db);
    let registry = build_registry();
    let pattern = glob_source::absolutize(pattern);
    let files = glob::expand(&pattern);
    let Some(first) = files.first() else {
        return Err(t(format!("no file matches {pattern}"), format!("ningún fichero casa con {pattern}")).into());
    };
    let cfg = chrono_classify_rules::load(&index_root);
    let config_adapter = alias
        .into_iter()
        .chain([pattern.as_str()])
        .find_map(|k| cfg.source_options.get(k).and_then(|o| o.get("adapter")).cloned());
    let effective_as = as_kind.map(str::to_string).or(config_adapter);
    // El tipo sale del primer fichero que se reconozca: uno vacío (un log recién
    // creado) no dice nada y no debe tumbar el glob entero.
    let probe = files.iter().find(|f| registry.pick(f).is_some()).unwrap_or(first);
    let (src, _, _) = resolve_source(&registry, probe, effective_as.as_deref())?;
    let gs = GlobSource {
        id: format!("{}:{pattern}", src.kind()),
        pattern: pattern.clone(),
        alias: alias.map(str::to_string),
        preset: preset.map(str::to_string),
    };
    if let Some(a) = alias {
        let taken = store.list_sources()?.into_iter().any(|s| {
            s.id != gs.id && GlobSource::from_row(&s.id, &s.manifest_json).and_then(|g| g.alias).as_deref() == Some(a)
        });
        if taken {
            return Err(t(format!("alias {a:?} is already in use"), format!("el alias {a:?} ya está en uso")).into());
        }
    }
    let already = store.list_sources()?.iter().any(|s| s.id == gs.id);
    if already {
        eprintln!("{}", t(
            format!("chrono: source {} already indexed — re-ingesting", gs.id),
            format!("chrono: la fuente {} ya estaba — se re-ingiere", gs.id),
        ));
        store.delete_source_events(&gs.id)?;
        gs.clear_state(&store)?;
    }
    // La fila va antes que los eventos: el estado por fichero cuelga de ella.
    store.upsert_source(&gs.id, src.kind(), &pattern, "", &gs.manifest(files.len()), &now_iso())?;
    let r = gs.sync(src, &mut store, &index_root)?;
    store.upsert_source(&gs.id, src.kind(), &pattern, &r.watermark, &gs.manifest(r.files_total), &now_iso())?;

    eprintln!("{}", t("  building search index…", "  construyendo índice de búsqueda…"));
    if already || r.diverged {
        store.rebuild_fts()?;
    } else {
        store.fts_sync()?;
    }
    finalize(&index_root, &store, &[], !(already || r.diverged), false)?;
    let total = store.list_sources()?.len();
    eprintln!("{}", t(
        format!("chrono: added {} source of {} files ({} events). Index now has {total} sources.", src.kind(), r.files_total, r.events),
        format!("chrono: añadida fuente {} de {} ficheros ({} eventos). El índice tiene ya {total} fuentes.", src.kind(), r.files_total, r.events),
    ));
    Ok(())
}

/// `chrono remove <fuente>`: quita una fuente y sus eventos del índice. La
/// fuente se nombra por su id (`jsonl:/ruta`), su alias (`--name`), su ruta o
/// patrón tal como se añadió, o `cmd:<nombre>`. Es la salida para una fuente
/// que ya no existe y no se quiere seguir viendo en `sources` ni en `sync`.
pub fn remove(db: &Path, arg: &str) -> Result<()> {
    // Sin índice, el mismo error que las consultas (y no se crea uno vacío).
    if !db.is_file() {
        return Err(chrono_store::NoIndex { path: db.to_path_buf() }.into());
    }
    let _lock = lock_ingest(db)?;
    let store = Store::open(db)?;
    let sources = store.list_sources()?;
    let path = match arg.strip_prefix(crate::command::PREFIX) {
        Some(name) => crate::command::cache_path(db, name),
        None => PathBuf::from(arg),
    };
    let abs = std::fs::canonicalize(&path).map(|p| p.to_string_lossy().into_owned());
    let pattern = glob_source::absolutize(arg);
    let alias = |s: &chrono_store::SourceRow| manifest_value(&s.manifest_json, "alias");
    let found = sources.iter().find(|s| {
        s.id == arg
            || alias(s).as_deref() == Some(arg)
            || abs.as_deref().is_ok_and(|a| a == s.path)
            || (s.path == pattern && GlobSource::from_row(&s.id, &s.manifest_json).is_some())
            // Un fichero que ya no está no se puede canonicalizar: su ruta absoluta.
            || std::path::absolute(&path).is_ok_and(|p| p.to_string_lossy() == s.path)
    });
    let Some(s) = found else {
        let names: Vec<String> = sources.iter().map(|s| alias(s).unwrap_or_else(|| s.id.clone())).collect();
        return Err(t(
            format!("no source matches {arg:?}. Sources: {} (see chrono sources)", names.join(", ")),
            format!("ninguna fuente casa con {arg:?}. Fuentes: {} (ver chrono sources)", names.join(", ")),
        )
        .into());
    };
    let n = store.remove_source(&s.id)?;
    store.rebuild_fts()?;
    finalize(&index_root_of(db), &store, &[], false, false)?;
    let total = store.list_sources()?.len();
    eprintln!("{}", t(
        format!("chrono: removed {} ({n} events). Index now has {total} sources.", s.id),
        format!("chrono: quitada {} ({n} eventos). El índice tiene ya {total} fuentes.", s.id),
    ));
    Ok(())
}

/// `chrono sync [path]`: sincroniza TODAS las fuentes registradas (o solo la de
/// `path`, si se pasa), ingiriendo el delta desde el watermark de cada una. Si
/// una fuente diverge (rebase/force-push, log rotado), reindexa SOLO esa fuente.
pub fn sync(db: &Path, source_arg: Option<&Path>) -> Result<()> {
    if !db.is_file() {
        return Err(chrono_store::NoIndex { path: db.to_path_buf() }.into());
    }
    let _lock = lock_ingest(db)?;
    let mut store = Store::open(db)?;
    sync_store(&mut store, &index_root_of(db), source_arg, None, false)
}

/// El cuerpo de `sync`, con el cerrojo ya tomado. `light` (`tail`, cada pocos
/// segundos): sin mensajes de progreso y sin `optimize` (FTS `optimize` y
/// estadísticas cuestan segundos a escala y no cambian ninguna cifra); todo lo
/// demás igual, así que tras un `tail` el índice queda como tras un `sync`.
///
/// `only` (`tail --source`): solo las fuentes que casan con ese filtro (lista de
/// kinds, kind o prefijo de id; ver [`source_selected`]).
fn sync_store(store: &mut Store, index_root: &Path, source_arg: Option<&Path>, only: Option<&str>, light: bool) -> Result<()> {
    let index_root = index_root.to_path_buf();
    let registry = build_registry();
    let say = |msg: String| {
        if !light {
            eprintln!("{msg}");
        }
    };

    let mut sources = store.list_sources()?;
    // Compat: índice anterior a multi-fuente (sin filas en `sources`). Se
    // reconstruye una fuente desde `meta` (repo_path/source_path + kind) para
    // que el primer `sync` tras actualizar se auto-cure.
    if sources.is_empty() {
        if let Some((id, kind, path, wm)) = legacy_source_from_meta(store)? {
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
    // De `total`, los que sustituyen a eventos borrados (relecturas de ficheros
    // reescritos, divergencias y cambios de dims): no son nuevos.
    let mut reread = 0usize;
    let mut synced_any = false;
    let mut matched_any = false;
    let mut diverged_any = false;
    // Solo las fuentes git que REALMENTE reingirieron necesitan refrescar
    // tamaños/tags/forge; si git no cambió (atajo de HEAD), no se re-tira del
    // forge de GitHub aunque otra fuente de log sí traiga eventos nuevos.
    let mut changed_git_paths: Vec<String> = Vec::new();
    // Fuentes que no se pudieron leer: las demás siguen (antes una sola, p. ej.
    // un fichero borrado, paraba el `sync` entero) y el error va al final.
    let mut failed: Vec<String> = Vec::new();
    let fail = |store: &Store, failed: &mut Vec<String>, id: &str, e: &dyn std::fmt::Display| -> Result<()> {
        let kept = store.count_source_events(id, None)?;
        if !light {
            eprintln!("{}", t(
                format!("  warning: can't read {id}: {e} — its {kept} indexed events are kept; the other sources go on"),
                format!("  aviso: no se pudo leer {id}: {e} — se conservan sus {kept} eventos indexados; las demás fuentes siguen"),
            ));
        }
        store.set_meta(&format!("sync_error:{id}"), &format!("{} {e}", now_iso()))?;
        failed.push(format!("{id} ({e})"));
        Ok(())
    };
    for s in &sources {
        if !source_selected(only, s) {
            continue;
        }
        if let Some(ref want) = filter_id {
            // También casa por ruta: una fuente añadida con `--as` puede no
            // ser la que `detect` elegiría hoy (p.ej. un fichero de Nod aún
            // vacío, o uno de `template`).
            let same_path = source_arg
                .and_then(|p| std::fs::canonicalize(p).ok())
                .is_some_and(|p| p.to_string_lossy() == s.path);
            // Una fuente glob casa por su patrón o por su alias.
            let same_glob = source_arg.is_some_and(|p| {
                let arg = p.to_string_lossy();
                GlobSource::from_row(&s.id, &s.manifest_json)
                    .is_some_and(|g| g.pattern == glob_source::absolutize(&arg) || g.alias.as_deref() == Some(arg.as_ref()))
            });
            if &s.id != want && !same_path && !same_glob {
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
        if let Some(gs) = GlobSource::from_row(&s.id, &s.manifest_json) {
            let cfg = chrono_classify_rules::load(&index_root);
            let ctx = IngestCtx { options_keys: gs.options_keys(), preset: gs.preset.as_deref(), group: Some((&s.id, "")), legacy_ids: false };
            let sides = relation_sides(&cfg, adapter, Path::new(&s.path), &ctx);
            let want = ingest_dims(adapter, &source_config(&cfg, Path::new(&s.path), &ctx), true, &sides);
            let reset = reset_if_dims_changed(store, &s.id, &want, &|st| gs.events_not_rereadable(st))?;
            let changed = reset.is_some();
            if changed {
                gs.clear_state(store)?;
            }
            let mut r = match gs.sync(adapter, store, &index_root) {
                Ok(r) => r,
                Err(e) => {
                    fail(store, &mut failed, &s.id, &e)?;
                    continue;
                }
            };
            r.diverged |= changed;
            store.upsert_source(&s.id, &s.kind, &s.path, &r.watermark, &gs.manifest(r.files_total), &now)?;
            if r.errors.is_empty() {
                store.delete_meta(&format!("sync_error:{}", s.id))?;
            } else {
                let e = r.errors.join("; ");
                store.set_meta(&format!("sync_error:{}", s.id), &format!("{} {e}", now_iso()))?;
                failed.push(format!("{} ({e})", s.id));
            }
            diverged_any |= r.diverged;
            synced_any |= r.events > 0 || r.diverged;
            total += r.events;
            reread += r.reread + reset.map_or(0, |d| d.min(r.events - r.reread));
            continue;
        }
        let path = PathBuf::from(&s.path);

        // Atajo de git: si el HEAD no cambió, no se abre el cursor.
        if s.kind == "git" {
            if let Ok(head) = chrono_source_git::head_sha(&path) {
                if s.watermark == format!("sha:{head}") {
                    // Se leyó bien: un error de un sync anterior ya no vale.
                    store.delete_meta(&format!("sync_error:{}", s.id))?;
                    continue;
                }
            }
        }

        let wm = if s.watermark.is_empty() {
            None
        } else {
            Some(Watermark { kind: s.kind.clone(), value: s.watermark.clone() })
        };

        let preset = manifest_value(&s.manifest_json, "preset");
        let ctx = IngestCtx { preset: preset.as_deref(), ..Default::default() };
        let cfg = chrono_classify_rules::load(&index_root);
        let sides = relation_sides(&cfg, adapter, &path, &ctx);
        let want = ingest_dims(adapter, &source_config(&cfg, &path, &ctx), false, &sides);
        // Un fichero que ya no está (o no se puede abrir) solo vive en el índice.
        let gone = |st: &Store| if s.kind != "git" && std::fs::File::open(&path).is_err() { st.count_source_events(&s.id, None) } else { Ok(0) };
        let reset = reset_if_dims_changed(store, &s.id, &want, &gone)?;
        let dims_changed = reset.is_some();
        let mut deleted = reset.unwrap_or(0);
        let wm = if dims_changed { None } else { wm };
        let (n, new_wm, diverged) = match ingest_source(adapter, &path, wm, store, &index_root, &ctx) {
            Ok((n, new_wm, _manifest)) => (n, new_wm, false),
            Err(IngestError::Diverged) => {
                deleted = store.count_source_events(&s.id, None)?;
                eprintln!("{}", t(
                    format!("  divergence in {} — reindexing that source", s.id),
                    format!("  divergencia en {} — se reindexa esa fuente", s.id),
                ));
                store.delete_source_events(&s.id)?;
                let (n, new_wm) = ingest_source(adapter, &path, None, store, &index_root, &ctx)
                    .map(|(n, new_wm, _)| (n, new_wm))
                    .map_err(|e| e.into_boxed())?;
                (n, new_wm, true)
            }
            Err(e) => {
                fail(store, &mut failed, &s.id, &e.into_boxed())?;
                continue;
            }
        };
        let diverged = diverged || dims_changed;
        store.set_source_watermark(&s.id, &new_wm.value, &now)?;
        store.delete_meta(&format!("sync_error:{}", s.id))?;
        // Una divergencia borró y reingirió la fuente: el índice cambió aunque el
        // conteo neto sea 0 (p.ej. el fichero encogió), así que FTS debe
        // reconstruirse por completo y finalize debe correr.
        if diverged {
            diverged_any = true;
        }
        if n > 0 || diverged {
            synced_any = true;
            if s.kind == "git" {
                changed_git_paths.push(s.path.clone());
                // Recomputar símbolos del delta (old HEAD..HEAD) si el índice los usa.
                if symbols_enabled {
                    let since_sha = s.watermark.strip_prefix("sha:").filter(|s| !s.is_empty());
                    ingest_symbols(&path, store, since_sha)?;
                }
            }
        }
        total += n;
        reread += n.min(deleted);
    }

    if filter_id.is_some() && !matched_any {
        eprintln!("{}", t(
            "sync: no matching source in the index",
            "sync: ninguna fuente del índice coincide",
        ));
        return Ok(());
    }

    // Lo que no se pudo leer sale como error al final, tras actualizar lo demás.
    let result = || -> Result<()> {
        if failed.is_empty() {
            return Ok(());
        }
        Err(t(
            format!("sync: {} source(s) could not be read: {} (their events are kept; see `sources`; chrono remove <source> drops one for good)", failed.len(), failed.join(", ")),
            format!("sync: {} fuente(s) no se pudieron leer: {} (sus eventos se conservan; ver `sources`; chrono remove <fuente> quita una del todo)", failed.len(), failed.join(", ")),
        )
        .into())
    };

    // Nada nuevo en ninguna fuente: no se reconstruye FTS ni se compacta.
    if !synced_any {
        say(t("sync: 0 new events", "sync: 0 eventos nuevos"));
        return result();
    }

    say(t("  building search index…", "  construyendo índice de búsqueda…"));
    // Una divergencia reescribe rowids arbitrarios: el watermark incremental de
    // FTS ya no es fiable, así que se reconstruye entero. Sin divergencia, solo
    // se indexan las filas nuevas (events.rowid > fts_max_rowid): O(delta).
    if diverged_any {
        store.rebuild_fts()?;
    } else {
        store.fts_sync()?;
    }
    finalize(&index_root, store, &changed_git_paths, !diverged_any, light)?;
    say(if reread > 0 {
        t(format!("sync: {} new events, {reread} reread", total - reread), format!("sync: {} eventos nuevos, {reread} releídos", total - reread))
    } else {
        t(format!("sync: {total} new events"), format!("sync: {total} eventos nuevos"))
    });
    result()
}

/// `chrono tail [--interval 2s] [--source delega,nod] [--kind/--level/--grep] [--since]`:
/// sigue el índice como `tail -f`. Cada intervalo hace un `sync` (delta de las
/// fuentes —solo las de `--source`, si se pasa—, FTS, rollups y plantillas al
/// día, reindexado si cambiaron dims u opciones; solo se salta `optimize`) e
/// imprime los eventos NUEVOS en orden cronológico, una línea por evento y al
/// momento (se vacía la salida tras cada línea): sirve de monitor único para un
/// orquestador (`chrono tail --source delega,nod --level WARN,ERROR`). Bucle
/// infinito; Ctrl-C para parar (o cerrar la tubería).
///
/// «Nuevo» es por `rid` (fila insertada), no por hora: un aviso de delega con
/// hora anterior al último evento de Nod se imprime igual. Con `since_epoch > 0`,
/// antes de seguir imprime lo ya indexado desde esa hora.
pub fn tail(
    db: &Path,
    interval_ms: i64,
    since_epoch: i64,
    source: Option<&str>,
    kind: Option<&str>,
    level: Option<&str>,
    grep: Option<&str>,
) -> Result<()> {
    let mut store = Store::open(db)?;
    let mut opened = file_id(db);
    let index_root = index_root_of(db);
    let interval = Duration::from_millis(interval_ms.clamp(200, 3_600_000) as u64);

    // `tail -f`: se sigue SOLO lo nuevo → desde el último `rid` ya presente. Por
    // `rid` y no por tiempo: con más de una página de golpe, los eventos del
    // mismo instante que el último mostrado se perdían (el índice sí los tenía).
    let max_rid = |st: &Store| st.conn().query_row("SELECT COALESCE(MAX(rid), 0) FROM events", [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    let mut last_rid = if since_epoch > 0 { 0 } else { max_rid(&store) };
    let mut last_err: Option<String> = None;

    eprintln!("{}", t(
        format!("chrono tail: following every {interval_ms} ms — Ctrl-C to stop"),
        format!("chrono tail: siguiendo cada {interval_ms} ms — Ctrl-C para parar"),
    ));

    let filter = chrono_metrics::EventFilter { since_epoch: since_epoch.max(0), source, kind, level, grep, ..Default::default() };
    let mut stdout = std::io::stdout();
    loop {
        {
            // El índice desapareció (borrado o movido): antes `Store::open` creaba
            // uno vacío en su sitio y el `tail` seguía sobre él.
            if !db.is_file() {
                return Err(t(
                    format!("the index {} is gone (deleted or moved); tail stops", db.display()),
                    format!("el índice {} ya no está (borrado o movido); tail se para", db.display()),
                )
                .into());
            }
            // Por vuelta, no durante todo el `tail`: un `sync` de un hook no espera horas.
            let _lock = lock_ingest(db)?;
            // Un `reindex` aparta el índice y crea otro en la misma ruta: con la
            // conexión vieja se seguía escribiendo en la copia (que luego se
            // borra). Si el fichero cambió, se reabre y se sigue desde lo que hay.
            if file_id(db) != opened {
                store = Store::open(db)?;
                opened = file_id(db);
                last_rid = max_rid(&store);
                eprintln!("{}", t(
                    "chrono tail: the index was replaced (reindex?); reopened, following from now",
                    "chrono tail: el índice cambió (¿reindex?); reabierto, se sigue desde ahora",
                ));
            }
            // Una fuente que falla (un fichero borrado) no para el `tail`: el
            // aviso sale una vez y las demás siguen al día.
            match sync_store(&mut store, &index_root, None, source, true) {
                Ok(()) => last_err = None,
                Err(e) => {
                    let msg = e.to_string();
                    if last_err.as_deref() != Some(msg.as_str()) {
                        eprintln!("{msg}");
                        last_err = Some(msg);
                    }
                }
            }
        }

        // Todo lo nuevo, de página en página, y en orden cronológico.
        let mut rows = Vec::new();
        loop {
            let page = chrono_metrics::events_after_rid(store.conn(), &filter, last_rid, 1000)?;
            let full = page.len() == 1000;
            if let Some((rid, _)) = page.last() {
                last_rid = *rid;
            }
            rows.extend(page.into_iter().map(|(_, r)| r));
            if !full {
                break;
            }
        }
        rows.sort_by(|a, b| a.at_epoch.cmp(&b.at_epoch).then_with(|| a.id.cmp(&b.id)));
        for r in &rows {
            let lvl: &str = if r.level.is_empty() { "-" } else { r.level.as_str() };
            let src = r.source_id.split(':').next().unwrap_or(&r.source_id);
            // Una línea por evento y vaciado inmediato: un monitor la ve al instante.
            if writeln!(stdout, "{}  {:5}  [{src}]  {}", r.at, lvl, r.title).and_then(|_| stdout.flush()).is_err() {
                return Ok(()); // la tubería se cerró (el monitor terminó)
            }
        }
        std::thread::sleep(interval);
    }
}

/// ¿Entra la fuente en el filtro `--source` de `tail`? Lista de kinds
/// (`delega,nod`), un kind, un prefijo de id (`jsonl:/var/log`) o el alias de
/// una fuente glob: lo mismo que casa el filtro de las consultas.
fn source_selected(filter: Option<&str>, s: &chrono_store::SourceRow) -> bool {
    let Some(f) = filter.map(str::trim).filter(|f| !f.is_empty()) else { return true };
    if f.contains(':') {
        return s.id.starts_with(f);
    }
    f.split(',').map(str::trim).any(|k| k == s.kind || GlobSource::from_row(&s.id, &s.manifest_json).and_then(|g| g.alias).as_deref() == Some(k))
}

/// Dispositivo e inodo del índice (`None` si no está): cambia si alguien lo
/// sustituye por otro fichero en la misma ruta.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).ok().and_then(|m| m.modified().ok()).map(|t| (0, t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)))
    }
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

/// Cerrojo de ingesta: `<db>.lock` con un cerrojo exclusivo del SO (se suelta
/// al cerrar el fichero, también si el proceso muere). Varios worktrees
/// comparten el índice de la raíz y sus hooks lanzan `sync` a la vez; sin esto
/// el segundo chocaba con la transacción larga del primero (`database is
/// locked` tras el `busy_timeout`) o repetía el mismo delta. Las consultas no
/// lo toman: leen del WAL sin bloquear.
///
/// Reentrante en el mismo hilo: `reindex` lo toma para todo su recorrido y
/// llama a `init`/`add`, que lo piden otra vez (un `flock` sobre otra
/// descripción del mismo fichero se bloquearía contra el propio proceso).
///
/// Con un `reindex` a medias (su marca sigue ahí), ninguna otra ingesta toca el
/// índice: está incompleto y la copia buena es otra.
pub(crate) fn lock_ingest(db: &Path) -> Result<IngestLock> {
    let lock = take_ingest_lock(db)?;
    if lock.file.is_some() {
        if let Some((backup, _)) = chrono_store::reindex_pending(db) {
            return Err(interrupted_reindex_msg(db, &backup).into());
        }
    }
    Ok(lock)
}

/// Como [`lock_ingest`], sin mirar la marca de reindex (lo usa `reindex`, que
/// es quien la pone y la quita).
pub(crate) fn lock_ingest_for_reindex(db: &Path) -> Result<IngestLock> {
    take_ingest_lock(db)
}

/// «Un reindex no terminó»: qué pasa y cómo seguir.
pub(crate) fn interrupted_reindex_msg(db: &Path, backup: &Path) -> String {
    t(
        format!(
            "a reindex of {} did not finish: the index may be incomplete and the good copy is {}. Resume it with: chrono --db {} reindex",
            db.display(), backup.display(), db.display()
        ),
        format!(
            "un reindex de {} no terminó: el índice puede estar a medias y la copia buena es {}. Reanúdalo con: chrono --db {} reindex",
            db.display(), backup.display(), db.display()
        ),
    )
}

thread_local! {
    /// Cerrojos de ingesta que este hilo ya tiene (ver [`lock_ingest`]).
    static HELD: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// El cerrojo de ingesta tomado; se suelta al soltarlo. `file` es `None` si ya
/// lo tenía este hilo (lo suelta quien lo tomó primero).
pub(crate) struct IngestLock {
    file: Option<std::fs::File>,
    path: PathBuf,
}

impl Drop for IngestLock {
    fn drop(&mut self) {
        if self.file.is_some() {
            HELD.with(|h| h.borrow_mut().retain(|p| p != &self.path));
        }
    }
}

fn take_ingest_lock(db: &Path) -> Result<IngestLock> {
    let path = PathBuf::from(format!("{}.lock", db.display()));
    if HELD.with(|h| h.borrow().contains(&path)) {
        return Ok(IngestLock { file: None, path });
    }
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
    if let Err(std::fs::TryLockError::WouldBlock) = f.try_lock() {
        eprintln!("{}", t(
            "chrono: another ingest is running on this index; waiting for it",
            "chrono: hay otra ingesta en curso sobre este índice; se espera a que termine",
        ));
        f.lock()?;
    }
    HELD.with(|h| h.borrow_mut().push(path.clone()));
    Ok(IngestLock { file: Some(f), path })
}

/// Abre el índice para `init`, que lo rehace con una sola fuente (`source_id`).
/// Si ya tiene otras fuentes (añadidas con `add`), `init` las borraría: error
/// que dice cuáles y manda a `sync`/`reindex`/`add`, del esquema que sea. Si no:
/// - de un chrono más nuevo, error (actualizar chrono; no se toca);
/// - más viejo y sin más fuentes que esta, se borra y se crea de nuevo.
///
/// Un fichero dañado o que no es un índice se aparta como `<db>.corrupt.bak` y
/// se crea otro. Cualquier otro error al abrir (ocupado, sin permisos) se
/// devuelve sin tocar nada: antes todo caía en «ilegible» y se borraba.
fn open_or_recreate(db: &Path, source_id: &str, explicit_db: bool) -> Result<Store> {
    let e = match Store::open(db) {
        Ok(s) => {
            let others: Vec<String> = s.list_sources()?.into_iter().map(|s| s.id).filter(|id| id != source_id).collect();
            if !others.is_empty() {
                return Err(init_would_drop(db, None, &others, explicit_db).into());
            }
            return Ok(s);
        }
        Err(e) if db.exists() => e,
        Err(e) => return Err(e),
    };
    let msg = match e.downcast_ref::<chrono_store::SchemaMismatch>() {
        // Sin versión: otro proceso lo está creando (o no es de chrono); no se toca.
        Some(m) if m.is_unknown() => return Err(crate::reindex::explain_open_error(&*e, db, explicit_db).unwrap_or_else(|| e.to_string()).into()),
        Some(m) if m.is_newer() => return Err(crate::reindex::explain_open_error(&*e, db, explicit_db).unwrap_or_else(|| e.to_string()).into()),
        Some(m) => {
            let others: Vec<String> = chrono_store::read_sources_any_version(db)
                .map_err(|re| format!("{e} ({re})"))?
                .into_iter()
                .map(|s| s.id)
                .filter(|id| id != source_id)
                .collect();
            if !others.is_empty() {
                return Err(init_would_drop(db, Some(&m.found_label()), &others, explicit_db).into());
            }
            t(
                format!("chrono: index from an older chrono (schema {}) with no other source; building it again", m.found_label()),
                format!("chrono: índice de una versión anterior (esquema {}) sin más fuentes; se crea de nuevo", m.found_label()),
            )
        }
        None if chrono_store::is_corrupt(&*e) => {
            let bak = PathBuf::from(format!("{}.corrupt.bak", db.display()));
            if bak.exists() {
                return Err(t(
                    format!("the index {} is damaged ({e}) and {} already exists: move one of them away and run init again", db.display(), bak.display()),
                    format!("el índice {} está dañado ({e}) y ya existe {}: aparta uno de los dos y repite init", db.display(), bak.display()),
                )
                .into());
            }
            std::fs::rename(db, &bak)?;
            for suffix in ["-wal", "-shm"] {
                let side = format!("{}{suffix}", db.display());
                if Path::new(&side).exists() {
                    std::fs::rename(&side, format!("{}{suffix}", bak.display()))?;
                }
            }
            t(
                format!("chrono: damaged index ({e}); kept as {} and building a new one", bak.display()),
                format!("chrono: índice dañado ({e}); se guarda como {} y se crea otro", bak.display()),
            )
        }
        None => {
            return Err(t(
                format!("can't open the index {}: {e}. Nothing was touched (if another process holds it, wait; check its permissions)", db.display()),
                format!("no se pudo abrir el índice {}: {e}. No se ha tocado nada (si otro proceso lo tiene, espera; revisa sus permisos)", db.display()),
            )
            .into())
        }
    };
    eprintln!("{msg}");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    Store::open(db)
}

/// Error de `init` sobre un índice con más fuentes que la suya: rehacerlo con
/// una sola las borraría. `old_schema`: el esquema, si es de un chrono anterior.
fn init_would_drop(db: &Path, old_schema: Option<&str>, others: &[String], explicit_db: bool) -> String {
    let pre = if explicit_db { format!("chrono --db {} ", db.display()) } else { "chrono ".to_string() };
    match old_schema {
        Some(v) => t(
            format!(
                "the index {} was built by an older chrono (schema {v}) and has {} more source(s) that init would drop ({}).\n\
                 Rebuild it with all its sources: {pre}reindex  (or delete it to start over with this one only)",
                db.display(), others.len(), others.join(", ")
            ),
            format!(
                "el índice {} es de una versión anterior de chrono (esquema {v}) y tiene {} fuente(s) más que init perdería ({}).\n\
                 Reconstrúyelo con todas sus fuentes: {pre}reindex  (o bórralo para empezar solo con esta)",
                db.display(), others.len(), others.join(", ")
            ),
        ),
        None => t(
            format!(
                "the index {} already has {} more source(s) that init would drop ({}).\n\
                 To update them all: {pre}sync · to reread only this one: {pre}add <path> · to rebuild everything: {pre}reindex  \
                 (or delete the index to start over with this one only)",
                db.display(), others.len(), others.join(", ")
            ),
            format!(
                "el índice {} ya tiene {} fuente(s) más que init borraría ({}).\n\
                 Para ponerlas todas al día: {pre}sync · para releer solo esta: {pre}add <ruta> · para rehacerlo todo: {pre}reindex  \
                 (o borra el índice para empezar solo con esta)",
                db.display(), others.len(), others.join(", ")
            ),
        ),
    }
}

/// De dónde salen las opciones de una ingesta y, en una fuente glob, a qué
/// fuente y fichero pertenecen sus eventos. `Default` = fuente de un fichero.
#[derive(Default)]
struct IngestCtx<'a> {
    /// Claves de `source_options` a probar antes que la ruta (alias y patrón de
    /// una fuente glob).
    options_keys: Vec<String>,
    /// Preset elegido en `add --preset`, si la config no pone otro.
    preset: Option<&'a str>,
    /// `(source_id de la fuente glob, ruta del fichero relativa a su base)`.
    group: Option<(&'a str, &'a str)>,
    /// En una glob, el fichero se empezó a leer con los ids de antes (sal solo
    /// del fichero, sin la fuente): sus deltas siguen así hasta que se relea
    /// entero, o el mismo evento entraría dos veces con dos ids.
    legacy_ids: bool,
}

/// Marca de las fuentes de un fichero cuyos ids de hash llevan la sal de la
/// fuente (ver [`id_salt`]). Va con las dims: una fuente indexada antes no la
/// tiene y el primer `sync` la reindexa una vez (borrando antes sus eventos).
const IDS_MARK: &str = "ids:2";

/// ¿Llevan sal de la fuente sus ids de hash? Las de fichero. Git no: el id es el
/// sha, y un commit es el mismo en cualquier clon o fork (ver [`id_salt`]).
/// Tampoco nod (sus ids son `nod:<instancia>:<run>:<seq>`, nunca de hash: la
/// marca solo costaría un reindex) ni oslog (no es un fichero: el log del
/// sistema es uno, y su ventana no se puede releer para reindexar).
fn salts_ids(source: &dyn Source) -> bool {
    !matches!(source.kind(), "git" | "nod" | "oslog")
}

/// Sal de los ids de hash (los que no llevan `:`; uno declarado con `id_field` o
/// `uid_field` ya es estable y puede repetirse a propósito): sin ella, dos
/// ficheros con la misma línea en el mismo offset (la misma cabecera, dos
/// réplicas del mismo log) darían el mismo id y el segundo evento se perdería
/// (`INSERT OR IGNORE`). En una glob, la fuente y el fichero (solo el fichero
/// si se empezó a leer con los ids de antes: dos globs con los mismos nombres
/// relativos chocaban); en una fuente de un fichero, su id.
///
/// Git queda fuera a propósito: dos clones o un fork del mismo repo como
/// fuentes comparten commits, y el commit es UN evento (el de la primera fuente
/// que lo indexó); los links `commit=`, `explain` y la resolución por prefijo de
/// sha dependen de que el id sea el sha.
fn id_salt(source: &dyn Source, path: &Path, ctx: &IngestCtx<'_>) -> Option<String> {
    let key = match ctx.group {
        Some((_, file)) if ctx.legacy_ids => file.to_string(),
        Some((id, file)) => format!("{id}\n{file}"),
        None if salts_ids(source) => canonical_source_id(source.kind(), path),
        None => return None,
    };
    Some(format!("{:08x}", fnv1a32(key.as_bytes())))
}

/// Deja la marca [`IDS_MARK`] en una fuente de un fichero recién leída entera
/// (`init`, `add`). El writer la escribe con el primer evento; esto cubre la que
/// no trajo ninguno, que si no se releería en el siguiente `sync`.
fn mark_ids(store: &Store, source: &dyn Source, source_id: &str) -> Result<()> {
    if salts_ids(source) {
        store.add_source_dims(source_id, &[IDS_MARK.to_string()])?;
    }
    Ok(())
}

/// Valor de texto `key` del `manifest_json` de una fuente.
fn manifest_value(manifest_json: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(manifest_json).ok()?;
    v.get(key)?.as_str().map(str::to_string)
}

/// FNV-1a de 32 bits (sal corta y estable para los ids de una fuente glob).
fn fnv1a32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(*b)).wrapping_mul(0x0100_0193))
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
    ctx: &IngestCtx<'_>,
) -> std::result::Result<(usize, Watermark, BTreeMap<String, String>), IngestError> {
    let cfg = chrono_classify_rules::load(repo_for_config);
    let source_cfg = source_config(&cfg, path, ctx);
    let sides = relation_sides(&cfg, source, path, ctx);
    let dims = ingest_dims(source, &source_cfg, ctx.group.is_some(), &sides);
    // Sin entidad propia, la del evento que comparte esta clave (`entity_via`).
    let entity_via = source.entity_via(&source_cfg);
    let salt = id_salt(source, path, ctx);
    // Caps de ingesta (0 = sin cap; por defecto no alteran nada → paridad).
    let body_max_bytes = cfg.body_max_bytes;
    let max_events = cfg.max_events_per_source;
    if body_max_bytes > 0 {
        let _ = store.set_meta("body_max_bytes", &body_max_bytes.to_string());
    }
    if max_events > 0 {
        let _ = store.set_meta("max_events_per_source", &max_events.to_string());
    }
    // Secretos fuera del índice salvo `"redact": "off"` en las opciones de la fuente.
    let redact = source_cfg.options.get("redact").map(String::as_str) != Some("off");
    let mut redacted = 0usize;
    let classifier = RulesClassifier::new(cfg);
    // Nod y delega son telemetría del PROCESO (sesiones y trabajos con agentes)
    // y dir un inventario de ficheros, no cambios del repo: no se clasifican
    // (un tool `fix_*` o un fichero `fix.pdf` no son un fix).
    let classify = !matches!(source.kind(), "nod" | "dir" | "delega");

    let mut cur: Box<dyn Cursor> = match source.open(path, watermark, &source_cfg) {
        Ok(c) => c,
        Err(CoreError::Diverged(_)) => return Err(IngestError::Diverged),
        Err(e) => return Err(IngestError::Other(e.to_string().into())),
    };

    let mut count = 0usize;
    let mut stderr = std::io::stderr();
    // Ingesta POR LOTES: se commitea cada INGEST_BATCH eventos (preservando las
    // cachés actor/entidad del writer) para que el WAL no crezca hasta el tamaño
    // del .db y el caudal no se desplome a escala. El orden de inserción y los
    // rowids son idénticos a un único commit (determinismo intacto).
    {
        let mut caches = chrono_store::WriterCaches::with_dims(dims);
        let mut done = false;
        while !done {
            let mut w = store.writer_seeded(std::mem::take(&mut caches))?;
            let mut in_batch = 0usize;
            loop {
                let mut ev = match cur.next() {
                    Ok(Some(ev)) => ev,
                    Ok(None) => {
                        done = true;
                        break;
                    }
                    Err(e) => return Err(IngestError::Other(e.to_string().into())),
                };
                // Cap de body (declarado en meta): recorte en frontera de carácter.
                if body_max_bytes > 0 && ev.body.len() > body_max_bytes {
                    let mut cut = body_max_bytes;
                    while cut > 0 && !ev.body.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    ev.body.truncate(cut);
                }
                if let Some((id, file)) = ctx.group {
                    ev.source_id = id.to_string();
                    ev.attrs.insert("file".to_string(), file.to_string());
                }
                // Solo los ids de hash (sin `:`), ver [`id_salt`].
                if let Some(salt) = salt.as_deref().filter(|_| !ev.id.contains(':')) {
                    ev.id = format!("{salt}.{}", ev.id);
                }
                relations::apply(&sides, &mut ev);
                if redact {
                    redacted += chrono_core::redact_event(&mut ev);
                }
                if let Some(rel) = &entity_via {
                    inherit_entity(&mut w, rel, &mut ev)?;
                }
                if !w.add_event(&ev)? {
                    // Ya estaba: no cuenta como nuevo ni se reclasifica.
                    continue;
                }
                if classify {
                    let labels = classifier.classify(&ev);
                    w.add_labels(&ev.id, &labels)?;
                }
                count += 1;
                in_batch += 1;
                if count.is_multiple_of(PROGRESS_EVERY) {
                    let _ = write!(stderr, "\r  {count}…");
                    let _ = stderr.flush();
                }
                // Cap de eventos por fuente (declarado en meta).
                if max_events > 0 && count >= max_events {
                    done = true;
                    break;
                }
                if in_batch >= INGEST_BATCH {
                    break; // cierra el lote, commitea y abre otro.
                }
            }
            caches = w.finish()?;
        }
        if count >= PROGRESS_EVERY {
            let _ = writeln!(stderr, "\r  {count}");
        }
    }

    if let Some(rel) = &entity_via {
        // Un resultado que entró antes que su uso (otro fichero de la glob, una
        // sesión reanudada) se quedó en `unknown`: segunda pasada en el índice.
        store.repair_entity_via(rel)?;
    }
    if redacted > 0 {
        // Acumulado del índice (no por fuente ni por sync): `sources` lo enseña.
        let prev: usize = store.meta("redacted_total")?.and_then(|v| v.parse().ok()).unwrap_or(0);
        store.set_meta("redacted_total", &(prev + redacted).to_string())?;
    }
    let wm = cur.watermark();
    let manifest = cur.manifest();
    Ok((count, wm, manifest))
}

/// Opciones del adaptador desde .chrono/config.json (columnas CSV, preset/year
/// de textlog…), resueltas por ruta/nombre de la fuente (o por alias/patrón en
/// una fuente glob), con el preset de `add --preset` si la config no pone otro.
fn source_config(cfg: &chrono_classify_rules::Config, path: &Path, ctx: &IngestCtx<'_>) -> SourceConfig {
    let mut options = ctx
        .options_keys
        .iter()
        .find_map(|k| cfg.source_options.get(k).cloned())
        .unwrap_or_else(|| if ctx.group.is_some() { BTreeMap::new() } else { cfg.source_options_for(path) });
    if let Some(preset) = ctx.preset {
        options.entry("preset".to_string()).or_insert_with(|| preset.to_string());
    }
    SourceConfig { options }
}

/// Dims con que se ingiere una fuente: las de su config (o preset) y, en una
/// fuente glob, `file` (cada evento lleva su fichero, y así se borra por fichero).
/// Llevan también la firma de sus `relations` (`rel:…`) y la de sus opciones
/// (`opt:…`, con el preset desplegado): cambiar una relación, `entity_via`,
/// `explode`, `attrs`, `links`… reindexa la fuente como cambiar una dim.
fn ingest_dims(source: &dyn Source, source_cfg: &SourceConfig, glob: bool, sides: &[relations::Side]) -> Vec<String> {
    let mut dims = source.dims(source_cfg);
    if glob && !dims.iter().any(|d| d == "file") {
        dims.push("file".to_string());
    }
    // Una glob no la lleva: reindexarla entera cuesta (GB de sesiones) y a menudo
    // no se puede (ficheros ya borrados); cambia de ids fichero a fichero.
    if !glob && salts_ids(source) {
        dims.push(IDS_MARK.to_string());
    }
    dims.extend(relations::signature(sides));
    dims.push(options_signature(source, source_cfg));
    dims.sort();
    dims
}

/// `opt:<hash>` de las opciones efectivas de la fuente (siempre presente, también
/// sin opciones: así una fuente indexada antes de esta firma se reconoce porque
/// no la tiene).
fn options_signature(source: &dyn Source, source_cfg: &SourceConfig) -> String {
    let opts = source.ingest_options(source_cfg).options;
    let flat: String = opts.iter().map(|(k, v)| format!("{k}\u{1f}{v}\u{1e}")).collect();
    format!("opt:{:08x}", fnv1a32(flat.as_bytes()))
}

/// ¿Cambió lo que se ingiere? Una fuente indexada antes de la firma de opciones
/// no la tiene: si el resto coincide se adopta la de hoy sin reindexar (no se
/// sabe con qué opciones se leyó, y reindexarlas todas al actualizar costaría
/// una relectura entera de cada una).
fn dims_changed(store: &Store, source_id: &str, want: &[String]) -> Result<bool> {
    let had = store.source_dims(source_id)?;
    // Sin la marca de los ids por fuente se relee una vez, aunque no le quede
    // ningún evento: una réplica exacta de otra fuente los perdió todos.
    let mark = IDS_MARK.to_string();
    if want.contains(&mark) && !had.contains(&mark) {
        return Ok(true);
    }
    if had == want || !store.source_has_events(source_id)? {
        return Ok(false);
    }
    let is_opt = |d: &&String| d.starts_with("opt:");
    if !had.iter().any(|d| is_opt(&d)) {
        let rest: Vec<&String> = want.iter().filter(|d| !is_opt(d)).collect();
        if had.iter().collect::<Vec<_>>() == rest {
            let opt: Vec<String> = want.iter().filter(is_opt).cloned().collect();
            store.add_source_dims(source_id, &opt)?;
            return Ok(false);
        }
    }
    Ok(true)
}

/// Extremos de `relations` que tocan a la fuente. Una glob se nombra por su id,
/// alias o patrón, nunca por el fichero que se está leyendo: la firma tiene
/// que ser la misma para todos sus ficheros.
fn relation_sides(cfg: &chrono_classify_rules::Config, source: &dyn Source, path: &Path, ctx: &IngestCtx<'_>) -> Vec<relations::Side> {
    match ctx.group {
        Some((id, _)) => relations::sides_for(cfg, source.kind(), id, Path::new(""), &ctx.options_keys),
        None => relations::sides_for(cfg, source.kind(), &canonical_source_id(source.kind(), path), path, &ctx.options_keys),
    }
}

/// `entity_via`: un evento sin entidad (solo `unknown`) toma la del primer
/// evento ya indexado con su misma clave `rel` (el resultado de una tool, la
/// de su uso). Se busca en el índice, no en el cursor: no se pierde si un
/// `sync` corta entre los dos ni si están en ficheros distintos.
fn inherit_entity(w: &mut chrono_store::Writer<'_>, rel: &str, ev: &mut chrono_core::Event) -> Result<()> {
    let unknown = ev.touches.iter().all(|t| t.entity == "unknown");
    if !unknown {
        return Ok(());
    }
    let Some(key) = ev.links.iter().find(|l| l.rel == rel).map(|l| l.target.clone()) else { return Ok(()) };
    if let Some((entity, entity_type)) = w.linked_entity(rel, &key)? {
        match ev.touches.first_mut() {
            Some(t) => {
                t.entity = entity;
                t.entity_type = entity_type;
            }
            None => ev.touches.push(chrono_core::Touch { entity, entity_type, weight: 1, attrs: BTreeMap::new() }),
        }
    }
    Ok(())
}

/// Si la fuente se ingirió con otras dims que las de la config de hoy, sus
/// eventos se borran (la próxima ingesta la rehace entera): las claves que
/// salieron de `attrs` no se pueden recuperar sin releer la fuente.
///
/// Salvo si parte de la fuente ya no se puede releer (`not_rereadable`: un
/// fichero de la glob borrado, el fichero de la fuente desaparecido): borrarla
/// perdería esos eventos para siempre. Entonces no se reindexa: las dims u
/// opciones de hoy valen para lo nuevo, lo ya indexado conserva las de antes
/// (`sources` lo cuenta) y el aviso dice cuántos eventos se habrían perdido.
///
/// Devuelve cuántos eventos se borraron (`None` si no se borró nada), para
/// contar como releídos y no como nuevos los que los sustituyan.
fn reset_if_dims_changed(
    store: &Store,
    source_id: &str,
    want: &[String],
    not_rereadable: &dyn Fn(&Store) -> Result<usize>,
) -> Result<Option<usize>> {
    if !dims_changed(store, source_id, want)? {
        return Ok(None);
    }
    let lost = not_rereadable(store)?;
    if lost > 0 {
        let kept = store.count_source_events(source_id, None)?;
        eprintln!("{}", t(
            format!(
                "  dims or options of {source_id} changed, but {lost} of its events can no longer be reread (deleted or unreadable files): \
                 not reindexing. New events get the new ones; the {kept} already indexed keep the old ones (see `sources`). \
                 To reindex anyway, losing those {lost}: chrono add <source> again"
            ),
            format!(
                "  las dims u opciones de {source_id} cambiaron, pero {lost} de sus eventos ya no se pueden releer (ficheros borrados o ilegibles): \
                 no se reindexa. Lo nuevo entra con las de hoy; los {kept} ya indexados conservan las de antes (ver `sources`). \
                 Para reindexar igualmente, perdiendo esos {lost}: chrono add <fuente> otra vez"
            ),
        ));
        store.set_source_dims(source_id, want)?;
        return Ok(None);
    }
    let had = store.source_dims(source_id)?;
    let mark = IDS_MARK.to_string();
    if want.contains(&mark) && !had.contains(&mark) {
        eprintln!("{}", t(
            format!("  {source_id} was indexed with ids that could collide with another source's — reindexing that source (once)"),
            format!("  {source_id} se indexó con ids que podían chocar con los de otra fuente — se reindexa esa fuente (una vez)"),
        ));
    } else {
        eprintln!("{}", t(
            format!("  dims or options of {source_id} changed ({} → {}) — reindexing that source", had.join(","), want.join(",")),
            format!("  las dims u opciones de {source_id} cambiaron ({} → {}) — se reindexa esa fuente", had.join(","), want.join(",")),
        ));
    }
    let deleted = store.count_source_events(source_id, None)?;
    store.delete_source_events(source_id)?;
    // La marca, aunque la relectura no traiga ningún evento (el writer solo
    // escribe las dims con el primero): si no, se releería en cada `sync`.
    if want.contains(&mark) {
        store.add_source_dims(source_id, &[mark])?;
    }
    Ok(Some(deleted))
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
/// `rollups_incremental`: si `true`, los rollups solo agregan el delta de
/// eventos nuevos (O(delta), para `sync`/`add` sin divergencia); si `false`, se
/// reconstruyen enteros (init, re-ingesta y divergencia, donde los conteos
/// previos ya no son fiables).
fn finalize(
    index_root: &Path,
    store: &Store,
    git_paths: &[String],
    rollups_incremental: bool,
    light: bool,
) -> Result<()> {
    // Un repo sin commits no tiene árbol, tags ni forge que mirar.
    let git_paths: Vec<&String> = git_paths.iter().filter(|p| !chrono_source_git::head_is_unborn(Path::new(p))).collect();
    for path in &git_paths {
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
        for path in &git_paths {
            finalize_git_forge(Path::new(path), store, &cfg.bug_labels)?;
        }
    }

    // Rollups Drain-light (plantillas + conteos por bucket) para `patterns`.
    // Incremental en sync/add (solo el delta); completo en init/divergencia.
    if !light {
        eprintln!("{}", t("  clustering log templates…", "  agrupando plantillas…"));
    }
    if rollups_incremental {
        store.build_rollups_sync(ROLLUP_BUCKET_MS)?;
    } else {
        store.build_rollups(ROLLUP_BUCKET_MS)?;
    }
    store.set_meta("rollup_bucket_ms", &ROLLUP_BUCKET_MS.to_string())?;

    // ANALYZE completo solo en reconstrucción (init/divergencia): a escala es
    // O(N) y no debe correr en cada sync incremental (PRAGMA optimize basta).
    // `tail` no lo hace: no cambia cifras y costaría segundos en cada vuelta.
    if !light {
        store.optimize(!rollups_incremental)?;
    }
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
        let touches: Vec<(String, String, i64, i64, i64, String)> = syms
            .iter()
            .map(|s| {
                (s.entity_key(), "symbol".to_string(), s.added + s.deleted, s.added, s.deleted, symbol_attrs_json(s))
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_cerrojo_de_ingesta_excluye_a_un_segundo_escritor() {
        let db = std::env::temp_dir().join(format!("chrono-lock-test-{}.db", std::process::id()));
        let held = lock_ingest(&db).unwrap();
        // Otra apertura (como la de un segundo `sync`) no puede tomarlo mientras dure.
        let other = std::fs::OpenOptions::new().write(true).open(format!("{}.lock", db.display())).unwrap();
        assert!(matches!(other.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        drop(held);
        // Otros tests del proceso lanzan comandos en paralelo: entre su `fork` y su
        // `exec` el hijo hereda el descriptor y el cerrojo dura un instante más.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while other.try_lock().is_err() {
            assert!(std::time::Instant::now() < deadline, "el cerrojo no se soltó");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(format!("{}.lock", db.display()));
    }

    fn count(db: &Path, sql: &str) -> i64 {
        Store::open_read(db).unwrap().conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn cambiar_las_dims_en_la_config_reindexa_esa_fuente_con_las_mismas_cifras() {
        let dir = std::env::temp_dir().join(format!("chrono-dims-src-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("datos")).unwrap();
        std::fs::create_dir_all(dir.join(".chrono")).unwrap();
        let line = |i: usize| format!("{{\"ts\":{},\"msg\":\"hola {i}\",\"service\":\"api\",\"host\":\"h{}\"}}\n", 1_700_000_000 + i, i % 3);
        std::fs::write(dir.join("datos/a.jsonl"), (0..9).map(line).collect::<String>()).unwrap();
        let config = |dims: &str| {
            std::fs::write(dir.join(".chrono/config.json"), format!("{{\"version\":1,\"source_options\":{{\"datos\":{{\"dims\":\"{dims}\"}}}}}}")).unwrap()
        };
        config("host");
        let db = dir.join(".chrono/index.db");
        add_glob(&db, &format!("{}/datos/*.jsonl", dir.display()), Some("jsonl"), Some("datos"), None).unwrap();
        let host_dims = "SELECT COUNT(*) FROM event_dims d JOIN dim_values v ON v.id = d.dim_id WHERE v.key = 'host'";
        assert_eq!(count(&db, host_dims), 9);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events WHERE json_extract(attrs, '$.host') IS NOT NULL"), 0);
        // `msg`, `ts` y `service` ya son título, tiempo y entidad: no se repiten en attrs.
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events WHERE attrs <> '{}'"), 0);

        // Sin `host` en las dims: el siguiente sync rehace la fuente con host en attrs.
        config("");
        sync(&db, None).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 9);
        assert_eq!(count(&db, host_dims), 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events WHERE json_extract(attrs, '$.host') = 'h1'"), 3);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM dim_values WHERE key = 'file'"), 1);
        // Los rollups se rehicieron sobre los eventos nuevos (watermark al día).
        let max_rid = count(&db, "SELECT MAX(rid) FROM events");
        assert_eq!(count(&db, "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'rollup_max_rowid'"), max_rid);
        assert_eq!(count(&db, "SELECT SUM(count) FROM dim_rollups"), 9);
        // Igual que antes: nada que hacer (no se relee: los rids no cambian).
        sync(&db, None).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 9);
        assert_eq!(count(&db, "SELECT MAX(rid) FROM events"), max_rid);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn una_fuente_glob_es_una_fila_y_solo_relee_lo_que_crece() {
        let dir = std::env::temp_dir().join(format!("chrono-glob-src-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let line = |i: usize| format!("{{\"ts\":{},\"msg\":\"hola\",\"service\":\"api\"}}\n", 1_700_000_000 + i);
        for f in ["p1/a.jsonl", "p1/b.jsonl", "p2/sub/c.jsonl"] {
            let p = dir.join("datos").join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            // Las tres primeras líneas son IGUALES en los tres ficheros (mismo offset):
            // sin la sal por fichero, sus ids chocarían y se perderían eventos.
            std::fs::write(&p, (0..3).map(line).collect::<String>()).unwrap();
        }
        let db = dir.join(".chrono/index.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let pattern = format!("{}/datos/**/*.jsonl", dir.display());
        add_glob(&db, &pattern, Some("jsonl"), Some("datos"), None).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM sources"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 9);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM dim_values WHERE key = 'file'"), 3);

        // Crece un fichero y aparece otro: solo entran sus líneas nuevas.
        let mut f = std::fs::OpenOptions::new().append(true).open(dir.join("datos/p1/a.jsonl")).unwrap();
        f.write_all(line(7).as_bytes()).unwrap();
        std::fs::write(dir.join("datos/p2/d.jsonl"), line(8)).unwrap();
        sync(&db, None).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 11);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM sources"), 1);
        let manifest: String = Store::open_read(&db).unwrap().list_sources().unwrap()[0].manifest_json.clone();
        assert!(manifest.contains("\"files\":\"4\""), "{manifest}");

        // Un fichero reescrito más corto diverge: se reingiere SOLO ese fichero.
        std::fs::write(dir.join("datos/p1/b.jsonl"), line(9)).unwrap();
        sync(&db, Some(Path::new("datos"))).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 9);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM event_dims d JOIN dim_values v ON v.id = d.dim_id WHERE v.key = 'file' AND v.value = 'p1/b.jsonl'"), 1);

        // Sin cambios: nada que leer.
        sync(&db, None).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 9);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Glob indexada antes de la sal con la fuente: no se reindexa; cada fichero
    /// sigue con sus ids de antes mientras solo crece (un delta nunca repite un
    /// evento con otro id) y pasa a los nuevos cuando se relee entero.
    #[test]
    fn una_glob_de_antes_cambia_de_ids_fichero_a_fichero() {
        let dir = std::env::temp_dir().join(format!("chrono-glob-ids-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let line = |i: usize| format!("{{\"ts\":{},\"msg\":\"hola\"}}\n", 1_700_000_000 + i);
        std::fs::create_dir_all(dir.join("datos")).unwrap();
        std::fs::write(dir.join("datos/a.jsonl"), (0..3).map(line).collect::<String>()).unwrap();
        let db = dir.join(".chrono/index.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let pattern = format!("{}/datos/*.jsonl", dir.display());
        add_glob(&db, &pattern, Some("jsonl"), None, None).unwrap();
        let id = format!("jsonl:{pattern}");
        let old = format!("{:08x}", fnv1a32(b"a.jsonl"));
        let new = format!("{:08x}", fnv1a32(format!("{id}\na.jsonl").as_bytes()));
        let with = |salt: &str| count(&db, &format!("SELECT COUNT(*) FROM events WHERE id GLOB '{salt}.*'"));
        assert_eq!(with(&new), 3);

        // Como lo dejaba la versión de antes: sal solo del fichero y estado sin versión.
        let store = Store::open(&db).unwrap();
        store.conn().execute(&format!("UPDATE events SET id = '{old}' || substr(id, 9)"), []).unwrap();
        store.conn().execute("UPDATE meta SET value = json_remove(value, '$.\"a.jsonl\"[5]') WHERE key GLOB 'glob_state:*'", []).unwrap();
        drop(store);
        assert_eq!(with(&old), 3);

        // Crece: el delta sigue con la sal de antes y nada se relee.
        let mut f = std::fs::OpenOptions::new().append(true).open(dir.join("datos/a.jsonl")).unwrap();
        f.write_all(line(3).as_bytes()).unwrap();
        drop(f);
        sync(&db, None).unwrap();
        assert_eq!((with(&old), with(&new)), (4, 0));

        // Reescrito (más corto): se relee entero, ya con la sal de la fuente.
        std::fs::write(dir.join("datos/a.jsonl"), (0..2).map(line).collect::<String>()).unwrap();
        sync(&db, None).unwrap();
        assert_eq!((with(&old), with(&new)), (0, 2));
        let mut f = std::fs::OpenOptions::new().append(true).open(dir.join("datos/a.jsonl")).unwrap();
        f.write_all(line(5).as_bytes()).unwrap();
        drop(f);
        sync(&db, None).unwrap();
        assert_eq!((with(&old), with(&new)), (0, 3));
        std::fs::remove_dir_all(&dir).ok();
    }
}
