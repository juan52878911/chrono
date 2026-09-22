//! Mapeo de una línea JSON ya parseada a un `chrono_core::Event`. Única
//! implementación del mapeo; la usan tanto el cursor de streaming como los
//! tests.

use crate::config::{self, JsonlOverrides};
use crate::simhash::{fnv1a64_line, simhash, tokenize};
use crate::timeutil::{epoch_to_iso8601, parse_time_value};
use chrono_core::{Actor, Event, Touch};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Tope de claves de primer nivel volcadas a `attrs` (evita inflar eventos
/// con objetos JSON muy anchos).
const MAX_ATTRS: usize = 30;

/// Longitud máxima del `title` cuando se recorta la línea cruda como
/// respaldo (sin `message_field`).
const RAW_TITLE_MAX_CHARS: usize = 200;

/// Campos candidatos a `id` explícito, en orden de prioridad.
const ID_FIELDS: &[&str] = &["id", "_id", "uuid"];

/// Convierte un valor JSON escalar (string/number/bool) a `String`; `None`
/// para null, array u objeto (no son escalares representables en `attrs`).
fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `id` determinista: el primer campo de `ID_FIELDS` presente y escalar no
/// vacío, o si no, FNV-1a 64 de (bytes de la línea cruda + offset) en hex.
fn compute_id(obj: &Map<String, Value>, raw_line: &str, offset: u64) -> String {
    for field in ID_FIELDS {
        if let Some(v) = obj.get(*field) {
            if let Some(s) = scalar_to_string(v) {
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    format!("{:016x}", fnv1a64_line(raw_line.as_bytes(), offset))
}

/// Valor de un campo configurado como escalar, o "" si el campo no está
/// configurado, no existe en el objeto, o no es escalar.
fn field_value(obj: &Map<String, Value>, field: &str) -> String {
    if field.is_empty() {
        return String::new();
    }
    obj.get(field).and_then(scalar_to_string).unwrap_or_default()
}

/// Recorta `s` a lo sumo `max_chars` caracteres (respetando fronteras UTF-8).
fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Vuelca hasta `MAX_ATTRS` campos escalares de primer nivel a un `BTreeMap`
/// determinista (claves ordenadas alfabéticamente, orden natural de
/// `serde_json::Map` sin la feature `preserve_order`).
fn scalar_attrs(obj: &Map<String, Value>) -> BTreeMap<String, String> {
    obj.iter()
        .filter_map(|(k, v)| scalar_to_string(v).map(|s| (k.clone(), s)))
        .take(MAX_ATTRS)
        .collect()
}

/// Convierte una línea JSONL ya parseada (`obj`, `raw_line` sin salto final,
/// `offset` = posición en bytes del inicio de la línea en el fichero) en un
/// `Event`, según `ov` (overrides globales de `SourceConfig.options`) y
/// `source_id` ("jsonl:<ruta absoluta>").
///
/// El campo efectivo de cada dimensión (tiempo, mensaje, nivel, entidad,
/// actor) se resuelve POR LÍNEA: si hay override se usa siempre; si no, el
/// primer candidato presente en ESTE objeto concreto (los logs JSONL reales
/// mezclan formas de un logger a otro, p.ej. `ts` epoch en unas líneas y
/// `@timestamp` ISO en otras).
pub fn line_to_event(obj: &Map<String, Value>, raw_line: &str, offset: u64, ov: &JsonlOverrides, source_id: &str) -> Event {
    let id = compute_id(obj, raw_line, offset);

    let time_field = config::resolve_field(ov.time_field.as_deref(), config::TIME_FIELDS, obj);
    // Documentado en el encargo: sin tiempo válido, at_epoch=0 y at="" — el
    // evento entra igual (no se descarta por falta de tiempo). Se distingue
    // "no había tiempo" de "el tiempo era exactamente epoch 0" mirando si el
    // parseo tuvo éxito, no solo el valor resultante.
    let time_value = if time_field.is_empty() { None } else { obj.get(time_field.as_str()).and_then(parse_time_value) };
    let at_epoch = time_value.unwrap_or(0);
    let at = if time_value.is_some() { epoch_to_iso8601(at_epoch) } else { String::new() };

    let message_field = config::resolve_field(ov.message_field.as_deref(), config::MESSAGE_FIELDS, obj);
    let message = field_value(obj, &message_field);
    let title = if !message.is_empty() { message } else { truncate_chars(raw_line.trim(), RAW_TITLE_MAX_CHARS) };

    let level_field = config::resolve_field(ov.level_field.as_deref(), config::LEVEL_FIELDS, obj);
    let level = field_value(obj, &level_field).to_uppercase();

    let actor_field = config::resolve_field(ov.actor_field.as_deref(), config::ACTOR_FIELDS, obj);
    let actor_value = field_value(obj, &actor_field);
    let actor = Actor { name: actor_value.clone(), key: actor_value.clone() };

    let entity_field = config::resolve_field(ov.entity_field.as_deref(), config::ENTITY_FIELDS, obj);
    let entity_value = field_value(obj, &entity_field);
    let entity = if !entity_value.is_empty() {
        entity_value
    } else if !actor_value.is_empty() {
        actor_value
    } else {
        "unknown".to_string()
    };

    let toks = tokenize(&title);
    let hash = simhash(&toks);

    Event {
        id,
        source_id: source_id.to_string(),
        kind: "log".to_string(),
        at,
        at_epoch,
        actor,
        title,
        body: String::new(),
        level,
        attrs: scalar_attrs(obj),
        touches: vec![Touch { entity, entity_type: "log-source".to_string(), weight: 1, attrs: BTreeMap::new() }],
        links: Vec::new(),
        simhash: hash,
        is_bulk: false, // un único touch por evento: nunca es "bulk" en logs.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn no_overrides() -> JsonlOverrides {
        JsonlOverrides::default()
    }

    #[test]
    fn mapea_campos_basicos() {
        let sample = obj(json!({"ts": 1_704_067_200, "msg": "hola", "service": "api", "level": "info"}));
        let ev = line_to_event(&sample, r#"{"ts":1704067200,"msg":"hola","service":"api","level":"info"}"#, 0, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(ev.at_epoch, 1_704_067_200);
        assert_eq!(ev.at, "2024-01-01T00:00:00Z");
        assert_eq!(ev.title, "hola");
        assert_eq!(ev.level, "INFO");
        assert_eq!(ev.touches[0].entity, "api");
        assert_eq!(ev.kind, "log");
        assert_eq!(ev.body, "");
    }

    #[test]
    fn id_explicito_si_existe() {
        let sample = obj(json!({"id": "abc-123", "msg": "x"}));
        let ev = line_to_event(&sample, r#"{"id":"abc-123","msg":"x"}"#, 0, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(ev.id, "abc-123");
    }

    #[test]
    fn id_hash_es_determinista_para_misma_linea_y_offset() {
        let sample = obj(json!({"msg": "sin id"}));
        let raw = r#"{"msg":"sin id"}"#;
        let a = line_to_event(&sample, raw, 42, &no_overrides(), "jsonl:/tmp/a.log");
        let b = line_to_event(&sample, raw, 42, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(a.id, b.id);
        assert!(!a.id.is_empty());
    }

    #[test]
    fn sin_tiempo_valido_at_epoch_cero_y_at_vacio() {
        let sample = obj(json!({"msg": "sin tiempo"}));
        let ev = line_to_event(&sample, r#"{"msg":"sin tiempo"}"#, 0, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(ev.at_epoch, 0);
        assert_eq!(ev.at, "");
    }

    #[test]
    fn title_cae_a_linea_cruda_recortada_sin_message_field() {
        let raw = format!(r#"{{"payload":"{}"}}"#, "x".repeat(300));
        let sample = obj(serde_json::from_str(&raw).unwrap());
        let ev = line_to_event(&sample, &raw, 0, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(ev.title.chars().count(), RAW_TITLE_MAX_CHARS);
    }

    #[test]
    fn entity_cae_a_actor_y_luego_a_unknown() {
        let sample_actor = obj(json!({"host": "h1", "msg": "x"}));
        let ev = line_to_event(&sample_actor, r#"{"host":"h1","msg":"x"}"#, 0, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(ev.touches[0].entity, "h1");

        let sample_none = obj(json!({"msg": "x"}));
        let ev2 = line_to_event(&sample_none, r#"{"msg":"x"}"#, 0, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(ev2.touches[0].entity, "unknown");
    }

    #[test]
    fn tiempo_se_resuelve_por_linea_no_globalmente() {
        let epoch_line = obj(json!({"ts": 1_704_067_200, "msg": "a"}));
        let iso_line = obj(json!({"@timestamp": "2024-01-01T00:00:05Z", "msg": "b"}));
        let ev1 = line_to_event(&epoch_line, r#"{"ts":1704067200,"msg":"a"}"#, 0, &no_overrides(), "jsonl:/tmp/a.log");
        let ev2 = line_to_event(&iso_line, r#"{"@timestamp":"2024-01-01T00:00:05Z","msg":"b"}"#, 40, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(ev1.at_epoch, 1_704_067_200);
        assert_eq!(ev2.at_epoch, 1_704_067_205);
    }
}
