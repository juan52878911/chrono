//! Mapeo de una fila CSV ya partida en campos (`parse::parse_csv_line`) a un
//! `chrono_core::Event`. Análogo a `chrono-source-jsonl::record`, pero aquí
//! las columnas ya vienen resueltas a índices fijos (`config::ResolvedColumns`)
//! porque la cabecera es la misma para todo el fichero (no hay heterogeneidad
//! fila a fila como en logs JSONL).

use crate::config::ResolvedColumns;
use crate::simhash::{fnv1a64_line, simhash, tokenize};
use crate::timeutil::{epoch_to_iso8601, parse_time_str};
use chrono_core::{Actor, Event, Touch};
use std::collections::BTreeMap;

/// Tope de columnas volcadas a `attrs` (evita inflar eventos con CSV muy anchos).
const MAX_ATTRS: usize = 30;

/// Longitud máxima del `title` cuando se recorta la línea cruda como
/// respaldo (sin `message_column`).
const RAW_TITLE_MAX_CHARS: usize = 200;

/// Celda de `fields` en la posición `idx`, o "" si `idx` es `None` o cae
/// fuera de rango (fila más corta que la cabecera: CSV "irregular").
fn field_at(fields: &[String], idx: Option<usize>) -> &str {
    idx.and_then(|i| fields.get(i)).map(String::as_str).unwrap_or("")
}

/// `id` determinista: la celda de la columna id si existe y no está vacía,
/// o si no, FNV-1a 64 de (bytes de la línea cruda + offset) en hex — igual
/// que `chrono-source-jsonl`.
fn compute_id(fields: &[String], cols: &ResolvedColumns, raw_line: &str, offset: u64) -> String {
    let id_value = field_at(fields, cols.id);
    if !id_value.is_empty() {
        return id_value.to_string();
    }
    format!("{:016x}", fnv1a64_line(raw_line.as_bytes(), offset))
}

/// Recorta `s` a lo sumo `max_chars` caracteres (respetando fronteras UTF-8).
fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Vuelca hasta `MAX_ATTRS` columnas (nombre de cabecera -> celda) en un
/// `BTreeMap` determinista. Columnas sin nombre (cabecera vacía en esa
/// posición) se omiten: no hay clave con la que guardarlas. Si la fila trae
/// menos campos que la cabecera, las columnas de más allá simplemente no
/// aparecen (CSV "irregular": limitación aceptada de v1).
fn row_attrs(header: &[String], fields: &[String]) -> BTreeMap<String, String> {
    header
        .iter()
        .zip(fields.iter())
        .filter(|(h, _)| !h.is_empty())
        .map(|(h, v)| (h.clone(), v.clone()))
        .take(MAX_ATTRS)
        .collect()
}

