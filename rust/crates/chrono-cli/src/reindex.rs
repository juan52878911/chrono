//! `chrono reindex`: reconstruye el índice desde cero con las MISMAS fuentes.
//!
//! Hace falta cuando una versión nueva de chrono cambia el esquema y no puede
//! migrar el índice al abrirlo. Antes la única salida era borrar el `.db` y
//! volver a `init`, que solo recupera la fuente principal: las añadidas con
//! `add` (logs, globs, comandos) se perdían. Aquí se leen las fuentes del índice
//! viejo (sin abrirlo con `Store::open`, que lo rechaza), se vuelve a añadir
//! cada una con su tipo, alias y preset en un índice aparte y este sustituye al
//! viejo al acabar (ver [`run`]).

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::i18n::t;
use crate::index::{self, Result};

/// Ruta de la copia del índice viejo: `index.db.v5.bak`, o `index.db.v5.2.bak`…
/// si ya hay una (una copia que se conservó no se pisa).
fn backup_path(db: &Path, version: &str) -> PathBuf {
    let name = db.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "index.db".into());
    let taken = |p: &Path| ["", "-wal", "-shm"].iter().any(|s| Path::new(&format!("{}{s}", p.display())).exists());
    let mut n = 1;
    loop {
        let p = db.with_file_name(if n == 1 { format!("{name}.v{version}.bak") } else { format!("{name}.v{version}.{n}.bak") });
        if !taken(&p) {
            return p;
        }
        n += 1;
    }
}

/// Mueve el `.db` y sus `-wal`/`-shm` de `from` a `to` (los que existan).
fn move_db(from: &Path, to: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{suffix}", from.display()));
        if src.exists() {
            std::fs::rename(&src, format!("{}{suffix}", to.display()))?;
        }
    }
    Ok(())
}

/// Borra el `.db` en `p` y sus `-wal`/`-shm` (los que existan).
fn remove_db(p: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", p.display()));
    }
}

/// Ruta donde se construye el índice nuevo: `<db>.new`, junto al bueno.
fn new_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}.new", db.display()))
}

/// Todo el recorrido va bajo el cerrojo de ingesta: dos `reindex` a la vez se
/// pisaban y se perdían fuentes.
///
/// El índice nuevo se construye aparte, en `<db>.new`, mientras el de siempre
/// sigue en su sitio y las consultas lo leen. Al acabar, los dos se vacían de
/// WAL (`wal_checkpoint(TRUNCATE)`) y `<db>.new` sustituye a `<db>` con un
/// `rename` atómico: nunca hay un instante sin `<db>` y nunca se renombra un
/// `-wal`/`-shm` en uso (antes se apartaba el índice con sus `-wal`/`-shm`
/// mientras los lectores los tenían abiertos: «disk I/O error», y un lector que
/// llegaba entre medias no encontraba índice). Si el proceso muere a medias, el
/// índice de siempre sigue intacto y el siguiente `reindex` descarta `<db>.new`.
///
/// Una fuente que no se puede reindexar (p. ej. su fichero ya no existe) no lo
/// bloquea: queda fuera con un aviso y sus cifras, la copia del índice viejo
/// (que sí la tiene) se conserva como `<db>.v<N>.bak` y `run` devuelve `true`
/// (el CLI sale con 3). Igual si el índice nuevo tiene menos eventos.
pub fn run(db: &Path) -> Result<bool> {
    let _lock = index::lock_ingest_for_reindex(db)?;
    resume_legacy(db)?;
    let sources = chrono_store::read_sources_any_version(db).map_err(|e| {
        t(
            format!("can't read the sources of {}: {e}. Delete it and run chrono init (and chrono add for each extra source)", db.display()),
            format!("no pude leer las fuentes de {}: {e}. Bórralo y ejecuta chrono init (y chrono add por cada fuente extra)", db.display()),
        )
    })?;
    if sources.is_empty() {
        return Err(t("the index has no sources to reindex; use chrono init", "el índice no tiene fuentes que reindexar; usa chrono init").into());
    }
    let symbols = chrono_store::read_meta_any_version(db, "symbols").as_deref() == Some("1");
    let version = chrono_store::read_meta_any_version(db, "schema_version").unwrap_or_else(|| "0".into());
    let before = chrono_store::count_events_any_version(db);
    let new = new_path(db);
    // Lo que dejara un reindex muerto a medias.
    remove_db(&new);
    let _ = std::fs::remove_file(format!("{}.lock", new.display()));
    eprintln!("{}", t(
        format!("chrono: reindexing {} sources into {} (queries keep using the current index until it finishes)", sources.len(), new.display()),
        format!("chrono: reindexando {} fuentes en {} (las consultas siguen con el índice actual hasta que acabe)", sources.len(), new.display()),
    ));
    let r = rebuild(&new, &sources, symbols).and_then(|failed| swap(db, &new, &version, &sources, &failed, before));
    remove_db(&new);
    let _ = std::fs::remove_file(format!("{}.lock", new.display()));
    r
}

