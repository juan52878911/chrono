//! `chrono-source-jsonl` — adaptador Source para logs JSONL/NDJSON: cada
//! línea de un fichero (un objeto JSON por línea) se convierte en un
//! `chrono_core::Event`. Primer adaptador de LOGS del port a Rust; ver
//! `docs/DESIGN-GENERAL-CORE.md §3` (adaptadores) y §2 (modelo Event).
//!
//! Diseño y decisiones documentadas aquí porque no están 1:1 en el encargo:
//! - La configuración de campos (`time_field`, `message_field`, ...) se
//!   resuelve UNA VEZ a partir de la primera línea del fichero (no de la
//!   línea de arranque de un watermark incremental), para que `manifest()`
//!   sea estable entre un `init` y un `sync`. Ver `config::resolve_config`.
//! - Una línea que no parsea como objeto JSON se descarta silenciosamente
//!   (no aborta la ingesta), igual que `chrono-source-git` descarta
//!   registros corruptos de `git log`.
//! - `detect`: si la primera línea no vacía es un objeto JSON, ya se
//!   considera "parece JSONL" (score 40); si además la extensión es una de
//!   las reconocidas y esa línea trae un campo de tiempo reconocido, sube a
//!   60. Cualquier otra cosa (no es fichero regular, primera línea no es
//!   JSON, o no es un objeto) da 0.

mod config;
mod cursor;
mod record;
mod simhash;
mod timeutil;

pub use config::JsonlConfig;
pub use cursor::JsonlCursor;

use chrono_core::{CoreError, Result as CoreResult, Source, SourceConfig, Watermark};
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::path::Path;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Extensiones de fichero que este adaptador reconoce de entrada.
const KNOWN_EXTENSIONS: &[&str] = &["jsonl", "ndjson", "log", "json"];

fn has_known_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| KNOWN_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Adaptador `Source` de logs JSONL/NDJSON.
pub struct JsonlSource;

impl JsonlSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JsonlSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for JsonlSource {
    fn kind(&self) -> &str {
        "jsonl"
    }

    fn detect(&self, path: &Path) -> i32 {
        let is_file = fs::metadata(path).map(|m| m.is_file()).unwrap_or(false);
        if !is_file {
            return 0;
        }
        let Some(first_line) = cursor::peek_first_nonempty_line(path) else {
            return 0;
        };
        let Ok(value) = serde_json::from_str::<Value>(&first_line) else {
            return 0;
        };
        let Some(obj) = value.as_object() else {
            return 0;
        };
        let has_time = config::TIME_FIELDS.iter().any(|f| obj.contains_key(*f));
        if has_known_extension(path) && has_time {
            60
        } else {
            40
        }
    }

