//! `chrono-source-textlog` — adaptador Source para logs de texto plano por
//! PRESETS (syslog, nginx). Cada línea se parsea con el preset activo a un
//! `chrono_core::Event`. Sin `regex` (parsers a mano, para no hinchar el
//! binario). Ver `docs/DESIGN-GENERAL-CORE.md §3` y `chrono-source-jsonl`
//! como referencia de estilo (cursor de streaming, watermark por offset de
//! bytes, manifest).
//!
//! Diseño y decisiones documentadas aquí porque no están 1:1 en el encargo:
//! - **`detect` y `SourceConfig`**: el trait `chrono_core::Source::detect`
//!   tiene la firma `fn detect(&self, path: &Path) -> i32` — NO recibe
//!   `SourceConfig` (solo `open` lo recibe). El encargo pide que `detect` dé
//!   60 cuando `options["preset"]` está fijado y casa, y 40 en autodetección;
//!   eso es imposible de leer desde el `detect` del trait sin tocar
//!   `chrono-core` (fuera de las fronteras de este encargo). Se resuelve así:
//!   el algoritmo COMPLETO vive en el método propio `detect_with_config`
//!   (más abajo), y el `detect` del trait delega en él con
//!   `SourceConfig::default()` — que al no traer `options["preset"]` cae
//!   siempre en la rama de autodetección (40 si algún preset casa, 0 si no).
//!   Es lo único observable hoy vía `Registry::pick`; los tests ejercitan el
//!   algoritmo completo (con y sin preset explícito) llamando directamente a
//!   `detect_with_config`.
//! - **Una línea que no casa el preset activo** se descarta silenciosamente
//!   (no aborta la ingesta), igual que `chrono-source-jsonl` descarta líneas
//!   que no son un objeto JSON.
//! - **`id`** siempre es FNV-1a 64 de (línea cruda + offset de inicio) en
//!   hex: a diferencia de jsonl, ningún preset de texto trae un campo de id
//!   explícito reconocible de forma genérica.
//! - **Año de RFC3164** (syslog sin año en la línea): `options["year"]` si es
//!   un entero válido, o 1970 si no se da — determinista a propósito, nunca
//!   el año del reloj del sistema (ver `preset::fallback_year_from`).

mod cursor;
mod nginx;
mod preset;
mod record;
mod simhash;
mod syslog;
// `parse_time_str` (parseo genérico epoch/RFC3339) queda sin usar: los dos
// presets v1 tienen gramáticas de tiempo propias más específicas (nginx trae
// fecha+zona en formato fijo; syslog usa `parse_rfc3339` directamente para
// RFC5424 y construye el epoch a mano para RFC3164). Se mantiene en el
// módulo (compartido con otros adaptadores de texto) para no reescribirlo.
#[allow(dead_code)]
mod timeutil;

pub use cursor::TextlogCursor;
pub use preset::Preset;

use chrono_core::{CoreError, Result as CoreResult, Source, SourceConfig, Watermark};
use std::fs;
use std::path::Path;

/// Adaptador `Source` de logs de texto por presets (syslog, nginx).
pub struct TextlogSource;

impl TextlogSource {
    pub fn new() -> Self {
        Self
    }

    /// Algoritmo completo de detección, consciente de `options["preset"]`
    /// (ver nota de módulo sobre por qué el `detect` del trait no puede
    /// serlo). Fichero no regular, o sin ninguna línea no vacía -> 0.
    ///
    /// - Con `options["preset"]` fijado a un preset válido: 60 si la primera
    ///   línea no vacía casa ESE preset, 0 si no (incluye nombre de preset
    ///   desconocido).
    /// - Sin preset en `options`: prueba ambos (orden estable: syslog, luego
    ///   nginx); 40 si alguno casa, 0 si ninguno.
    pub fn detect_with_config(&self, path: &Path, cfg: &SourceConfig) -> i32 {
        let is_file = fs::metadata(path).map(|m| m.is_file()).unwrap_or(false);
        if !is_file {
            return 0;
        }
        let Some(first_line) = cursor::peek_first_nonempty_line(path) else {
            return 0;
        };
        let fallback_year = preset::fallback_year_from(cfg);

        match cfg.options.get("preset") {
            Some(p) => match preset::Preset::parse_name(p) {
                Some(preset) if preset::parse_line(preset, &first_line, fallback_year).is_some() => 60,
                _ => 0,
            },
            None => match preset::autodetect_preset(&first_line, fallback_year) {
                Some(_) => 40,
                None => 0,
            },
        }
    }
}

impl Default for TextlogSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for TextlogSource {
    fn kind(&self) -> &str {
        "textlog"
    }

