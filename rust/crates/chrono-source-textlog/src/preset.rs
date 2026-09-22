//! Selección de preset (syslog | nginx) y utilidades de tokenizado
//! compartidas entre los dos formatos de texto plano.
//!
//! Nota importante sobre `detect`: el trait `chrono_core::Source::detect(&self,
//! path)` NO recibe `SourceConfig` (solo `open` lo recibe), así que el
//! `detect` del trait no puede aplicar el score 60 de "`options[\"preset\"]`
//! dado y la primera línea casa ese preset" que pide el encargo — no hay
//! forma de leer `options` ahí sin tocar la firma del trait en `chrono-core`
//! (fuera de las fronteras de este encargo). El algoritmo COMPLETO (con y sin
//! preset explícito) vive en `TextlogSource::detect_with_config` (`lib.rs`),
//! un método propio del adaptador; el `detect` del trait delega en él con
//! `SourceConfig::default()`, que cae siempre en la rama "sin preset" (40/0).
//! Es lo único observable hoy vía `Registry::pick`, pero los tests ejercitan
//! el algoritmo completo llamando a `detect_with_config` directamente.

use crate::nginx::{self, NginxFields};
use crate::syslog::{self, SyslogFields};
use chrono_core::SourceConfig;

/// Preset activo: qué gramática de línea de texto se usa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Syslog,
    Nginx,
}

impl Preset {
    pub fn as_str(&self) -> &'static str {
        match self {
            Preset::Syslog => "syslog",
            Preset::Nginx => "nginx",
        }
    }

    /// Distinto de `FromStr::from_str` a propósito (clippy `should_implement_trait`):
    /// no queremos que este `Option` se confunda con la semántica de `FromStr`.
    pub fn parse_name(s: &str) -> Option<Self> {
        match s {
            "syslog" => Some(Preset::Syslog),
            "nginx" => Some(Preset::Nginx),
            _ => None,
        }
    }
}

/// Una línea ya parseada por el preset activo, previa al mapeo a `Event`
/// (ver `record::line_to_event`).
pub enum ParsedLine {
    Nginx(NginxFields),
    Syslog(SyslogFields),
}

/// Intenta parsear `line` con `preset`. `fallback_year` solo lo usa syslog
/// RFC3164 (nginx siempre trae año completo en la línea).
pub fn parse_line(preset: Preset, line: &str, fallback_year: i64) -> Option<ParsedLine> {
    match preset {
        Preset::Nginx => nginx::parse(line).map(ParsedLine::Nginx),
        Preset::Syslog => syslog::parse(line, fallback_year).map(ParsedLine::Syslog),
    }
}

/// Prueba ambos presets contra `first_line` (orden estable: syslog, luego
/// nginx) y devuelve el primero que casa, o `None` si ninguno lo hace.
pub fn autodetect_preset(first_line: &str, fallback_year: i64) -> Option<Preset> {
    [Preset::Syslog, Preset::Nginx]
        .into_iter()
        .find(|&candidate| parse_line(candidate, first_line, fallback_year).is_some())
}

/// Año de referencia para syslog RFC3164 (el formato no trae año en la
/// línea): `options["year"]` si es un entero válido, o 1970 si no se da.
/// Determinista a propósito: NUNCA el año del reloj del sistema.
pub fn fallback_year_from(cfg: &SourceConfig) -> i64 {
    cfg.options.get("year").and_then(|s| s.parse::<i64>().ok()).unwrap_or(1970)
}

/// Primer trozo no vacío de `s` separado por espacio en blanco, y el resto
/// de la cadena (con el separador aún al principio: cada llamada recorta su
/// propio extremo inicial con `trim_start`). `None` si `s` no tiene ningún
/// trozo no vacío.
pub(crate) fn split_ws(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    match s.find(char::is_whitespace) {
        Some(i) => Some((&s[..i], &s[i..])),
        None => Some((s, "")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ws_parte_por_espacios() {
        assert_eq!(split_ws("  hola   mundo"), Some(("hola", "   mundo")));
        assert_eq!(split_ws("solo"), Some(("solo", "")));
        assert_eq!(split_ws("   "), None);
        assert_eq!(split_ws(""), None);
    }

    #[test]
    fn preset_from_str_y_as_str_son_inversas() {
        assert_eq!(Preset::parse_name("syslog").map(|p| p.as_str()), Some("syslog"));
        assert_eq!(Preset::parse_name("nginx").map(|p| p.as_str()), Some("nginx"));
        assert!(Preset::parse_name("otro").is_none());
    }

    #[test]
    fn autodetect_preset_rechaza_json_y_csv() {
        assert!(autodetect_preset(r#"{"foo": "bar"}"#, 1970).is_none());
        assert!(autodetect_preset("a,b,c,d", 1970).is_none());
    }
}
