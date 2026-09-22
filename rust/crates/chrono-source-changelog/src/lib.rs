//! `chrono-source-changelog` — adaptador Source para ficheros CHANGELOG estilo
//! "Keep a Changelog": cada sección de versión (`## [1.2.3] - 2024-06-01`) se
//! convierte en un `chrono_core::Event` con `kind="release"`. Ver
//! `docs/DESIGN-GENERAL-CORE.md §3` y `chrono-source-jsonl` como referencia.
//!
//! Decisiones documentadas aquí porque no están 1:1 en el encargo:
//! - **Watermark por hash de contenido, no por offset.** Un changelog se
//!   edita anteponiendo releases arriba del fichero (lo nuevo va primero);
//!   un offset de bytes asumiría que lo nuevo se añade al final, como un
//!   log, y aquí es justo lo contrario. Por eso el watermark es
//!   `sha:<fnv1a64 del contenido completo>`: mismo hash → nada nuevo (0
//!   eventos); hash distinto → `CoreError::Diverged` y el CLI reingiere la
//!   fuente entera (barato y correcto: un changelog es pequeño). Ver
//!   `cursor::ChangelogCursor`.
//! - **`detect` exige nombre Y contenido.** Solo el nombre de fichero
//!   conteniendo "changelog" (sin importar mayúsculas) no basta: un
//!   `docs/CHANGELOG-ideas.md` sin secciones de versión no es un changelog
//!   ingeríble. Y solo el contenido tampoco basta: no queremos robarle
//!   ficheros `.md` genéricos a otros adaptadores por casualidad de que
//!   tengan una línea `## [algo]`. Se exige AMBAS condiciones para dar
//!   score 60; si falta cualquiera, 0.
//! - **`touches` = sub-secciones, no líneas.** Cada `### Added/Fixed/...`
//!   presente en la sección es un `Touch` con `entity_type="change-type"` y
//!   `weight` = nº de viñetas; así `top entity` sobre un changelog dice qué
//!   tipo de cambio domina, igual que en git dice qué fichero.

mod cursor;
mod parser;
mod record;
mod simhash;
mod timeutil;

pub use cursor::ChangelogCursor;

use chrono_core::{CoreError, Result as CoreResult, Source, SourceConfig, Watermark};
use std::fs;
use std::path::Path;

/// `true` si el nombre del fichero (no la ruta completa) contiene
/// "changelog", sin importar mayúsculas/minúsculas: "CHANGELOG.md",
/// "CHANGELOG", "changelog.txt", "Changelog-v2.md"...
fn has_changelog_name(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).map(|n| n.to_lowercase().contains("changelog")).unwrap_or(false)
}

/// Adaptador `Source` de ficheros CHANGELOG (Keep a Changelog).
pub struct ChangelogSource;

impl ChangelogSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ChangelogSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for ChangelogSource {
    fn kind(&self) -> &str {
        "changelog"
    }

    fn detect(&self, path: &Path) -> i32 {
        let is_file = fs::metadata(path).map(|m| m.is_file()).unwrap_or(false);
        if !is_file || !has_changelog_name(path) {
            return 0;
        }
        let Ok(content) = fs::read_to_string(path) else {
            return 0;
        };
        if parser::has_version_heading(&content) {
            60
        } else {
            0
        }
    }