/// Un `reindex` de antes (apartaba el índice y dejaba la marca `<db>.reindexing`
/// con la ruta de la copia) que se cortó: la copia es la buena y vuelve a su
/// sitio; el índice a medias se descarta.
fn resume_legacy(db: &Path) -> Result<()> {
    let marker = chrono_store::reindex_marker(db);
    let Some((backup, _)) = chrono_store::reindex_pending(db) else { return Ok(()) };
    if !backup.exists() {
        return Err(t(
            format!(
                "{} says a reindex was interrupted, but its copy {} is not there. If {} is fine (the reindex died before moving it), delete {} and try again",
                marker.display(), backup.display(), db.display(), marker.display()
            ),
            format!(
                "{} dice que un reindex se cortó, pero su copia {} no está. Si {} está bien (murió antes de apartarlo), borra {} y repite",
                marker.display(), backup.display(), db.display(), marker.display()
            ),
        )
        .into());
    }
    eprintln!("{}", t(
        format!("chrono: resuming an interrupted reindex from {} (the half-built index is discarded)", backup.display()),
        format!("chrono: se reanuda un reindex cortado desde {} (se descarta el índice a medias)", backup.display()),
    ));
    remove_db(db);
    move_db(&backup, db)?;
    std::fs::remove_file(&marker)?;
    Ok(())
}

/// Añade cada fuente al índice nuevo `new`. Devuelve las que fallaron, con el error.
fn rebuild(new: &Path, sources: &[chrono_store::SourceRow], symbols: bool) -> Result<Vec<(String, String)>> {
    let mut failed = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        let m: Value = serde_json::from_str(&s.manifest_json).unwrap_or(Value::Null);
        let get = |k: &str| m.get(k).and_then(Value::as_str);
        let r = if let Some(pattern) = get("glob") {
            index::add_glob(new, pattern, Some(&s.kind), get("alias"), get("preset"))
        } else if i == 0 && s.kind == "git" {
            // La fuente principal: `init` además deja el repo en `meta`.
            index::init(Path::new(&s.path), symbols, Some("git"), Some(new))
        } else {
            index::add(new, Path::new(&s.path), symbols && s.kind == "git", Some(&s.kind), get("preset"))
        };
        if let Err(e) = r {
            eprintln!("chrono: {}: {e}", s.id);
            failed.push((s.id.clone(), e.to_string()));
        }
    }
    if failed.len() == sources.len() {
        return Err(t(
            "no source could be reindexed; the index was not touched",
            "no se pudo reindexar ninguna fuente; el índice no se tocó",
        )
        .into());
    }
    Ok(failed)
}