    fn open(&self, path: &Path, watermark: Option<Watermark>, cfg: &SourceConfig) -> CoreResult<Box<dyn chrono_core::Cursor>> {
        let meta = fs::metadata(path).map_err(|e| CoreError::Other(format!("leyendo metadata de {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(CoreError::Other(format!("{} no es un fichero regular", path.display())));
        }
        let file_len = meta.len();

        let start_offset = match watermark {
            None => 0u64,
            Some(wm) => {
                let off_str = wm.value.strip_prefix("off:").ok_or_else(|| CoreError::Diverged(wm.value.clone()))?;
                let off: u64 = off_str.parse().map_err(|_| CoreError::Diverged(wm.value.clone()))?;
                if file_len < off {
                    // Fichero más corto que el watermark: rotado o truncado.
                    return Err(CoreError::Diverged(wm.value.clone()));
                }
                off
            }
        };

        let cur = JsonlCursor::open(path, start_offset, cfg)?;
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

    /// Fichero JSONL temporal con limpieza automática al salir de scope.
    struct TempJsonl {
        path: std::path::PathBuf,
    }

    impl TempJsonl {
        fn write(contents: &str) -> Self {
            Self::write_named(contents, "jsonl")
        }

        fn write_named(contents: &str, ext: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "chrono-source-jsonl-test-{}-{}-{n}.{ext}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(contents.as_bytes()).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempJsonl {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    /// ~6 líneas variadas: epoch-segundos, epoch-milis, ISO con Z y con
    /// offset, `msg`/`level`/`service` variados, una línea con `id` explícito.
    const SAMPLE: &str = concat!(
        r#"{"ts": 1704067200, "msg": "arranque del servicio", "level": "info", "service": "api"}"#, "\n",
        r#"{"ts": 1704067201000, "msg": "peticion recibida", "level": "debug", "service": "api"}"#, "\n",
        r#"{"@timestamp": "2024-01-01T00:00:05Z", "msg": "respuesta enviada", "level": "info", "service": "api"}"#, "\n",
        r#"{"@timestamp": "2024-01-01T02:00:10+02:00", "msg": "cache miss", "level": "warn", "service": "cache"}"#, "\n",
        r#"{"id": "evt-42", "ts": 1704067210, "msg": "id explicito", "level": "error", "service": "db"}"#, "\n",
        r#"{"ts": 1704067215, "msg": "sin service", "level": "info"}"#, "\n",
    );

    fn open_source(path: &Path) -> Box<dyn chrono_core::Cursor> {
        let src = JsonlSource::new();
        let cfg = SourceConfig::default();
        src.open(path, None, &cfg).unwrap()
    }

    #[test]
    fn detect_reconoce_jsonl_y_rechaza_directorio() {
        let f = TempJsonl::write(SAMPLE);
        let src = JsonlSource::new();
        assert!(src.detect(f.path()) > 0);

        let dir = std::env::temp_dir();
        assert_eq!(src.detect(&dir), 0);
    }

    #[test]
    fn detect_da_60_con_extension_y_tiempo_40_sin_extension() {
        let src = JsonlSource::new();

        let with_ext = TempJsonl::write(SAMPLE);
        assert_eq!(src.detect(with_ext.path()), 60);

        // Misma primera línea (con tiempo reconocido) pero extensión no
        // reconocida: sigue "pareciendo JSONL", pero con menos confianza.
        let without_ext = TempJsonl::write_named(SAMPLE, "txt");
        assert_eq!(src.detect(without_ext.path()), 40);
    }

    #[test]
    fn cursor_produce_un_evento_por_linea() {
        let f = TempJsonl::write(SAMPLE);
        let mut cur = open_source(f.path());
        let mut count = 0;
        while cur.next().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 6);
    }

    #[test]
    fn at_epoch_correcto_para_segundos_milis_e_iso_con_offset() {
        let f = TempJsonl::write(SAMPLE);
        let mut cur = open_source(f.path());

        let e1 = cur.next().unwrap().unwrap(); // epoch-segundos
        assert_eq!(e1.at_epoch, 1_704_067_200);
        assert_eq!(e1.at, "2024-01-01T00:00:00Z");

        let e2 = cur.next().unwrap().unwrap(); // epoch-milisegundos
        assert_eq!(e2.at_epoch, 1_704_067_201);

        let e3 = cur.next().unwrap().unwrap(); // ISO con Z
        assert_eq!(e3.at_epoch, 1_704_067_205);

        let e4 = cur.next().unwrap().unwrap(); // ISO con offset +02:00 -> UTC
        assert_eq!(e4.at_epoch, 1_704_067_210);
    }

    #[test]
    fn entity_level_y_title_desde_service_level_msg() {
        let f = TempJsonl::write(SAMPLE);
        let mut cur = open_source(f.path());
        let e1 = cur.next().unwrap().unwrap();
        assert_eq!(e1.touches.len(), 1);
        assert_eq!(e1.touches[0].entity, "api");
        assert_eq!(e1.touches[0].entity_type, "log-source");
        assert_eq!(e1.level, "INFO");
        assert_eq!(e1.title, "arranque del servicio");
    }

    #[test]
    fn id_explicito_y_id_hash_deterministas() {
        let f = TempJsonl::write(SAMPLE);

        // El id explícito ("evt-42") se respeta tal cual.
        let mut cur = open_source(f.path());
        let mut last = None;
        while let Some(ev) = cur.next().unwrap() {
            if ev.title == "id explicito" {
                last = Some(ev.id);
            }
        }
        assert_eq!(last.as_deref(), Some("evt-42"));

        // Misma línea sin id explícito, misma posición en el fichero (misma
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
    fn override_de_time_field_via_options() {
        let contents = concat!(r#"{"customTime": 1704067200, "msg": "hola"}"#, "\n");
        let f = TempJsonl::write(contents);

        let src = JsonlSource::new();
        let mut opts = BTreeMap::new();
        opts.insert("time_field".to_string(), "customTime".to_string());
        let cfg = SourceConfig { options: opts };
        let mut cur = src.open(f.path(), None, &cfg).unwrap();

        let ev = cur.next().unwrap().unwrap();
        assert_eq!(ev.at_epoch, 1_704_067_200);
    }

    #[test]
    fn watermark_incremental_y_divergencia_por_truncado() {
        let f = TempJsonl::write(SAMPLE);
        let src = JsonlSource::new();
        let cfg = SourceConfig::default();

        // Consume todo, guarda el watermark final.
        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        while cur.next().unwrap().is_some() {}
        let wm = cur.watermark();
        assert_eq!(wm.kind, "jsonl");
        assert!(wm.value.starts_with("off:"));

        // Reabrir con ese watermark: no quedan eventos por leer.
        let mut cur2 = src.open(f.path(), Some(wm.clone()), &cfg).unwrap();
        assert!(cur2.next().unwrap().is_none());

        // Un watermark con offset mayor que el fichero actual (rotado/truncado) -> Diverged.
        let bad_wm = Watermark { kind: "jsonl".to_string(), value: "off:999999".to_string() };
        match src.open(f.path(), Some(bad_wm), &cfg) {
            Err(CoreError::Diverged(_)) => {}
            other => panic!("esperaba CoreError::Diverged, obtuve otra cosa (ok={})", other.is_ok()),
        }
    }

    #[test]
    fn manifest_reporta_los_campos_resueltos() {
        let f = TempJsonl::write(SAMPLE);
        let mut cur = open_source(f.path());
        while cur.next().unwrap().is_some() {}
        let m = cur.manifest();
        assert_eq!(m["format"], "jsonl");
        assert_eq!(m["time_field"], "ts");
        assert_eq!(m["message_field"], "msg");
        assert_eq!(m["entity_field"], "service");
    }
}
