//! Resolución de configuración del adaptador: qué campo de un objeto JSON es
//! el tiempo, el mensaje, el nivel, la entidad y el actor.
//!
//! Los logs JSONL reales suelen ser heterogéneos línea a línea (un logger
//! manda `ts` epoch, otro `@timestamp` ISO); por eso la autodetección de
//! "primer candidato presente" se hace POR LÍNEA (`resolve_field`, usada
//! desde `record::line_to_event`), no una sola vez para todo el fichero.
//! Un override explícito en `SourceConfig.options` sí es global: se aplica
//! igual a todas las líneas.
//!
//! `resolve_config`/`JsonlConfig` calculan además una resolución "de
//! muestra" (a partir de la primera línea no vacía del fichero) que solo se
//! usa para `manifest()`: información para el usuario sobre qué campos se
//! autodetectaron, estable entre un `init` y un `sync` incremental.

use chrono_core::SourceConfig;
use serde_json::Map;
use serde_json::Value;

pub const TIME_FIELDS: &[&str] = &["ts", "time", "timestamp", "@timestamp", "date", "eventTime"];
pub const MESSAGE_FIELDS: &[&str] = &["msg", "message", "text", "log", "event"];
pub const LEVEL_FIELDS: &[&str] = &["level", "severity", "lvl", "loglevel"];
pub const ENTITY_FIELDS: &[&str] =
    &["service", "logger", "name", "path", "source", "unit", "container", "component"];
pub const ACTOR_FIELDS: &[&str] = &["host", "hostname", "service", "pod", "node"];

/// Overrides globales de `SourceConfig.options`, uno por campo lógico.
/// `None` significa "sin override": cae a la autodetección por línea.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonlOverrides {
    pub time_field: Option<String>,
    pub message_field: Option<String>,
    pub level_field: Option<String>,
    pub entity_field: Option<String>,
    pub actor_field: Option<String>,
}

/// Resolución de campos para una única línea u otra "muestra" representativa
/// (usada para `manifest()`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonlConfig {
    pub time_field: String,
    pub message_field: String,
    pub level_field: String,
    pub entity_field: String,
    pub actor_field: String,
}

/// Primer candidato presente como clave en `obj`, o "" si ninguno lo está.
fn first_present(obj: &Map<String, Value>, candidates: &[&str]) -> String {
    candidates
        .iter()
        .find(|c| obj.contains_key(**c))
        .map(|c| c.to_string())
        .unwrap_or_default()
}

/// Valor de override en `options`, ignorando cadenas vacías (se tratan como
/// "sin override": cae a la autodetección).
fn override_of(options: &std::collections::BTreeMap<String, String>, key: &str) -> Option<String> {
    options.get(key).map(String::as_str).filter(|v| !v.is_empty()).map(str::to_string)
}

pub fn overrides_from(cfg: &SourceConfig) -> JsonlOverrides {
    JsonlOverrides {
        time_field: override_of(&cfg.options, "time_field"),
        message_field: override_of(&cfg.options, "message_field"),
        level_field: override_of(&cfg.options, "level_field"),
        entity_field: override_of(&cfg.options, "entity_field"),
        actor_field: override_of(&cfg.options, "actor_field"),
    }
}

/// Resuelve el nombre de campo para UN objeto concreto: el override si lo
/// hay (se usa tal cual, exista o no esa clave en `obj`), o si no, el primer
/// candidato de `candidates` presente como clave en `obj` ("" si ninguno).
pub fn resolve_field(override_value: Option<&str>, candidates: &[&str], obj: &Map<String, Value>) -> String {
    match override_value {
        Some(v) => v.to_string(),
        None => first_present(obj, candidates),
    }
}

/// Resuelve los 5 campos para `sample` (informativo, para `manifest()`).
pub fn resolve_config(cfg: &SourceConfig, sample: &Map<String, Value>) -> JsonlConfig {
    let ov = overrides_from(cfg);
    JsonlConfig {
        time_field: resolve_field(ov.time_field.as_deref(), TIME_FIELDS, sample),
        message_field: resolve_field(ov.message_field.as_deref(), MESSAGE_FIELDS, sample),
        level_field: resolve_field(ov.level_field.as_deref(), LEVEL_FIELDS, sample),
        entity_field: resolve_field(ov.entity_field.as_deref(), ENTITY_FIELDS, sample),
        actor_field: resolve_field(ov.actor_field.as_deref(), ACTOR_FIELDS, sample),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn autodetecta_primer_presente() {
        let sample = sample_obj(json!({"@timestamp": "x", "msg": "hola", "service": "api", "level": "info"}));
        let cfg = resolve_config(&SourceConfig::default(), &sample);
        assert_eq!(cfg.time_field, "@timestamp");
        assert_eq!(cfg.message_field, "msg");
        assert_eq!(cfg.level_field, "level");
        assert_eq!(cfg.entity_field, "service");
    }

    #[test]
    fn override_gana_a_la_autodeteccion() {
        let sample = sample_obj(json!({"ts": 1, "msg": "hola"}));
        let mut opts = std::collections::BTreeMap::new();
        opts.insert("time_field".to_string(), "customTime".to_string());
        let cfg = resolve_config(&SourceConfig { options: opts }, &sample);
        assert_eq!(cfg.time_field, "customTime");
    }

    #[test]
    fn sin_candidatos_presentes_queda_vacio() {
        let sample = sample_obj(json!({"foo": "bar"}));
        let cfg = resolve_config(&SourceConfig::default(), &sample);
        assert_eq!(cfg.time_field, "");
        assert_eq!(cfg.entity_field, "");
    }

    #[test]
    fn resolve_field_es_por_objeto_no_global() {
        let with_ts = sample_obj(json!({"ts": 1}));
        let with_at_timestamp = sample_obj(json!({"@timestamp": "x"}));
        assert_eq!(resolve_field(None, TIME_FIELDS, &with_ts), "ts");
        assert_eq!(resolve_field(None, TIME_FIELDS, &with_at_timestamp), "@timestamp");
    }
}
