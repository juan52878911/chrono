//! `chrono-source-csv` — adaptador Source para CSV con cabecera: cada fila de
//! datos se convierte en un `chrono_core::Event`. Ver
//! `docs/DESIGN-GENERAL-CORE.md §3` (adaptadores) y `chrono-source-jsonl` como
//! referencia de estilo (este crate imita su estructura: `config`, `cursor`,
//! `record`).
//!
//! Diseño y decisiones documentadas aquí porque no están 1:1 en el encargo:
//! - Formato: "un registro por línea física" con soporte de campos
//!   entrecomillados RFC4180 (`parse::parse_csv_line`). LIMITACIÓN DE v1: NO
//!   se soportan saltos de línea dentro de un campo entrecomillado — eso
//!   rompería el watermark por offset de bytes (cada línea física tiene que
//!   corresponder a un único registro para poder reanudar la lectura).
//! - A diferencia de `chrono-source-jsonl` (que resuelve el campo de cada
//!   dimensión POR LÍNEA, porque los logs JSONL reales mezclan formas), un
//!   CSV tiene una cabecera única para todo el fichero: las columnas se
//!   resuelven UNA SOLA VEZ contra esa cabecera (`config::resolve_columns`,
//!   invocado desde `cursor::CsvCursor::open`), y esa resolución es la misma
//!   tanto si se lee desde el principio como si se reanuda con un watermark
//!   a mitad de fichero (`cursor::peek_header` siempre lee el principio del
//!   fichero, nunca la línea de arranque del watermark) — así `manifest()`
//!   es estable entre un `init` y un `sync`.
//! - `detect`: es deliberadamente conservador para no robarle ficheros a
//!   otros adaptadores (una línea JSON tiene comas y no debe puntuar aquí):
//!   solo puntúa si la extensión es `.csv`/`.tsv` Y la primera línea no
//!   vacía tiene al menos 2 campos con el delimitador esperado por esa
//!   extensión (coma para `.csv`, tab para `.tsv`). Cualquier otra cosa
//!   (fichero no regular, extensión distinta, una sola columna) da 0.

mod config;
mod cursor;
mod parse;
mod record;
mod simhash;
mod timeutil;

pub use config::{CsvOverrides, ResolvedColumns};
pub use cursor::CsvCursor;

use chrono_core::{CoreError, Result as CoreResult, Source, SourceConfig, Watermark};
use std::fs;
use std::path::Path;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Adaptador `Source` de ficheros CSV.
pub struct CsvSource;

impl CsvSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CsvSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for CsvSource {
    fn kind(&self) -> &str {
        "csv"
    }