/// Convierte una fila CSV ya partida en campos (`fields`, en el mismo orden
/// que `header`) en un `Event`. `raw_line` es la línea física cruda (sin
/// salto final, usada como respaldo de `title` y como semilla del `id` hash);
/// `offset` es la posición en bytes del inicio de la línea en el fichero;
/// `source_id` es "csv:<ruta absoluta>".
pub fn row_to_event(
    fields: &[String],
    header: &[String],
    cols: &ResolvedColumns,
    raw_line: &str,
    offset: u64,
    source_id: &str,
) -> Event {
    let id = compute_id(fields, cols, raw_line, offset);

    let time_value = field_at(fields, cols.time);
    // Documentado en el encargo: sin tiempo válido, at_epoch=0 y at="" — el
    // evento entra igual (no se descarta por falta de tiempo).
    let parsed_time = if time_value.is_empty() { None } else { parse_time_str(time_value) };
    let at_epoch = parsed_time.unwrap_or(0);
    let at = if parsed_time.is_some() { epoch_to_iso8601(at_epoch) } else { String::new() };

    let message = field_at(fields, cols.message);
    let title = if !message.is_empty() { message.to_string() } else { truncate_chars(raw_line.trim(), RAW_TITLE_MAX_CHARS) };

    let level = field_at(fields, cols.level).to_uppercase();

    let actor_value = field_at(fields, cols.actor).to_string();
    let actor = Actor { name: actor_value.clone(), key: actor_value.clone() };

    let entity_value = field_at(fields, cols.entity).to_string();
    let entity = if !entity_value.is_empty() {
        entity_value
    } else if !actor_value.is_empty() {
        actor_value.clone()
    } else {
        "unknown".to_string()
    };

    let toks = tokenize(&title);
    let hash = simhash(&toks);

    Event {
        id,
        source_id: source_id.to_string(),
        kind: "row".to_string(),
        at,
        at_epoch,
        actor,
        title,
        body: String::new(),
        level,
        attrs: row_attrs(header, fields),
        touches: vec![Touch { entity, entity_type: "row-source".to_string(), weight: 1, attrs: BTreeMap::new() }],
        links: Vec::new(),
        simhash: hash,
        is_bulk: false, // un único touch por fila: nunca es "bulk" en CSV.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, CsvOverrides};

    fn header(cols: &[&str]) -> Vec<String> {
        cols.iter().map(|s| s.to_string()).collect()
    }

    fn fields(cols: &[&str]) -> Vec<String> {
        cols.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn mapea_campos_basicos() {
        let h = header(&["ts", "msg", "service", "level"]);
        let cols = config::resolve_columns(&h, &CsvOverrides::default());
        let f = fields(&["1704067200", "hola", "api", "info"]);
        let ev = row_to_event(&f, &h, &cols, "1704067200,hola,api,info", 0, "csv:/tmp/a.csv");
        assert_eq!(ev.at_epoch, 1_704_067_200);
        assert_eq!(ev.at, "2024-01-01T00:00:00Z");
        assert_eq!(ev.title, "hola");
        assert_eq!(ev.level, "INFO");
        assert_eq!(ev.touches[0].entity, "api");
        assert_eq!(ev.touches[0].entity_type, "row-source");
        assert_eq!(ev.kind, "row");
    }

    #[test]
    fn id_explicito_si_hay_columna_id_no_vacia() {
        let h = header(&["id", "msg"]);
        let cols = config::resolve_columns(&h, &CsvOverrides::default());
        let f = fields(&["evt-42", "x"]);
        let ev = row_to_event(&f, &h, &cols, "evt-42,x", 0, "csv:/tmp/a.csv");
        assert_eq!(ev.id, "evt-42");
    }

    #[test]
    fn id_hash_determinista_para_misma_linea_y_offset() {
        let h = header(&["msg"]);
        let cols = config::resolve_columns(&h, &CsvOverrides::default());
        let f = fields(&["sin id"]);
        let a = row_to_event(&f, &h, &cols, "sin id", 42, "csv:/tmp/a.csv");
        let b = row_to_event(&f, &h, &cols, "sin id", 42, "csv:/tmp/a.csv");
        assert_eq!(a.id, b.id);
        assert!(!a.id.is_empty());
    }

    #[test]
    fn sin_tiempo_valido_at_epoch_cero_y_at_vacio() {
        let h = header(&["msg"]);
        let cols = config::resolve_columns(&h, &CsvOverrides::default());
        let f = fields(&["sin tiempo"]);
        let ev = row_to_event(&f, &h, &cols, "sin tiempo", 0, "csv:/tmp/a.csv");
        assert_eq!(ev.at_epoch, 0);
        assert_eq!(ev.at, "");
    }

    #[test]
    fn title_cae_a_linea_cruda_recortada_sin_message_column() {
        let h = header(&["payload"]);
        let cols = config::resolve_columns(&h, &CsvOverrides::default());
        let value = "x".repeat(300);
        let f = fields(&[&value]);
        let raw = value.clone();
        let ev = row_to_event(&f, &h, &cols, &raw, 0, "csv:/tmp/a.csv");
        assert_eq!(ev.title.chars().count(), RAW_TITLE_MAX_CHARS);
    }

    #[test]
    fn entity_cae_a_actor_y_luego_a_unknown() {
        let h = header(&["host", "msg"]);
        let cols = config::resolve_columns(&h, &CsvOverrides::default());
        let f = fields(&["h1", "x"]);
        let ev = row_to_event(&f, &h, &cols, "h1,x", 0, "csv:/tmp/a.csv");
        assert_eq!(ev.touches[0].entity, "h1");

        let h2 = header(&["msg"]);
        let cols2 = config::resolve_columns(&h2, &CsvOverrides::default());
        let f2 = fields(&["x"]);
        let ev2 = row_to_event(&f2, &h2, &cols2, "x", 0, "csv:/tmp/a.csv");
        assert_eq!(ev2.touches[0].entity, "unknown");
    }
}
