//! Selección de idioma para los mensajes de usuario (ayuda, progreso,
//! avisos) del cli. La salida JSON NUNCA se traduce (es el contrato).
//!
//! Réplica de `internal/i18n` (Go): inglés por defecto; español si el
//! locale del SO (`LC_ALL`/`LC_MESSAGES`/`LANG`) empieza por "es", o si
//! `CHRONO_LANG=es`; `--lang` (si se pasa) gana sobre todo lo anterior.

use std::sync::atomic::{AtomicBool, Ordering};

static SPANISH: AtomicBool = AtomicBool::new(false);

fn starts_with_es(v: &str) -> bool {
    v.to_ascii_lowercase().starts_with("es")
}

/// Detecta el idioma por entorno: `CHRONO_LANG`, si no `LC_ALL`/`LC_MESSAGES`/
/// `LANG` (en ese orden), si no inglés.
fn detect() -> bool {
    if let Ok(v) = std::env::var("CHRONO_LANG") {
        if !v.is_empty() {
            return starts_with_es(&v);
        }
    }
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return starts_with_es(&v);
            }
        }
    }
    false
}

/// Inicializa el idioma activo: detección por entorno, luego `--lang` (si se
/// pasó) gana sobre ella. Llamar una única vez al arrancar, antes de emitir
/// cualquier texto de usuario (incluida la ayuda).
pub fn init(lang_flag: Option<&str>) {
    let mut es = detect();
    if let Some(v) = lang_flag {
        if !v.is_empty() {
            es = starts_with_es(v);
        }
    }
    SPANISH.store(es, Ordering::Relaxed);
}

/// `true` si el idioma activo es español.
pub fn is_es() -> bool {
    SPANISH.load(Ordering::Relaxed)
}

/// Elige el texto según el idioma activo (inglés por defecto). Acepta
/// `&str` o `String` en cada rama para poder pasar mensajes ya formateados
/// (`format!(...)`) sin fricción.
pub fn t(en: impl Into<String>, es: impl Into<String>) -> String {
    if is_es() {
        es.into()
    } else {
        en.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Los tests de detección mutan variables de entorno (estado global del
    // proceso): se serializan con un mutex para no interferir entre sí ni
    // con otros tests del binario que corren en paralelo.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved: Vec<(&str, Option<String>)> =
            vars.iter().map(|(k, _)| (*k, std::env::var(k).ok())).collect();
        for (k, v) in vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    #[test]
    fn ingles_por_defecto_sin_entorno() {
        with_env(&[("CHRONO_LANG", None), ("LC_ALL", None), ("LC_MESSAGES", None), ("LANG", None)], || {
            init(None);
            assert!(!is_es());
            assert_eq!(t("hello", "hola"), "hello");
        });
    }

    #[test]
    fn chrono_lang_es_activa_espanol() {
        with_env(&[("CHRONO_LANG", Some("es")), ("LC_ALL", None), ("LC_MESSAGES", None), ("LANG", None)], || {
            init(None);
            assert!(is_es());
            assert_eq!(t("hello", "hola"), "hola");
        });
    }

    #[test]
    fn locale_del_so_activa_espanol() {
        with_env(&[("CHRONO_LANG", None), ("LC_ALL", None), ("LC_MESSAGES", None), ("LANG", Some("es_ES.UTF-8"))], || {
            init(None);
            assert!(is_es());
        });
    }

    #[test]
    fn flag_lang_gana_sobre_el_entorno() {
        with_env(&[("CHRONO_LANG", Some("es")), ("LC_ALL", None), ("LC_MESSAGES", None), ("LANG", None)], || {
            init(Some("en"));
            assert!(!is_es());
        });
        with_env(&[("CHRONO_LANG", None), ("LC_ALL", None), ("LC_MESSAGES", None), ("LANG", None)], || {
            init(Some("es"));
            assert!(is_es());
        });
    }
}
