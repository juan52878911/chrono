//! Mapeo de una línea JSON ya parseada a uno o varios `chrono_core::Event`.
//! Única implementación del mapeo; la usan tanto el cursor de streaming como
//! los tests.
//!
//! Dos modos:
//! - Sin `explode` (por defecto): 1 línea → 1 evento.
//! - Con `explode` (ruta a un array de la línea): 1 línea → N eventos, uno por
//!   elemento objeto del array (filtrado opcionalmente por `explode_where`).
//!
//! Resolución de campos con `explode`, "elemento-luego-raíz": cada selector
//! (`time_field`, `message_field`, ...; y los candidatos de autodetección) se
//! busca primero en el ELEMENTO y, si ahí falta, en el objeto RAÍZ de la línea.
//! Así `time_field = "timestamp"` sale de la raíz y `message_field = "name"`
//! del elemento. Los selectores pueden ser rutas (ver `path`).

use crate::config::{self, scalar_to_string, JsonlOverrides};
use crate::path::Scope;
use crate::simhash::{fnv1a64_line, simhash, tokenize};
use crate::timeutil::{epoch_to_iso8601, parse_time_value};
use chrono_core::{Actor, Event, Touch};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Tope de claves volcadas a `attrs` (evita inflar eventos con objetos JSON
/// muy anchos).
const MAX_ATTRS: usize = 30;

/// Longitud máxima del `title` cuando se recorta la línea cruda como
/// respaldo (sin `message_field`).
const RAW_TITLE_MAX_CHARS: usize = 200;

/// Campos candidatos a `id` explícito, en orden de prioridad (solo sin
/// `explode`; con `explode` el id se deriva del hash, ver `line_to_events`).
const ID_FIELDS: &[&str] = &["id", "_id", "uuid"];

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

/// `id` de un evento expandido: `H(H(línea, offset), idx)`, es decir, el hash
/// de línea+offset de siempre y, sobre sus 8 bytes, el índice del elemento en
/// el array ORIGINAL (sin filtrar). Determinista, único entre los N eventos
/// de una misma línea (el índice difiere) y entre líneas (difiere el offset),
/// sin límite artificial en `idx` (no se multiplica ni se concatena).
/// Se ignora a propósito el `id` propio del elemento (p.ej. `toolu_…`): un
/// mismo item repetido en líneas distintas debe seguir siendo un evento
/// distinto; el `id` del elemento queda disponible en `attrs`.
fn exploded_id(raw_line: &str, offset: u64, idx: usize) -> String {
    let base = fnv1a64_line(raw_line.as_bytes(), offset);
    format!("{:016x}", fnv1a64_line(&base.to_le_bytes(), idx as u64))
}

/// Valor de un campo configurado como escalar, o "" si el campo no está
/// configurado, no existe en el ámbito, o no es escalar.
fn field_value(scope: &Scope<'_>, field: &str) -> String {
    if field.is_empty() {
        return String::new();
    }
    scope.get(field).and_then(scalar_to_string).unwrap_or_default()
}

/// Recorta `s` a lo sumo `max_chars` caracteres (respetando fronteras UTF-8).
fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Escalares de primer nivel de `obj` como pares ordenados (orden natural de
/// `serde_json::Map`).
fn scalars(obj: &Map<String, Value>) -> impl Iterator<Item = (String, String)> + '_ {
    obj.iter().filter_map(|(k, v)| scalar_to_string(v).map(|s| (k.clone(), s)))
}

/// Vuelca hasta `MAX_ATTRS` escalares a un `BTreeMap` determinista. Sin
/// elemento: los de la raíz. Con elemento: primero los del ELEMENTO (tienen
/// prioridad) y después, hasta completar el tope, los de la raíz cuya clave no
/// choque con una ya presente (p.ej. `type` del elemento gana a `type` de la
/// raíz).
fn scalar_attrs(scope: &Scope<'_>) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    if let Some(elem) = scope.elem {
        out.extend(scalars(elem).take(MAX_ATTRS));
    }
    let remaining = MAX_ATTRS.saturating_sub(out.len());
    let extra: Vec<(String, String)> = scalars(scope.root).filter(|(k, _)| !out.contains_key(k)).take(remaining).collect();
    out.extend(extra);
    out
}

