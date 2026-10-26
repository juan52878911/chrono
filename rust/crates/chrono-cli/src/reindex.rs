//! `chrono reindex`: reconstruye el índice desde cero con las MISMAS fuentes.
//!
//! Hace falta cuando una versión nueva de chrono cambia el esquema y no puede
//! migrar el índice al abrirlo. Antes la única salida era borrar el `.db` y
//! volver a `init`, que solo recupera la fuente principal: las añadidas con
//! `add` (logs, globs, comandos) se perdían. Aquí se leen las fuentes del índice
//! viejo (sin abrirlo con `Store::open`, que lo rechaza), se aparta como copia y
//! se vuelve a añadir cada una con su tipo, alias y preset.

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

/// Todo el recorrido va bajo el cerrojo de ingesta: dos `reindex` a la vez se
/// apartaban el índice el uno al otro (el segundo tomaba el parcial del primero
/// por bueno) y se perdían fuentes. Antes de apartar el índice se deja la marca
/// `<db>.reindexing` con la ruta de la copia; si el proceso muere a medias, la
/// marca sigue ahí, los demás comandos lo dicen y el siguiente `reindex`
/// reanuda desde esa copia (descarta el índice a medias).
pub fn run(db: &Path) -> Result<()> {
    let _lock = index::lock_ingest_for_reindex(db)?;
    let marker = chrono_store::reindex_marker(db);
    let resume = match chrono_store::reindex_pending(db) {
        Some((backup, _)) if !backup.exists() => {
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
        Some((backup, _)) => {
            eprintln!("{}", t(
                format!("chrono: resuming an interrupted reindex from {} (the half-built index is discarded)", backup.display()),
                format!("chrono: se reanuda un reindex cortado desde {} (se descarta el índice a medias)", backup.display()),
            ));
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
            }
            Some(backup)
        }
        None => None,
    };
    // Las fuentes salen del índice o, al reanudar, de la copia buena.
    let src_db = resume.clone().unwrap_or_else(|| db.to_path_buf());
    let sources = chrono_store::read_sources_any_version(&src_db).map_err(|e| {
        t(
            format!("can't read the sources of {}: {e}. Delete it and run chrono init (and chrono add for each extra source)", src_db.display()),
            format!("no pude leer las fuentes de {}: {e}. Bórralo y ejecuta chrono init (y chrono add por cada fuente extra)", src_db.display()),
        )
    })?;
    if sources.is_empty() {
        return Err(t("the index has no sources to reindex; use chrono init", "el índice no tiene fuentes que reindexar; usa chrono init").into());
    }
    let symbols = chrono_store::read_meta_any_version(&src_db, "symbols").as_deref() == Some("1");
    let version = chrono_store::read_meta_any_version(&src_db, "schema_version").unwrap_or_else(|| "0".into());
    let before = chrono_store::count_events_any_version(&src_db);
    let backup = match resume {
        Some(b) => b,
        None => {
            let backup = backup_path(db, &version);
            std::fs::write(&marker, backup.to_string_lossy().as_bytes())?;
            if let Err(e) = move_db(db, &backup) {
                let _ = std::fs::remove_file(&marker);
                return Err(e.into());
            }
            backup
        }
    };
    eprintln!("{}", t(
        format!("chrono: reindexing {} sources (old index kept at {} until it finishes)", sources.len(), backup.display()),
        format!("chrono: reindexando {} fuentes (el índice viejo queda en {} hasta que acabe)", sources.len(), backup.display()),
    ));
    rebuild(db, &backup, &marker, &sources, symbols, before).map_err(|e| {
        // Cualquier error a partir de aquí: la copia buena es lo que importa.
        if e.to_string().contains(&*backup.to_string_lossy()) {
            e
        } else {
            t(format!("{e} (the old index is at {})", backup.display()), format!("{e} (el índice viejo está en {})", backup.display())).into()
        }
    })
}

/// El cuerpo de `run` con el índice ya apartado en `backup`.
fn rebuild(db: &Path, backup: &Path, marker: &Path, sources: &[chrono_store::SourceRow], symbols: bool, before: Option<i64>) -> Result<()> {
    let mut failed = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        let m: Value = serde_json::from_str(&s.manifest_json).unwrap_or(Value::Null);
        let get = |k: &str| m.get(k).and_then(Value::as_str);
        let r = if let Some(pattern) = get("glob") {
            index::add_glob(db, pattern, Some(&s.kind), get("alias"), get("preset"))
        } else if i == 0 && s.kind == "git" {
            // La fuente principal: `init` además deja el repo en `meta`.
            index::init(Path::new(&s.path), symbols, Some("git"), Some(db))
        } else {
            index::add(db, Path::new(&s.path), symbols && s.kind == "git", Some(&s.kind), get("preset"))
        };
        if let Err(e) = r {
            eprintln!("chrono: {}: {e}", s.id);
            failed.push(s.id.clone());
        }
    }

    if failed.len() == sources.len() {
        // Nada se pudo reindexar: se deja todo como estaba.
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        move_db(backup, db)?;
        let _ = std::fs::remove_file(marker);
        return Err(t(
            "no source could be reindexed; the old index was restored",
            "no se pudo reindexar ninguna fuente; se restauró el índice viejo",
        )
        .into());
    }
    if !failed.is_empty() {
        // La marca se queda: el índice no tiene esas fuentes y otro `reindex`
        // las reintenta desde la copia, que es la que las tiene.
        return Err(t(
            format!(
                "reindexed, but these sources failed: {} (the old index stays at {}; run chrono reindex again to retry from it)",
                failed.join(", "),
                backup.display()
            ),
            format!(
                "reindexado, pero fallaron estas fuentes: {} (el índice viejo sigue en {}; repite chrono reindex para reintentar desde él)",
                failed.join(", "),
                backup.display()
            ),
        )
        .into());
    }
    // Que todas las fuentes se añadan no basta: un fichero truncado o rotado
    // desde la última ingesta deja menos eventos. Entonces se avisa con las
    // cifras y se conserva la copia, que es lo único que los tiene.
    let after = chrono_store::count_events_any_version(db);
    let counts = match (before, after) {
        (Some(b), Some(a)) => format!("{b} → {a}"),
        (_, Some(a)) => format!("? → {a}"),
        _ => "?".into(),
    };
    if let (Some(b), Some(a)) = (before, after) {
        if a < b {
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
            let _ = std::fs::remove_file(marker);
            return Ok(());
        }
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", backup.display()));
    }
    let _ = std::fs::remove_file(marker);
    eprintln!("{}", t(
        format!("chrono: reindex done ({} sources, events {counts}).", sources.len()),
        format!("chrono: reindexado ({} fuentes, eventos {counts}).", sources.len()),
    ));
    Ok(())
}

/// Mensaje de un error al abrir el índice: si es de otra versión de esquema,
/// qué versión tiene, cuál hace falta y el comando exacto para reconstruirlo.
pub fn explain_open_error(e: &(dyn std::error::Error + 'static), db: &Path, explicit_db: bool) -> Option<String> {
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
