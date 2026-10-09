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
//! Los selectores pueden ser rutas JSON (`message.role`, `data.0.id`,
//! `message.content[].name`, ver `path`) y, con la opción `explode`, se
//! resuelven en "elemento-luego-raíz" (ver `record`). Un selector sin `.` ni
//! `[` es una clave de primer nivel, igual que antes.
//!
//! `resolve_config`/`JsonlConfig` calculan además una resolución "de
//! muestra" (a partir de la primera línea no vacía del fichero) que solo se
//! usa para `manifest()`: información para el usuario sobre qué campos se
//! autodetectaron, estable entre un `init` y un `sync` incremental.

use crate::path::{self, Scope};
use chrono_core::{CoreError, Result as CoreResult, SourceConfig};
use serde_json::Map;
use serde_json::Value;

pub const TIME_FIELDS: &[&str] = &["ts", "time", "timestamp", "@timestamp", "date", "eventTime"];
pub const MESSAGE_FIELDS: &[&str] = &["msg", "message", "text", "log", "event"];
pub const LEVEL_FIELDS: &[&str] = &["level", "severity", "lvl", "loglevel"];
pub const ENTITY_FIELDS: &[&str] =
    &["service", "logger", "name", "path", "source", "unit", "container", "component"];
pub const ACTOR_FIELDS: &[&str] = &["host", "hostname", "service", "pod", "node"];

/// Expansión de un array de la línea: una línea → N eventos (ver
/// `SourceConfig.options["explode"]` / `["explode_where"]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplodeSpec {
    /// Ruta (ver `path`) al array a expandir, p.ej. `message.content`.
    pub path: String,
    /// Filtro opcional `(clave, valor)`: solo expanden los elementos objeto
    /// cuyo campo `clave` (ruta relativa al elemento) sea igual a `valor`.
    pub filter: Option<(String, String)>,
}

impl ExplodeSpec {
    /// Elementos que expanden en `root`, como `(índice en el array original,
    /// elemento)`, en el orden natural del array. El índice es el del array
    /// SIN filtrar, para que el `id` de cada evento no dependa del filtro.
    /// Vacío si la ruta falta o no apunta a un array. Los elementos que no son
    /// objeto (escalares, null, arrays) se omiten: no hay de dónde leer campos.
    pub fn elements<'a>(&self, root: &'a Map<String, Value>) -> Vec<(usize, &'a Map<String, Value>)> {
        let Some(Value::Array(items)) = path::lookup(root, &self.path) else {
            return Vec::new();
        };
        items
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_object().map(|m| (i, m)))
            .filter(|(_, m)| match &self.filter {
                None => true,
                Some((key, want)) => path::lookup(m, key).and_then(scalar_to_string).is_some_and(|got| got == *want),
            })
            .collect()
    }
}

/// Convierte un valor JSON escalar (string/number/bool) a `String`; `None`
/// para null, array u objeto.
pub fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Overrides globales de `SourceConfig.options`, uno por campo lógico.
/// `None` significa "sin override": cae a la autodetección por línea.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonlOverrides {
    pub time_field: Option<String>,
    pub message_field: Option<String>,
    pub level_field: Option<String>,
    pub entity_field: Option<String>,
    pub actor_field: Option<String>,
    pub explode: Option<ExplodeSpec>,
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
    /// Ruta de `explode` ("" si no hay expansión).
    pub explode: String,
    /// Filtro de `explode_where` como `clave=valor` ("" si no hay).
    pub explode_where: String,
}

/// Primer candidato que resuelve en `scope` (como clave o como ruta), o ""
/// si ninguno.
fn first_present(scope: &Scope<'_>, candidates: &[&str]) -> String {
    candidates.iter().find(|c| scope.get(c).is_some()).map(|c| c.to_string()).unwrap_or_default()
}

/// Valor de override en `options`, ignorando cadenas vacías (se tratan como
/// "sin override": cae a la autodetección).
fn override_of(options: &std::collections::BTreeMap<String, String>, key: &str) -> Option<String> {
    options.get(key).map(String::as_str).filter(|v| !v.is_empty()).map(str::to_string)
}