    fn open(&self, path: &Path, watermark: Option<Watermark>, cfg: &SourceConfig) -> CoreResult<Box<dyn chrono_core::Cursor>> {
        let meta = fs::metadata(path).map_err(|e| CoreError::Other(format!("leyendo metadata de {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(CoreError::Other(format!("{} no es un fichero regular", path.display())));
        }
        let cur = ChangelogCursor::open(path, watermark, cfg)?;
        Ok(Box::new(cur))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_core::Cursor;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Fichero temporal con limpieza automática al salir de scope (mismo
    /// patrón `TempJsonl` que `chrono-source-jsonl`), con extensión y
    /// nombre configurables para probar `detect`.
    struct TempFile {
        path: std::path::PathBuf,
    }

    impl TempFile {
        fn write(contents: &str) -> Self {
            Self::write_named(contents, "CHANGELOG.md")
        }

        fn write_named(contents: &str, filename: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "chrono-source-changelog-test-{}-{}-{n}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(filename);
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(contents.as_bytes()).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn overwrite(&self, contents: &str) {
            fs::write(&self.path, contents).unwrap();
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            if let Some(dir) = self.path.parent() {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

    const SAMPLE: &str = concat!(
        "# Changelog\n",
        "\n",
        "Todas las notas de cambios de este proyecto.\n",
        "\n",
        "## [Unreleased]\n",
        "\n",
        "### Added\n",
        "- soporte para exportar a csv\n",
        "\n",
        "## [1.2.3] - 2024-06-01\n",
        "\n",
        "### Added\n",
        "- nueva funcionalidad de busqueda\n",
        "- soporte para filtros avanzados\n",
        "\n",
        "### Fixed\n",
        "- corrige fuga de memoria en el indexador\n",
        "\n",
        "### Security\n",
        "- actualiza dependencia vulnerable\n",
        "\n",
        "## [1.0.0] - 2024-01-01\n",
        "\n",
        "### Added\n",
        "- version inicial\n",
    );

    fn open_source(path: &Path) -> Box<dyn Cursor> {
        let src = ChangelogSource::new();
        let cfg = SourceConfig::default();
        src.open(path, None, &cfg).unwrap()
    }

    #[test]
    fn detect_acepta_changelog_md_con_encabezado() {
        let f = TempFile::write(SAMPLE);
        let src = ChangelogSource::new();
        assert_eq!(src.detect(f.path()), 60);
    }

    #[test]
    fn detect_rechaza_md_sin_encabezado_de_version() {
        let f = TempFile::write_named("# Changelog\n\nsolo texto, sin secciones.\n", "CHANGELOG.md");
        let src = ChangelogSource::new();
        assert_eq!(src.detect(f.path()), 0);
    }

    #[test]
    fn detect_rechaza_ficheros_sin_changelog_en_el_nombre() {
        let f = TempFile::write_named(SAMPLE, "HISTORY.md");
        let src = ChangelogSource::new();
        assert_eq!(src.detect(f.path()), 0);
    }

    #[test]
    fn detect_rechaza_directorios() {
        let f = TempFile::write(SAMPLE);
        let src = ChangelogSource::new();
        assert_eq!(src.detect(f.path().parent().unwrap()), 0);
    }

    #[test]
    fn detect_ignora_mayusculas_en_el_nombre() {
        let f = TempFile::write_named(SAMPLE, "changelog.txt");
        let src = ChangelogSource::new();
        assert_eq!(src.detect(f.path()), 60);
    }

    #[test]
    fn una_release_por_evento_con_touches_por_categoria() {
        let f = TempFile::write(SAMPLE);
        let mut cur = open_source(f.path());

        let e0 = cur.next().unwrap().unwrap();
        assert_eq!(e0.kind, "release");
        assert_eq!(e0.id, "Unreleased");
        assert_eq!(e0.touches.len(), 1);
        assert_eq!(e0.touches[0].entity, "Added");
        assert_eq!(e0.touches[0].entity_type, "change-type");
        assert_eq!(e0.touches[0].weight, 1);

        let e1 = cur.next().unwrap().unwrap();
        assert_eq!(e1.id, "1.2.3");
        assert_eq!(e1.touches.len(), 3);
        assert_eq!(e1.touches[0].entity, "Added");
        assert_eq!(e1.touches[0].weight, 2);
        assert_eq!(e1.touches[1].entity, "Fixed");
        assert_eq!(e1.touches[2].entity, "Security");

        let e2 = cur.next().unwrap().unwrap();
        assert_eq!(e2.id, "1.0.0");

        assert!(cur.next().unwrap().is_none());
    }

    #[test]
    fn fecha_del_encabezado_produce_at_epoch() {
        let f = TempFile::write(SAMPLE);
        let mut cur = open_source(f.path());
        cur.next().unwrap(); // Unreleased
        let e1 = cur.next().unwrap().unwrap();
        assert_eq!(e1.at, "2024-06-01T00:00:00Z");
        assert_eq!(e1.at_epoch, 1_717_200_000);
    }

    #[test]
    fn unreleased_sin_fecha_entra_igual_con_epoch_cero() {
        let f = TempFile::write(SAMPLE);
        let mut cur = open_source(f.path());
        let e0 = cur.next().unwrap().unwrap();
        assert_eq!(e0.at_epoch, 0);
        assert_eq!(e0.at, "");
    }

    #[test]
    fn watermark_mismo_contenido_da_cero_eventos() {
        let f = TempFile::write(SAMPLE);
        let src = ChangelogSource::new();
        let cfg = SourceConfig::default();

        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        while cur.next().unwrap().is_some() {}
        let wm = cur.watermark();
        assert_eq!(wm.kind, "changelog");
        assert!(wm.value.starts_with("sha:"));

        let mut cur2 = src.open(f.path(), Some(wm.clone()), &cfg).unwrap();
        assert!(cur2.next().unwrap().is_none());
        assert_eq!(cur2.watermark(), wm);
    }

    #[test]
    fn watermark_contenido_cambiado_diverge() {
        let f = TempFile::write(SAMPLE);
        let src = ChangelogSource::new();
        let cfg = SourceConfig::default();

        let cur = src.open(f.path(), None, &cfg).unwrap();
        let wm = cur.watermark();
        drop(cur);

        // Se antepone una release nueva arriba del fichero (patrón típico
        // de edición de un CHANGELOG): el contenido cambia por completo
        // respecto al hash anterior aunque las releases viejas sigan ahí.
        f.overwrite(&format!("## [2.0.0] - 2024-09-01\n\n### Added\n- x\n\n{SAMPLE}"));

        match src.open(f.path(), Some(wm), &cfg) {
            Err(CoreError::Diverged(_)) => {}
            other => panic!("esperaba Diverged, obtuve ok={}", other.is_ok()),
        }
    }

    #[test]
    fn manifest_reporta_formato_y_numero_de_releases() {
        let f = TempFile::write(SAMPLE);
        let mut cur = open_source(f.path());
        while cur.next().unwrap().is_some() {}
        let m = cur.manifest();
        assert_eq!(m["format"], "changelog");
        assert_eq!(m["releases"], "3");
    }
}
