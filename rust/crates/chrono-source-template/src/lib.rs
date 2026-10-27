//! `chrono-source-template` — adaptador Source de texto plano definido por una
//! PLANTILLA en `.chrono/config.json` (sin `regex`): el usuario describe el
//! formato de una línea con marcadores `<campo>` y chrono extrae los campos.
//! Permite analizar "cualquier" log estructurado sin escribir Rust.
//!
//! No se autodetecta (`detect` = 0): se elige con `chrono add <f> --as template`
//! o poniendo `"adapter": "template"` en las `source_options` de esa fuente.
//! La plantilla y el resto de opciones se leen de `SourceConfig.options`.
//!
//! Opciones:
//! - `template` (REQUERIDA): p.ej. `"<time> <level> [<service>] <message>"`.
//!   Sintaxis y reglas de casado (espacios flexibles, `<<` como `<` literal,
//!   captura no voraz…) en [`matcher`].
//! - `time_field` / `message_field` / `level_field` / `entity_field` /
//!   `actor_field` (opcionales): marcador que cumple cada rol; si no, se infiere
//!   por NOMBRE (`time|ts|timestamp`, `message|msg|text`, `level|severity|lvl`,
//!   `entity|service|source|logger`, `actor|host|user`). Cualquier otro
//!   marcador va a `attrs[nombre]` (si no está vacío).
//!
//! Una línea que no casa la plantilla se descarta (no aborta la ingesta). La
//! plantilla se resuelve al abrir y es estable entre `init` y `sync`.

mod cursor;
mod matcher;
mod record;
#[allow(dead_code)]
mod simhash;

pub use cursor::TemplateCursor;

use chrono_core::{CoreError, Result as CoreResult, Source, SourceConfig, Watermark};
use std::fs;
use std::path::Path;

/// Adaptador `Source` de texto por plantilla.
pub struct TemplateSource;

impl TemplateSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TemplateSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for TemplateSource {
    fn kind(&self) -> &str {
        "template"
    }

    fn detect(&self, _path: &Path) -> i32 {
        0 // nunca autodetectado: se fuerza con --as template o config "adapter".
    }