    fn detect(&self, path: &Path) -> i32 {
        // Ver nota de módulo: el trait no recibe `SourceConfig`, así que esto
        // siempre cae en la rama "sin preset" de `detect_with_config`.
        self.detect_with_config(path, &SourceConfig::default())
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

        let fallback_year = preset::fallback_year_from(cfg);
        let chosen_preset = match cfg.options.get("preset") {
            Some(p) => preset::Preset::parse_name(p)
                .ok_or_else(|| CoreError::Other(format!("chrono-source-textlog: preset desconocido \"{p}\" (usa \"syslog\" o \"nginx\")")))?,
            None => {
                let first_line = cursor::peek_first_nonempty_line(path).unwrap_or_default();
                preset::autodetect_preset(&first_line, fallback_year).ok_or_else(|| {
                    CoreError::Unrecognized(format!(
                        "{}: ninguna línea inicial casa un preset de textlog conocido (syslog/nginx); fija options[\"preset\"] para forzarlo",
                        path.display()
                    ))
                })?
            }
        };

        let cur = cursor::TextlogCursor::open(path, start_offset, chosen_preset, fallback_year)?;
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

    /// Fichero de texto temporal con limpieza automática al salir de scope.
    struct TempTextlog {
        path: std::path::PathBuf,
    }

    impl TempTextlog {
        fn write(contents: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "chrono-source-textlog-test-{}-{}-{n}.log",
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

        fn append(&self, contents: &str) {
            let mut f = fs::OpenOptions::new().append(true).open(&self.path).unwrap();
            f.write_all(contents.as_bytes()).unwrap();
        }

        /// Reescribe TODO el fichero (cambia el prefijo ya consumido).
        fn overwrite(&self, contents: &str) {
            fs::write(&self.path, contents).unwrap();
        }
    }

    impl Drop for TempTextlog {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    const NGINX_LINE: &str =
        r#"127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /apache_pb.gif HTTP/1.0" 200 2326"#;
    const SYSLOG_RFC5424_LINE: &str =
        r#"<34>1 2023-10-11T22:14:15Z mymachine.example.com su - ID47 - root failed for lonvick"#;
    const SYSLOG_RFC3164_LINE: &str = "<13>Oct 11 22:14:15 host sshd[1234]: Accepted password for root";

    fn cfg_with(options: &[(&str, &str)]) -> SourceConfig {
        let mut m = BTreeMap::new();
        for (k, v) in options {
            m.insert(k.to_string(), v.to_string());
        }
        SourceConfig { options: m }
    }

    // --- detect ---------------------------------------------------------

    #[test]
    fn detect_con_preset_explicito_da_60_si_casa_y_0_si_no() {
        let f = TempTextlog::write(NGINX_LINE);
        let src = TextlogSource::new();
        assert_eq!(src.detect_with_config(f.path(), &cfg_with(&[("preset", "nginx")])), 60);
        assert_eq!(src.detect_with_config(f.path(), &cfg_with(&[("preset", "syslog")])), 0);
    }

    #[test]
    fn detect_con_preset_desconocido_da_0() {
        let f = TempTextlog::write(NGINX_LINE);
        let src = TextlogSource::new();
        assert_eq!(src.detect_with_config(f.path(), &cfg_with(&[("preset", "apache-raro")])), 0);
    }

    #[test]
    fn detect_sin_preset_autodetecta_con_40() {
        let src = TextlogSource::new();

        let f_nginx = TempTextlog::write(NGINX_LINE);
        assert_eq!(src.detect_with_config(f_nginx.path(), &SourceConfig::default()), 40);

        let f_syslog = TempTextlog::write(SYSLOG_RFC3164_LINE);
        assert_eq!(src.detect_with_config(f_syslog.path(), &SourceConfig::default()), 40);
    }

    #[test]
    fn detect_rechaza_json_csv_y_directorio() {
        let src = TextlogSource::new();

        let f_json = TempTextlog::write(r#"{"foo": "bar", "level": "info"}"#);
        assert_eq!(src.detect_with_config(f_json.path(), &SourceConfig::default()), 0);

        let f_csv = TempTextlog::write("id,nombre,valor\n1,a,10\n");
        assert_eq!(src.detect_with_config(f_csv.path(), &SourceConfig::default()), 0);

        let dir = std::env::temp_dir();
        assert_eq!(src.detect_with_config(&dir, &SourceConfig::default()), 0);
    }

    #[test]
    fn trait_detect_delega_en_autodeteccion() {
        let f = TempTextlog::write(NGINX_LINE);
        let src = TextlogSource::new();
        // `Source::detect` no recibe opciones: siempre autodetecta (ver
        // comentario de módulo). Debe dar el mismo score que la rama "sin
        // preset" de `detect_with_config`.
        assert_eq!(Source::detect(&src, f.path()), 40);
    }

    // --- nginx: mapeo, status->level, epoch ------------------------------

    #[test]
    fn nginx_mapea_endpoint_status_level_y_epoch_conocido() {
        let f = TempTextlog::write(NGINX_LINE);
        let src = TextlogSource::new();
        let cfg = cfg_with(&[("preset", "nginx")]);
        let mut cur = src.open(f.path(), None, &cfg).unwrap();

        let ev = cur.next().unwrap().unwrap();
        assert_eq!(ev.touches[0].entity, "/apache_pb.gif");
        assert_eq!(ev.touches[0].entity_type, "endpoint");
        assert_eq!(ev.level, "INFO");
        assert_eq!(ev.actor.name, "127.0.0.1");
        assert_eq!(ev.title, "GET /apache_pb.gif 200");
        // 10/Oct/2000:13:55:36 -0700, verificado independientemente.
        assert_eq!(ev.at_epoch, 971_211_336);
        assert!(cur.next().unwrap().is_none());
    }

    // --- syslog: RFC5424 con año, RFC3164 con year option ----------------

    #[test]
    fn syslog_rfc5424_trae_su_propio_ano() {
        let f = TempTextlog::write(SYSLOG_RFC5424_LINE);
        let src = TextlogSource::new();
        let cfg = cfg_with(&[("preset", "syslog")]);
        let mut cur = src.open(f.path(), None, &cfg).unwrap();

        let ev = cur.next().unwrap().unwrap();
        assert_eq!(ev.touches[0].entity, "su");
        assert_eq!(ev.actor.name, "mymachine.example.com");
        assert_eq!(ev.level, "CRIT");
        assert_eq!(ev.at_epoch, 1_697_062_455); // 2023-10-11T22:14:15Z
    }

    #[test]
    fn syslog_rfc3164_usa_year_option() {
        let f = TempTextlog::write(SYSLOG_RFC3164_LINE);
        let src = TextlogSource::new();
        let cfg = cfg_with(&[("preset", "syslog"), ("year", "2023")]);
        let mut cur = src.open(f.path(), None, &cfg).unwrap();

        let ev = cur.next().unwrap().unwrap();
        assert_eq!(ev.touches[0].entity, "sshd");
        assert_eq!(ev.attrs["pid"], "1234");
        assert_eq!(ev.at_epoch, 1_697_062_455); // Oct 11 2023 22:14:15Z
    }

    #[test]
    fn syslog_rfc3164_sin_year_option_usa_1970_determinista() {
        let f = TempTextlog::write("Jan 1 00:00:00 host tag: msg");
        let src = TextlogSource::new();
        let cfg = cfg_with(&[("preset", "syslog")]);
        let mut cur = src.open(f.path(), None, &cfg).unwrap();

        let ev = cur.next().unwrap().unwrap();
        assert_eq!(ev.at_epoch, 0);
    }

    // --- líneas basura descartadas ---------------------------------------

    #[test]
    fn lineas_que_no_casan_el_preset_se_descartan_sin_abortar() {
        let contents = format!("{NGINX_LINE}\nlinea que no es nginx\n{NGINX_LINE}\n\n");
        let f = TempTextlog::write(&contents);
        let src = TextlogSource::new();
        let cfg = cfg_with(&[("preset", "nginx")]);
        let mut cur = src.open(f.path(), None, &cfg).unwrap();

        let mut count = 0;
        while cur.next().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 2); // las 2 líneas nginx válidas; la basura y la línea en blanco no cuentan.
    }

    // --- watermark incremental + divergencia ------------------------------

    #[test]
    fn watermark_incremental_y_divergencia_por_truncado() {
        let contents = format!("{NGINX_LINE}\n{NGINX_LINE}\n");
        let f = TempTextlog::write(&contents);
        let src = TextlogSource::new();
        let cfg = cfg_with(&[("preset", "nginx")]);

        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        while cur.next().unwrap().is_some() {}
        let wm = cur.watermark();
        assert_eq!(wm.kind, "textlog");
        assert!(wm.value.starts_with("off:"));

        // Reabrir con ese watermark: no quedan eventos por leer.
        let mut cur2 = src.open(f.path(), Some(wm.clone()), &cfg).unwrap();
        assert!(cur2.next().unwrap().is_none());

        // Watermark con offset mayor que el fichero actual (rotado/truncado).
        let bad_wm = Watermark { kind: "textlog".to_string(), value: "off:999999".to_string() };
        match src.open(f.path(), Some(bad_wm), &cfg) {
            Err(CoreError::Diverged(_)) => {}
            other => panic!("esperaba CoreError::Diverged, obtuve otra cosa (ok={})", other.is_ok()),
        }
    }

    // --- manifest ----------------------------------------------------------

    #[test]
    fn manifest_reporta_formato_preset_y_year_si_aplica() {
        let f_nginx = TempTextlog::write(NGINX_LINE);
        let src = TextlogSource::new();
        let mut cur = src.open(f_nginx.path(), None, &cfg_with(&[("preset", "nginx")])).unwrap();
        while cur.next().unwrap().is_some() {}
        let m = cur.manifest();
        assert_eq!(m["format"], "textlog");
        assert_eq!(m["preset"], "nginx");
        assert!(!m.contains_key("year"));

        let f_syslog = TempTextlog::write(SYSLOG_RFC3164_LINE);
        let mut cur2 = src
            .open(f_syslog.path(), None, &cfg_with(&[("preset", "syslog"), ("year", "2024")]))
            .unwrap();
        while cur2.next().unwrap().is_some() {}
        let m2 = cur2.manifest();
        assert_eq!(m2["preset"], "syslog");
        assert_eq!(m2["year"], "2024");
    }

    // --- open sin preset ni línea que case: error claro ---------------------

    #[test]
    fn open_sin_preset_y_sin_ninguna_linea_reconocible_da_error_claro() {
        let f = TempTextlog::write("esto no es syslog ni nginx\notra linea cualquiera\n");
        let src = TextlogSource::new();
        // `Box<dyn Cursor>` no implementa `Debug`: no se puede usar
        // `unwrap_err()` directamente, se compara el `Result` a mano.
        match src.open(f.path(), None, &SourceConfig::default()) {
            Err(CoreError::Unrecognized(_)) => {}
            Ok(_) => panic!("esperaba error, obtuve Ok"),
            Err(_) => panic!("esperaba CoreError::Unrecognized"),
        }
    }

    #[test]
    fn open_con_preset_desconocido_da_error_claro() {
        let f = TempTextlog::write(NGINX_LINE);
        let src = TextlogSource::new();
        match src.open(f.path(), None, &cfg_with(&[("preset", "apache-raro")])) {
            Err(CoreError::Other(_)) => {}
            Ok(_) => panic!("esperaba error, obtuve Ok"),
            Err(_) => panic!("esperaba CoreError::Other"),
        }
    }

    // --- divergencia por hash de prefijo -------------------------------

    /// El watermark tras consumir trae la forma `off:<n>|p:<hex>`, y reabrir
    /// tras un APPEND (líneas nuevas al final) NO diverge: lee solo el delta.
    #[test]
    fn watermark_con_prefijo_y_append_no_diverge() {
        let f = TempTextlog::write(&format!("{SYSLOG_RFC5424_LINE}\n"));
        let src = TextlogSource::new();
        let cfg = cfg_with(&[("preset", "syslog")]);

        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        while cur.next().unwrap().is_some() {}
        let wm = cur.watermark();
        assert!(wm.value.contains("|p:"), "el watermark debe traer hash de prefijo: {}", wm.value);

        // Append de otra línea válida: al reabrir con el watermark, solo la nueva.
        f.append("<34>1 2023-10-11T22:20:00Z host app - - - otra cosa\n");
        let mut cur2 = src.open(f.path(), Some(wm), &cfg).unwrap();
        let ev = cur2.next().unwrap().expect("debe leer la línea nueva");
        assert!(ev.title.contains("otra cosa"));
        assert!(cur2.next().unwrap().is_none());
    }

    /// Reescribir el prefijo YA consumido manteniendo longitud ≥ off SÍ diverge
    /// (lo que el chequeo de longitud, solo, no detectaba).
    #[test]
    fn reescritura_del_prefijo_consumido_diverge() {
        let f = TempTextlog::write(&format!("{SYSLOG_RFC5424_LINE}\n"));
        let src = TextlogSource::new();
        let cfg = cfg_with(&[("preset", "syslog")]);

        let mut cur = src.open(f.path(), None, &cfg).unwrap();
        while cur.next().unwrap().is_some() {}
        let wm = cur.watermark();

        // Reescribe el fichero entero (cambia el prefijo consumido) con longitud
        // igual o mayor, así que el chequeo de longitud pasaría; el hash no.
        f.overwrite("<11>1 2023-10-11T22:14:15Z host app - - - CONTENIDO DISTINTO Y MAS LARGO QUE ANTES\n");
        match src.open(f.path(), Some(wm), &cfg) {
            Err(CoreError::Diverged(_)) => {}
            other => panic!("esperaba Diverged por prefijo cambiado (ok={})", other.is_ok()),
        }
    }
}