/// Convierte una línea JSONL ya parseada (`obj`, `raw_line` sin salto final,
/// `offset` = posición en bytes del inicio de la línea en el fichero) en sus
/// `Event`s, según `ov` (overrides globales de `SourceConfig.options`) y
/// `source_id` ("jsonl:<ruta absoluta>").
///
/// Sin `ov.explode`: exactamente un evento. Con `ov.explode`: uno por elemento
/// que expande, en el orden del array; vacío si la ruta falta, no es un array
/// o ningún elemento pasa el filtro.
///
/// El campo efectivo de cada dimensión (tiempo, mensaje, nivel, entidad,
/// actor) se resuelve POR EVENTO: si hay override se usa siempre; si no, el
/// primer candidato presente en el ámbito concreto (los logs JSONL reales
/// mezclan formas de un logger a otro, p.ej. `ts` epoch en unas líneas y
/// `@timestamp` ISO en otras).
pub fn line_to_events(obj: &Map<String, Value>, raw_line: &str, offset: u64, ov: &JsonlOverrides, source_id: &str) -> Vec<Event> {
    match &ov.explode {
        None => {
            let id = compute_id(obj, raw_line, offset);
            vec![build_event(&Scope::root_only(obj), id, raw_line, ov, source_id)]
        }
        Some(spec) => spec
            .elements(obj)
            .into_iter()
            .map(|(idx, elem)| {
                let scope = Scope { elem: Some(elem), root: obj };
                build_event(&scope, exploded_id(raw_line, offset, idx), raw_line, ov, source_id)
            })
            .collect(),
    }
}