/// Como `override_of` pero ignorando también las cadenas en blanco (opciones
/// de expansión, donde un valor de solo espacios es "sin valor").
fn option_trimmed(options: &std::collections::BTreeMap<String, String>, key: &str) -> Option<String> {
    options.get(key).map(|v| v.trim()).filter(|v| !v.is_empty()).map(str::to_string)
}

/// Parsea `clave=valor` (separa en el primer `=`). `None` si falta el `=` o
/// la clave queda vacía.
fn parse_where(s: &str) -> Option<(String, String)> {
    let (k, v) = s.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), v.trim().to_string()))
}

/// Valida las opciones de expansión: `explode_where` requiere `explode` y
/// debe tener la forma `clave=valor`. Se llama al abrir el cursor para que una
/// config errónea falle en alto en vez de no expandir nada en silencio.
pub fn validate_options(cfg: &SourceConfig) -> CoreResult<()> {
    let explode = option_trimmed(&cfg.options, "explode");
    if let Some(w) = option_trimmed(&cfg.options, "explode_where") {
        if explode.is_none() {
            return Err(CoreError::Other("jsonl: `explode_where` requiere `explode`".to_string()));
        }
        if parse_where(&w).is_none() {
            return Err(CoreError::Other(format!("jsonl: `explode_where` debe ser `clave=valor`, no {w:?}")));
        }
    }
    Ok(())
}

pub fn overrides_from(cfg: &SourceConfig) -> JsonlOverrides {
    let explode = option_trimmed(&cfg.options, "explode").map(|p| ExplodeSpec {
        path: path::normalize_array_path(&p),
        filter: option_trimmed(&cfg.options, "explode_where").and_then(|w| parse_where(&w)),
    });
    JsonlOverrides {
        time_field: override_of(&cfg.options, "time_field"),
        message_field: override_of(&cfg.options, "message_field"),
        level_field: override_of(&cfg.options, "level_field"),
        entity_field: override_of(&cfg.options, "entity_field"),
        actor_field: override_of(&cfg.options, "actor_field"),
        explode,
    }
}

/// Resuelve el selector para UN ámbito concreto: el override si lo hay (se
/// usa tal cual, exista o no), o si no, el primer candidato que resuelve en
/// `scope` ("" si ninguno). Con `explode`, "resuelve" sigue la regla
/// elemento-luego-raíz de `Scope::get`.
pub fn resolve_field_in(override_value: Option<&str>, candidates: &[&str], scope: &Scope<'_>) -> String {
    match override_value {
        Some(v) => v.to_string(),
        None => first_present(scope, candidates),
    }
}

/// Como `resolve_field_in` sobre un único objeto (sin expansión).
#[cfg(test)]
pub fn resolve_field(override_value: Option<&str>, candidates: &[&str], obj: &Map<String, Value>) -> String {
    resolve_field_in(override_value, candidates, &Scope::root_only(obj))
}

/// Resuelve los 5 campos para `sample` (informativo, para `manifest()`). Con
/// `explode`, el ámbito es el primer elemento expandible de la muestra (y la
/// muestra como raíz); si no hay ninguno, solo la raíz.
pub fn resolve_config(cfg: &SourceConfig, sample: &Map<String, Value>) -> JsonlConfig {
    let ov = overrides_from(cfg);
    let elem = ov.explode.as_ref().and_then(|spec| spec.elements(sample).into_iter().next().map(|(_, m)| m));
    let scope = Scope { elem, root: sample };
    JsonlConfig {
        time_field: resolve_field_in(ov.time_field.as_deref(), TIME_FIELDS, &scope),
        message_field: resolve_field_in(ov.message_field.as_deref(), MESSAGE_FIELDS, &scope),
        level_field: resolve_field_in(ov.level_field.as_deref(), LEVEL_FIELDS, &scope),
        entity_field: resolve_field_in(ov.entity_field.as_deref(), ENTITY_FIELDS, &scope),
        actor_field: resolve_field_in(ov.actor_field.as_deref(), ACTOR_FIELDS, &scope),
        explode: ov.explode.as_ref().map(|e| e.path.clone()).unwrap_or_default(),
        explode_where: ov.explode.as_ref().and_then(|e| e.filter.as_ref()).map(|(k, v)| format!("{k}={v}")).unwrap_or_default(),
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