    fn options(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("template", "plantilla con <campos> (obligatoria)"),
            ("time_field", "campo del tiempo"),
            ("message_field", "campo del mensaje"),
            ("level_field", "campo del nivel"),
            ("entity_field", "campo de la entidad"),
            ("actor_field", "campo del actor"),
        ]
    }

    fn open(
        &self,
        path: &Path,
        watermark: Option<Watermark>,
        cfg: &SourceConfig,
    ) -> CoreResult<Box<dyn chrono_core::Cursor>> {
        let texto = cfg
            .options
            .get("template")
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| CoreError::Other("chrono-source-template: falta la opción 'template'".to_string()))?;
        let plantilla = matcher::Plantilla::parse(texto)
            .map_err(|e| CoreError::Other(format!("chrono-source-template: plantilla inválida: {e}")))?;
        let roles = record::Roles::resolve(&plantilla, &cfg.options)
            .map_err(|e| CoreError::Other(format!("chrono-source-template: {e}")))?;

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
                // Con hash de prefijo (`off:<n>|p:<hex>`) se comprueba la MISMA
                // región que lo produjo (`min(4096, off)`); sin él (watermark
                // viejo) solo queda la comprobación de longitud.
                if let Some(expected) = &prefix_hex {
                    if &cursor::hash_prefix(path, off)? != expected {
                        return Err(CoreError::Diverged(wm.value.clone()));
                    }
                }
                off
            }
        };

        Ok(Box::new(cursor::TemplateCursor::open(path, start_offset, plantilla, roles)?))
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
    struct TempTpl {
        path: std::path::PathBuf,
    }

    impl TempTpl {
        fn write(contents: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "chrono-source-template-test-{}-{}-{n}.log",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            fs::write(&path, contents).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn append(&self, contents: &str) {
            let mut f = fs::OpenOptions::new().append(true).open(&self.path).unwrap();
            f.write_all(contents.as_bytes()).unwrap();
        }

        fn overwrite(&self, contents: &str) {
            fs::write(&self.path, contents).unwrap();
        }
    }

    impl Drop for TempTpl {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    const TPL: &str = "<time> <level> [<service>] <message>";

    fn cfg_with(options: &[(&str, &str)]) -> SourceConfig {
        let mut m = BTreeMap::new();
        for (k, v) in options {
            m.insert(k.to_string(), v.to_string());
        }
        SourceConfig { options: m }
    }

    fn cfg_tpl() -> SourceConfig {
        cfg_with(&[("template", TPL)])
    }

    fn drain(cur: &mut Box<dyn chrono_core::Cursor>) -> Vec<chrono_core::Event> {
        let mut v = Vec::new();
        while let Some(e) = cur.next().unwrap() {
            v.push(e);
        }
        v
    }

    const LOG: &str = "2024-01-01T10:00:00Z info [api] arranque ok\n\
                       2024-01-01T10:00:01Z error [db] conexión perdida\n";

    #[test]
    fn detect_siempre_cero() {
        let f = TempTpl::write(LOG);
        assert_eq!(TemplateSource::new().detect(f.path()), 0);
    }

    #[test]
    fn open_sin_template_da_error_claro() {
        let f = TempTpl::write(LOG);
        let err = TemplateSource::new().open(f.path(), None, &SourceConfig::default()).err().unwrap();
        match err {
            CoreError::Other(m) => assert_eq!(m, "chrono-source-template: falta la opción 'template'"),
            e => panic!("error inesperado: {e:?}"),
        }
    }

    #[test]
    fn open_con_plantilla_invalida_o_override_desconocido_falla() {
        let f = TempTpl::write(LOG);
        let src = TemplateSource::new();
        assert!(src.open(f.path(), None, &cfg_with(&[("template", "sin marcadores")])).is_err());
        assert!(src.open(f.path(), None, &cfg_with(&[("template", TPL), ("time_field", "nope")])).is_err());
    }

    #[test]
    fn un_evento_por_linea_con_mapeo_de_roles() {
        let f = TempTpl::write(LOG);
        let mut cur = TemplateSource::new().open(f.path(), None, &cfg_tpl()).unwrap();
        let evs = drain(&mut cur);
        assert_eq!(evs.len(), 2);

        let e = &evs[0];
        assert_eq!(e.kind, "log");
        assert_eq!(e.at_epoch, 1_704_103_200_000); // 2024-01-01T10:00:00Z en ms
        assert_eq!(e.at, "2024-01-01T10:00:00Z");
        assert_eq!(e.level, "INFO");
        assert_eq!(e.title, "arranque ok");
        assert_eq!(e.touches.len(), 1);
        assert_eq!(e.touches[0].entity, "api");
        assert_eq!(e.touches[0].entity_type, "log-source");
        assert_eq!(e.id.len(), 16);
        assert!(!e.is_bulk);
        assert!(e.source_id.starts_with("template:"));
        assert_eq!(evs[1].level, "ERROR");
        assert_ne!(evs[0].id, evs[1].id);
    }

    #[test]
    fn marcador_extra_va_a_attrs_y_override_de_roles() {
        let f = TempTpl::write("1700000000 web-1 GET /a/b | cae\n");
        let cfg = cfg_with(&[
            ("template", "<when> <box> <verb> <path> | <what>"),
            ("time_field", "when"),
            ("actor_field", "box"),
            ("message_field", "what"),
        ]);
        let mut cur = TemplateSource::new().open(f.path(), None, &cfg).unwrap();
        let e = cur.next().unwrap().unwrap();
        assert_eq!(e.at_epoch, 1_700_000_000_000);
        assert_eq!(e.actor.name, "web-1");
        assert_eq!(e.actor.key, "web-1");
        assert_eq!(e.title, "cae");
        assert_eq!(e.attrs.get("verb").map(String::as_str), Some("GET"));
        assert_eq!(e.attrs.get("path").map(String::as_str), Some("/a/b"));
        assert!(!e.attrs.contains_key("box"));
        // sin entity: cae al actor.
        assert_eq!(e.touches[0].entity, "web-1");
    }

    #[test]
    fn entity_cae_a_unknown_y_tiempo_invalido_entra_con_cero() {
        let f = TempTpl::write("???? algo raro\n");
        let cfg = cfg_with(&[("template", "<ts> <resto>")]);
        let mut cur = TemplateSource::new().open(f.path(), None, &cfg).unwrap();
        let e = cur.next().unwrap().unwrap();
        assert_eq!(e.at_epoch, 0);
        assert_eq!(e.at, "");
        assert_eq!(e.touches[0].entity, "unknown");
        // sin marcador de mensaje: título = línea cruda recortada.
        assert_eq!(e.title, "???? algo raro");
        assert_eq!(e.attrs["resto"], "algo raro");
    }

    #[test]
    fn linea_que_no_casa_se_descarta() {
        let f = TempTpl::write(
            "2024-01-01T10:00:00Z info [api] uno\nbasura sin formato\n\n2024-01-01T10:00:02Z warn [api] dos\n",
        );
        let mut cur = TemplateSource::new().open(f.path(), None, &cfg_tpl()).unwrap();
        let evs = drain(&mut cur);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].title, "uno");
        assert_eq!(evs[1].title, "dos");
    }

    #[test]
    fn deterministas_entre_aperturas() {
        let f = TempTpl::write(LOG);
        let src = TemplateSource::new();
        let a = drain(&mut src.open(f.path(), None, &cfg_tpl()).unwrap());
        let b = drain(&mut src.open(f.path(), None, &cfg_tpl()).unwrap());
        assert_eq!(a, b);
    }

    #[test]
    fn watermark_incremental_y_append_no_diverge() {
        let f = TempTpl::write(LOG);
        let src = TemplateSource::new();
        let mut cur = src.open(f.path(), None, &cfg_tpl()).unwrap();
        assert_eq!(drain(&mut cur).len(), 2);
        let wm = cur.watermark();
        assert_eq!(wm.kind, "template");
        assert!(wm.value.starts_with(&format!("off:{}|p:", LOG.len())), "{}", wm.value);

        // Sin cambios: nada nuevo.
        let mut cur2 = src.open(f.path(), Some(wm.clone()), &cfg_tpl()).unwrap();
        assert!(cur2.next().unwrap().is_none());

        // Append: solo llegan las líneas nuevas.
        f.append("2024-01-01T10:00:03Z info [api] tres\n");
        let mut cur3 = src.open(f.path(), Some(wm), &cfg_tpl()).unwrap();
        let nuevos = drain(&mut cur3);
        assert_eq!(nuevos.len(), 1);
        assert_eq!(nuevos[0].title, "tres");
    }

    #[test]
    fn watermark_viejo_sin_hash_es_retrocompatible() {
        let f = TempTpl::write(LOG);
        let wm = Watermark { kind: "template".into(), value: format!("off:{}", LOG.len()) };
        let mut cur = TemplateSource::new().open(f.path(), Some(wm), &cfg_tpl()).unwrap();
        assert!(cur.next().unwrap().is_none());
    }

    #[test]
    fn diverge_por_truncado() {
        let f = TempTpl::write(LOG);
        let src = TemplateSource::new();
        let mut cur = src.open(f.path(), None, &cfg_tpl()).unwrap();
        drain(&mut cur);
        let wm = cur.watermark();
        f.overwrite("2024-01-01T10:00:00Z info [api] x\n");
        let err = src.open(f.path(), Some(wm), &cfg_tpl()).err().unwrap();
        assert!(matches!(err, CoreError::Diverged(_)), "{err:?}");
    }

    #[test]
    fn diverge_por_reescritura_de_prefijo() {
        let f = TempTpl::write(LOG);
        let src = TemplateSource::new();
        let mut cur = src.open(f.path(), None, &cfg_tpl()).unwrap();
        drain(&mut cur);
        let wm = cur.watermark();
        // Mismo tamaño, contenido distinto en la región ya consumida.
        let otro = LOG.replace("arranque ok", "arranque KO");
        assert_eq!(otro.len(), LOG.len());
        f.overwrite(&otro);
        let err = src.open(f.path(), Some(wm), &cfg_tpl()).err().unwrap();
        assert!(matches!(err, CoreError::Diverged(_)), "{err:?}");
    }

    #[test]
    fn watermark_malformado_diverge() {
        let f = TempTpl::write(LOG);
        let wm = Watermark { kind: "template".into(), value: "basura".into() };
        let err = TemplateSource::new().open(f.path(), Some(wm), &cfg_tpl()).err().unwrap();
        assert!(matches!(err, CoreError::Diverged(_)));
    }

    #[test]
    fn manifest_estable_entre_init_y_sync() {
        let f = TempTpl::write(LOG);
        let src = TemplateSource::new();
        let mut cur = src.open(f.path(), None, &cfg_tpl()).unwrap();
        let m_init = cur.manifest();
        drain(&mut cur);
        let wm = cur.watermark();
        let cur2 = src.open(f.path(), Some(wm), &cfg_tpl()).unwrap();
        assert_eq!(m_init, cur2.manifest());
        assert_eq!(m_init["format"], "template");
        assert_eq!(m_init["template"], TPL);
    }
}