fn build_event(scope: &Scope<'_>, id: String, raw_line: &str, ov: &JsonlOverrides, source_id: &str) -> Event {
    let time_field = config::resolve_field_in(ov.time_field.as_deref(), config::TIME_FIELDS, scope);
    // Documentado en el encargo: sin tiempo válido, at_epoch=0 y at="" — el
    // evento entra igual (no se descarta por falta de tiempo). Se distingue
    // "no había tiempo" de "el tiempo era exactamente epoch 0" mirando si el
    // parseo tuvo éxito, no solo el valor resultante.
    let time_value = if time_field.is_empty() { None } else { scope.get(&time_field).and_then(parse_time_value) };
    let at_epoch = time_value.unwrap_or(0);
    let at = if time_value.is_some() { epoch_to_iso8601(at_epoch) } else { String::new() };

    let message_field = config::resolve_field_in(ov.message_field.as_deref(), config::MESSAGE_FIELDS, scope);
    let message = field_value(scope, &message_field);
    let title = if !message.is_empty() {
        message
    } else {
        // Respaldo: la línea cruda; con `explode`, el elemento serializado
        // (la línea entera sería idéntica para los N eventos).
        match scope.elem {
            Some(elem) => truncate_chars(&serde_json::to_string(elem).unwrap_or_default(), RAW_TITLE_MAX_CHARS),
            None => truncate_chars(raw_line.trim(), RAW_TITLE_MAX_CHARS),
        }
    };

    let level_field = config::resolve_field_in(ov.level_field.as_deref(), config::LEVEL_FIELDS, scope);
    let level = field_value(scope, &level_field).to_uppercase();

    let actor_field = config::resolve_field_in(ov.actor_field.as_deref(), config::ACTOR_FIELDS, scope);
    let actor_value = field_value(scope, &actor_field);
    let actor = Actor { name: actor_value.clone(), key: actor_value.clone() };

    let entity_field = config::resolve_field_in(ov.entity_field.as_deref(), config::ENTITY_FIELDS, scope);
    let entity_value = field_value(scope, &entity_field);
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
        attrs: scalar_attrs(scope),
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

    /// Sin `explode` siempre hay exactamente un evento por línea.
    fn line_to_event(obj: &Map<String, Value>, raw: &str, offset: u64, ov: &JsonlOverrides, source_id: &str) -> Event {
        let mut evs = line_to_events(obj, raw, offset, ov, source_id);
        assert_eq!(evs.len(), 1);
        evs.remove(0)
    }

    #[test]
    fn mapea_campos_basicos() {
        let sample = obj(json!({"ts": 1_704_067_200, "msg": "hola", "service": "api", "level": "info"}));
        let ev = line_to_event(&sample, r#"{"ts":1704067200,"msg":"hola","service":"api","level":"info"}"#, 0, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!(ev.at_epoch, 1_704_067_200_000);
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
        assert_eq!(ev1.at_epoch, 1_704_067_200_000);
        assert_eq!(ev2.at_epoch, 1_704_067_205_000);
    }

    fn ov_with(pairs: &[(&str, &str)]) -> JsonlOverrides {
        let options = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        config::overrides_from(&chrono_core::SourceConfig { options })
    }

    const CLAUDE_LINE: &str = r#"{"timestamp":"2024-01-01T00:00:00.100Z","type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"toolu_1","name":"Read"},{"type":"tool_use","id":"toolu_2","name":"Bash"}]}}"#;

    fn claude_ov() -> JsonlOverrides {
        ov_with(&[
            ("explode", "message.content"),
            ("explode_where", "type=tool_use"),
            ("time_field", "timestamp"),
            ("entity_field", "name"),
            ("message_field", "name"),
        ])
    }

    #[test]
    fn ruta_anidada_en_entity_field() {
        let raw = r#"{"message":{"role":"user"},"ts":1}"#;
        let sample = obj(serde_json::from_str(raw).unwrap());
        let ev = line_to_event(&sample, raw, 0, &ov_with(&[("entity_field", "message.role")]), "jsonl:/tmp/a.log");
        assert_eq!(ev.touches[0].entity, "user");
        assert_eq!(ev.at_epoch, 1_000);
    }

    #[test]
    fn indice_de_array_en_ruta() {
        let raw = r#"{"data":[{"id":"a"},{"id":"b"}],"ts":1}"#;
        let sample = obj(serde_json::from_str(raw).unwrap());
        let ev = line_to_event(&sample, raw, 0, &ov_with(&[("entity_field", "data.1.id")]), "jsonl:/tmp/a.log");
        assert_eq!(ev.touches[0].entity, "b");
    }

    #[test]
    fn autodeteccion_acepta_candidatos_de_una_palabra_sin_cambios() {
        let sample = obj(json!({"ts": 5, "msg": "m", "service": "api"}));
        let ev = line_to_event(&sample, "{}", 0, &no_overrides(), "jsonl:/tmp/a.log");
        assert_eq!((ev.at_epoch, ev.title.as_str(), ev.touches[0].entity.as_str()), (5_000, "m", "api"));
    }

    #[test]
    fn explode_con_where_da_un_evento_por_tool_use() {
        let sample = obj(serde_json::from_str(CLAUDE_LINE).unwrap());
        let evs = line_to_events(&sample, CLAUDE_LINE, 10, &claude_ov(), "jsonl:/tmp/a.log");
        assert_eq!(evs.len(), 2, "el item `text` no expande");
        let entities: Vec<&str> = evs.iter().map(|e| e.touches[0].entity.as_str()).collect();
        assert_eq!(entities, ["Read", "Bash"]);
        assert_eq!(evs[0].title, "Read");
        // Tiempo de la RAÍZ (elemento-luego-raíz), en ms.
        assert!(evs.iter().all(|e| e.at_epoch == 1_704_067_200_100));
        assert_eq!(evs[0].at, "2024-01-01T00:00:00.100Z");
        assert_ne!(evs[0].id, evs[1].id);
    }

    #[test]
    fn explode_attrs_priorizan_elemento_sobre_raiz() {
        let sample = obj(serde_json::from_str(CLAUDE_LINE).unwrap());
        let evs = line_to_events(&sample, CLAUDE_LINE, 0, &claude_ov(), "jsonl:/tmp/a.log");
        assert_eq!(evs[0].attrs["type"], "tool_use", "gana el elemento a la raíz");
        assert_eq!(evs[0].attrs["id"], "toolu_1");
        assert_eq!(evs[0].attrs["timestamp"], "2024-01-01T00:00:00.100Z", "escalar de la raíz sin choque");
    }

    #[test]
    fn explode_ids_deterministas_y_dependen_de_offset_e_indice() {
        let sample = obj(serde_json::from_str(CLAUDE_LINE).unwrap());
        let a = line_to_events(&sample, CLAUDE_LINE, 10, &claude_ov(), "s");
        let b = line_to_events(&sample, CLAUDE_LINE, 10, &claude_ov(), "s");
        let c = line_to_events(&sample, CLAUDE_LINE, 11, &claude_ov(), "s");
        assert_eq!(a.iter().map(|e| &e.id).collect::<Vec<_>>(), b.iter().map(|e| &e.id).collect::<Vec<_>>());
        assert_ne!(a[0].id, c[0].id);
        // El índice es el del array original: Read=1, Bash=2 (aunque el filtro quite el 0).
        let sin_filtro = line_to_events(&sample, CLAUDE_LINE, 10, &ov_with(&[("explode", "message.content")]), "s");
        assert_eq!(sin_filtro.len(), 3);
        assert_eq!(sin_filtro[1].id, a[0].id);
        assert_eq!(sin_filtro[2].id, a[1].id);
    }

    #[test]
    fn explode_sin_array_no_produce_eventos() {
        let sample = obj(json!({"ts": 1, "message": {"content": "texto"}}));
        assert!(line_to_events(&sample, "{}", 0, &claude_ov(), "s").is_empty());
        let sin_message = obj(json!({"ts": 1}));
        assert!(line_to_events(&sin_message, "{}", 0, &claude_ov(), "s").is_empty());
    }

    #[test]
    fn explode_omite_elementos_no_objeto() {
        let sample = obj(json!({"xs": [1, "a", {"k": "v"}, null]}));
        let evs = line_to_events(&sample, "{}", 0, &ov_with(&[("explode", "xs[]")]), "s");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].attrs["k"], "v");
    }
}