    fn detect(&self, path: &Path) -> i32 {
        let is_file = fs::metadata(path).map(|m| m.is_file()).unwrap_or(false);
        if !is_file {
            return 0;
        }
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
        // Delimitador esperado según la extensión: no adivinamos por
        // contenido (una línea JSON o texto libre también tiene comas), solo
        // confirmamos la forma cuando el nombre de fichero ya sugiere CSV/TSV.
        let delimiter = match ext.as_deref() {
            Some("csv") => ',',
            Some("tsv") => '\t',
            _ => return 0,
        };
        let Some(first_line) = cursor::peek_first_nonempty_line(path) else {
            return 0;
        };
        let fields = parse::parse_csv_line(&first_line, delimiter);
        if fields.len() >= 2 {
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
        let file_len = meta.len();

        let start_offset = match &watermark {
            None => 0u64,
            Some(wm) => {
                let (off, prefix_hex) = cursor::parse_watermark(&wm.value).ok_or_else(|| CoreError::Diverged(wm.value.clone()))?;
                if file_len < off {
                    // Fichero más corto que el watermark: rotado o truncado.
                    return Err(CoreError::Diverged(wm.value.clone()));
                }
                // NUEVO: si el watermark trae hash de prefijo (formato
                // `off:<n>|p:<hex>`), comprobar que los primeros bytes del
                // fichero no cambiaron. Un watermark viejo (sin `|p:`) no
                // trae `prefix_hex` -> se salta esta comprobación
                // (retrocompatible, solo queda la de longitud de arriba).
                if let Some(expected) = &prefix_hex {
                    // Se hashea la MISMA región que produjo el watermark:
                    // `min(4096, off)`, no `min(4096, file_len)` — si no, un
                    // append a un fichero < 4 KB cambiaría la región y daría
                    // divergencia falsa en cada sync.
                    let actual = cursor::hash_prefix(path, off)?;
                    if &actual != expected {
                        // Mismo o mayor tamaño pero prefijo distinto: el
                        // fichero se reescribió desde el principio (p.ej.
                        // rotación in-place). Esto es lo que el chequeo de
                        // longitud, solo, no detecta.
                        return Err(CoreError::Diverged(wm.value.clone()));
                    }
                }
                off
            }
        };

        let cur = CsvCursor::open(path, start_offset, cfg)?;
        Ok(Box::new(cur))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Fichero CSV temporal con limpieza automática al salir de scope.
    struct TempCsv {
        path: std::path::PathBuf,
    }

    impl TempCsv {
        fn write(contents: &str) -> Self {
            Self::write_named(contents, "csv")
        }

        fn write_named(contents: &str, ext: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "chrono-source-csv-test-{}-{}-{n}.{ext}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(contents.as_bytes()).unwrap();
            Self { path }
        }

        fn append(&self, contents: &str) {
            use std::io::Write as _;
            let mut f = fs::OpenOptions::new().append(true).open(&self.path).unwrap();
            f.write_all(contents.as_bytes()).unwrap();
        }

        fn truncate_to(&self, len: u64) {
            let f = fs::OpenOptions::new().write(true).open(&self.path).unwrap();
            f.set_len(len).unwrap();
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempCsv {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    const SAMPLE: &str = concat!(
        "ts,msg,level,service\n",
        "1704067200,arranque del servicio,info,api\n",
        "1704067201,peticion recibida,debug,api\n",
        "2024-01-01T00:00:05Z,respuesta enviada,info,api\n",
    );

    fn open_source(path: &Path) -> Box<dyn chrono_core::Cursor> {
        let src = CsvSource::new();
        let cfg = SourceConfig::default();
        src.open(path, None, &cfg).unwrap()
    }

    #[test]
    fn detect_acepta_csv_y_rechaza_directorio_jsonl_y_txt() {
        let f = TempCsv::write(SAMPLE);
        let src = CsvSource::new();
        assert_eq!(src.detect(f.path()), 60);

        let dir = std::env::temp_dir();
        assert_eq!(src.detect(&dir), 0);

        let jsonl = TempCsv::write_named(r#"{"ts": 1, "msg": "hola"}"#, "jsonl");
        assert_eq!(src.detect(jsonl.path()), 0);

        // Extensión .txt: aunque el contenido "parezca" CSV, no puntúa (no
        // robamos ficheros de otros adaptadores por contenido, solo por
        // extensión + forma).
        let txt = TempCsv::write_named(SAMPLE, "txt");
        assert_eq!(src.detect(txt.path()), 0);

        // Una sola columna (sin delimitador en la primera línea): no puntúa.
        let one_col = TempCsv::write("solo_una_columna\nvalor\n");
        assert_eq!(src.detect(one_col.path()), 0);
    }

    #[test]
    fn detect_tsv_usa_tab_como_delimitador() {
        let tsv = TempCsv::write_named("ts\tmsg\n1704067200\thola\n", "tsv");
        let src = CsvSource::new();
        assert_eq!(src.detect(tsv.path()), 60);
    }

    #[test]
    fn cursor_produce_un_evento_por_fila_respetando_cabecera() {
        let f = TempCsv::write(SAMPLE);
        let mut cur = open_source(f.path());
        let mut count = 0;
        let mut titles = Vec::new();
        while let Some(ev) = cur.next().unwrap() {
            titles.push(ev.title);
            count += 1;
        }
        assert_eq!(count, 3);
        assert_eq!(titles, vec!["arranque del servicio", "peticion recibida", "respuesta enviada"]);
    }

    #[test]
    fn campo_entrecomillado_con_coma_no_parte_la_fila() {
        let contents = concat!("ts,msg,service\n", r#"1704067200,"hola, mundo",api"#, "\n",);
        let f = TempCsv::write(contents);
        let mut cur = open_source(f.path());
        let ev = cur.next().unwrap().unwrap();
        assert_eq!(ev.title, "hola, mundo");
        assert_eq!(ev.touches[0].entity, "api");
        assert!(cur.next().unwrap().is_none());
    }

    #[test]
    fn tsv_via_delimiter_option() {
        let contents = "ts\tmsg\tservice\n1704067200\thola\tapi\n";
        let f = TempCsv::write_named(contents, "tsv");
        let src = CsvSource::new();
        let mut opts = BTreeMap::new();
        opts.insert("delimiter".to_string(), "\\t".to_string());
        let cfg = SourceConfig { options: opts };
        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        let ev = cur.next().unwrap().unwrap();
        assert_eq!(ev.title, "hola");
        assert_eq!(ev.touches[0].entity, "api");
    }

    #[test]
    fn override_de_columnas_via_options() {
        let contents = "customTime,customMsg\n1704067200,hola\n";
        let f = TempCsv::write(contents);
        let src = CsvSource::new();
        let mut opts = BTreeMap::new();
        opts.insert("time_column".to_string(), "customTime".to_string());
        opts.insert("message_column".to_string(), "customMsg".to_string());
        let cfg = SourceConfig { options: opts };
        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        let ev = cur.next().unwrap().unwrap();
        assert_eq!(ev.at_epoch, 1_704_067_200);
        assert_eq!(ev.title, "hola");
    }

    #[test]
    fn id_explicito_vs_hash_determinista() {
        let contents = concat!("id,msg\n", "evt-42,con id\n", "," , "sin id\n",);
        let f = TempCsv::write(contents);

        let mut cur = open_source(f.path());
        let e1 = cur.next().unwrap().unwrap();
        assert_eq!(e1.id, "evt-42");

        // Misma fila sin id explícito, misma posición en el fichero (misma
        // ejecución determinista) -> mismo id calculado por hash.
        let mut cur_a = open_source(f.path());
        let mut cur_b = open_source(f.path());
        loop {
            let (a, b) = (cur_a.next().unwrap(), cur_b.next().unwrap());
            match (a, b) {
                (Some(a), Some(b)) => assert_eq!(a.id, b.id),
                (None, None) => break,
                _ => panic!("cursores desincronizados"),
            }
        }
    }

    #[test]
    fn at_epoch_para_epoch_e_iso() {
        let f = TempCsv::write(SAMPLE);
        let mut cur = open_source(f.path());
        let e1 = cur.next().unwrap().unwrap();
        assert_eq!(e1.at_epoch, 1_704_067_200);
        assert_eq!(e1.at, "2024-01-01T00:00:00Z");

        let _e2 = cur.next().unwrap().unwrap();
        let e3 = cur.next().unwrap().unwrap(); // ISO con Z
        assert_eq!(e3.at_epoch, 1_704_067_205);
    }

    #[test]
    fn watermark_incremental_y_divergencia_por_truncado() {
        let f = TempCsv::write(SAMPLE);
        let src = CsvSource::new();
        let cfg = SourceConfig::default();

        // Consume todo, guarda el watermark final.
        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        while cur.next().unwrap().is_some() {}
        let wm = cur.watermark();
        assert_eq!(wm.kind, "csv");
        assert!(wm.value.starts_with("off:"));

        // Reabrir con ese watermark: no quedan filas por leer.
        let mut cur2 = src.open(f.path(), Some(wm.clone()), &cfg).unwrap();
        assert!(cur2.next().unwrap().is_none());

        // Añadir una fila nueva y reabrir con el watermark: solo la fila
        // nueva, y la cabecera no se vuelve a tratar como fila de datos.
        f.append("1704067300,fila nueva,warn,cache\n");
        let mut cur3 = src.open(f.path(), Some(wm.clone()), &cfg).unwrap();
        let ev = cur3.next().unwrap().unwrap();
        assert_eq!(ev.title, "fila nueva");
        assert!(cur3.next().unwrap().is_none());

        // Un watermark con offset mayor que el fichero actual (truncado) -> Diverged.
        f.truncate_to(5);
        match src.open(f.path(), Some(wm), &cfg) {
            Err(CoreError::Diverged(_)) => {}
            other => panic!("esperaba CoreError::Diverged, obtuve otra cosa (ok={})", other.is_ok()),
        }
    }

    #[test]
    fn manifest_reporta_columnas_resueltas_y_es_estable_entre_init_y_sync() {
        let f = TempCsv::write(SAMPLE);
        let src = CsvSource::new();
        let cfg = SourceConfig::default();

        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        while cur.next().unwrap().is_some() {}
        let m = cur.manifest();
        assert_eq!(m["format"], "csv");
        assert_eq!(m["delimiter"], ",");
        assert_eq!(m["time_column"], "ts");
        assert_eq!(m["message_column"], "msg");
        assert_eq!(m["level_column"], "level");
        assert_eq!(m["entity_column"], "service");

        let wm = cur.watermark();
        let cur2 = src.open(f.path(), Some(wm), &cfg).unwrap();
        let m2 = cur2.manifest();
        assert_eq!(m, m2, "manifest debe ser estable entre init (offset 0) y sync (offset>0)");
    }
}