/// Pone el índice nuevo en su sitio (ver [`run`]) y lo cuenta. Devuelve `true`
/// si el índice nuevo tiene menos que el viejo (fuentes fuera o menos eventos):
/// entonces el viejo se conserva como copia.
fn swap(
    db: &Path,
    new: &Path,
    version: &str,
    sources: &[chrono_store::SourceRow],
    failed: &[(String, String)],
    before: Option<i64>,
) -> Result<bool> {
    let after = chrono_store::count_events_any_version(new);
    // Cifras de lo que queda fuera, del índice viejo (antes de ponerle otro nombre:
    // abrir la copia le dejaría sus propios -wal/-shm).
    let left_out: Vec<String> = failed
        .iter()
        .map(|(id, _)| match chrono_store::count_source_events_any_version(db, id) {
            Some(n) => t(format!("{id} ({n} events)"), format!("{id} ({n} eventos)")),
            None => id.clone(),
        })
        .collect();
    let degraded = !failed.is_empty() || matches!((before, after), (Some(b), Some(a)) if a < b);
    let wait = std::time::Duration::from_secs(60);
    for p in [new, db] {
        chrono_store::checkpoint_truncate(p, wait).map_err(|e| {
            t(
                format!("{e} (readers kept {} busy for a minute; the index was not touched, try again)", p.display()),
                format!("{e} (los lectores tuvieron {} ocupado un minuto; el índice no se tocó, repite)", p.display()),
            )
        })?;
    }
    let backup = if degraded {
        // El viejo, ya sin nada en el WAL, queda también con otro nombre: un
        // enlace (o una copia si el sistema de ficheros no los admite).
        let b = backup_path(db, version);
        if std::fs::hard_link(db, &b).is_err() {
            std::fs::copy(db, &b)?;
        }
        Some(b)
    } else {
        None
    };
    std::fs::rename(new, db)?;
    let counts = match (before, after) {
        (Some(b), Some(a)) => format!("{b} → {a}"),
        (_, Some(a)) => format!("? → {a}"),
        _ => "?".into(),
    };
    let Some(backup) = backup else {
        eprintln!("{}", t(
            format!("chrono: reindex done ({} sources, events {counts}).", sources.len()),
            format!("chrono: reindexado ({} fuentes, eventos {counts}).", sources.len()),
        ));
        return Ok(false);
    };
    if !failed.is_empty() {
        let list = left_out;
        eprintln!("{}", t(
            format!(
                "chrono: reindexed {} of {} sources (events {counts}). Left out because they could not be read: {}. \
                 The old index, with them, stays at {}. Add them again with chrono add once they are back; \
                 delete the copy when you no longer need it.",
                sources.len() - failed.len(), sources.len(), list.join(", "), backup.display()
            ),
            format!(
                "chrono: reindexadas {} de {} fuentes (eventos {counts}). Quedan fuera porque no se pudieron leer: {}. \
                 El índice viejo, con ellas, sigue en {}. Vuelve a añadirlas con chrono add cuando estén; \
                 borra la copia cuando ya no la necesites.",
                sources.len() - failed.len(), sources.len(), list.join(", "), backup.display()
            ),
        ));
    } else {
        // Que todas las fuentes se añadan no basta: un fichero truncado o rotado
        // desde la última ingesta deja menos eventos.
        eprintln!("{}", t(
            format!(
                "chrono: reindexed {} sources, but with fewer events than before ({counts}): some source no longer has all its data. The old index stays at {}; delete it once you've checked.",
                sources.len(),
                backup.display()
            ),
            format!(
                "chrono: reindexadas {} fuentes, pero con menos eventos que antes ({counts}): alguna fuente ya no tiene todos sus datos. El índice viejo sigue en {}; bórralo cuando lo hayas comprobado.",
                sources.len(),
                backup.display()
            ),
        ));
    }
    Ok(true)
}

/// Mensaje de un error al abrir el índice: si es de otra versión de esquema,
/// qué versión tiene, cuál hace falta y el comando exacto para reconstruirlo.
pub fn explain_open_error(e: &(dyn std::error::Error + 'static), db: &Path, explicit_db: bool) -> Option<String> {
    if let Some(n) = e.downcast_ref::<chrono_store::NoIndex>() {
        // Antes salía en español con cualquier CHRONO_LANG.
        return Some(t(
            format!("no index at {} (create it with chrono init)", n.path.display()),
            format!("no hay índice en {} (créalo con chrono init)", n.path.display()),
        ));
    }
    let m = e.downcast_ref::<chrono_store::SchemaMismatch>()?;
    if m.is_unknown() {
        return Some(t(
            format!("the index {} doesn't say its schema version: another process is creating it right now (try again in a moment) or it isn't a chrono index", db.display()),
            format!("el índice {} no dice su versión de esquema: otro proceso lo está creando ahora mismo (repite en un momento) o no es un índice de chrono", db.display()),
        ));
    }
    if m.is_newer() {
        return Some(t(
            format!("the index {} was built by a newer chrono (schema {}; this one reads v{}): update chrono", db.display(), m.found_label(), m.want),
            format!("el índice {} es de una versión más reciente de chrono (esquema {}; esta lee el v{}): actualiza chrono", db.display(), m.found_label(), m.want),
        ));
    }
    let cmd = if explicit_db { format!("chrono --db {} reindex", db.display()) } else { "chrono reindex".to_string() };
    Some(t(
        format!(
            "the index {} was built by an older chrono (schema {}; this version needs v{}).\nRebuild it with the same sources: {cmd}",
            db.display(),
            m.found_label(),
            m.want
        ),
        format!(
            "el índice {} es de una versión anterior de chrono (esquema {}; esta versión necesita el v{}).\nReconstrúyelo con las mismas fuentes: {cmd}",
            db.display(),
            m.found_label(),
            m.want
        ),
    ))
}
