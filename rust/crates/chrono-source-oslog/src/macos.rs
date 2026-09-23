//! Parseo de la salida de `log show --style json` (macOS) a `chrono_core::Event`.
//! Función pura sobre `&str`, sin invocar el binario `log`: así se puede
//! testear con una muestra fija y se reutiliza igual desde `open()` (que sí
//! shellea) que desde los tests.
//!
//! Formato de entrada: UN ARRAY JSON `[{...}, {...}, ...]` (no JSONL). Cada
//! registro trae, entre otros: `timestamp` ("2026-09-22 23:12:11.497633-0500",
//! parseable por `timeutil::parse_time_str`), `eventMessage`, `messageType`
//! ("Default"/"Info"/"Debug"/"Error"/"Fault"), `subsystem`,
//! `processImagePath`, `processID`, `category`.

use crate::simhash::{simhash, tokenize};
use crate::timeutil::{epoch_to_iso8601, parse_time_str};
use crate::util::{as_string, basename, compute_id, truncate_chars};
use chrono_core::{Actor, CoreError, Event, Result as CoreResult, Touch};
use serde_json::Value;
use std::collections::BTreeMap;

/// Longitud máxima del `title` (mensaje del log), ver encargo §4.
const TITLE_MAX_CHARS: usize = 500;

/// Convierte el array JSON crudo de `log show --style json` en eventos. Un
/// registro que no es un objeto JSON se ignora (defensivo: el volumen está
/// acotado por la ventana temporal del comando, pero un registro corrupto no
/// debe abortar toda la ingesta). El array en sí, si no es JSON válido o no
/// es un array, sí es un error duro (la salida del comando no es la
/// esperada).
pub(crate) fn parse_macos(json_text: &str, source_id: &str) -> CoreResult<Vec<Event>> {
    let value: Value = serde_json::from_str(json_text)
        .map_err(|e| CoreError::Other(format!("log show --style json: salida no es JSON válido: {e}")))?;
    let Value::Array(records) = value else {
        return Err(CoreError::Other("log show --style json: se esperaba un array JSON en la salida".to_string()));
    };

    let mut events = Vec::with_capacity(records.len());
    for (idx, rec) in records.iter().enumerate() {
        let Value::Object(obj) = rec else { continue };

        let ts_raw = as_string(obj.get("timestamp"));
        let time_epoch = parse_time_str(&ts_raw);
        // Sin tiempo válido, el evento entra igual: at_epoch=0, at="".
        let at_epoch = time_epoch.unwrap_or(0);
        let at = if time_epoch.is_some() { epoch_to_iso8601(at_epoch) } else { String::new() };

        let message = as_string(obj.get("eventMessage"));
        let title = truncate_chars(&message, TITLE_MAX_CHARS);

        let level = as_string(obj.get("messageType")).to_uppercase();

        let process_path = as_string(obj.get("processImagePath"));
        let process_name = basename(&process_path);

        let subsystem = as_string(obj.get("subsystem"));
        let entity = if !subsystem.is_empty() {
            subsystem.clone()
        } else if !process_name.is_empty() {
            process_name.clone()
        } else {
            "system".to_string()
        };

        let pid = as_string(obj.get("processID"));
        let category = as_string(obj.get("category"));

        let mut attrs = BTreeMap::new();
        if !subsystem.is_empty() {
            attrs.insert("subsystem".to_string(), subsystem);
        }
        if !category.is_empty() {
            attrs.insert("category".to_string(), category);
        }
        if !pid.is_empty() {
            attrs.insert("pid".to_string(), pid);
        }
        if !process_name.is_empty() {
            attrs.insert("process".to_string(), process_name.clone());
        }

        let id = compute_id(&ts_raw, &message, idx);
        let hash = simhash(&tokenize(&title));

        events.push(Event {
            id,
            source_id: source_id.to_string(),
            kind: "log".to_string(),
            at,
            at_epoch,
            actor: Actor { name: process_name.clone(), key: process_name },
            title,
            body: String::new(),
            level,
            attrs,
            touches: vec![Touch { entity, entity_type: "log-source".to_string(), weight: 1, attrs: BTreeMap::new() }],
            links: Vec::new(),
            simhash: hash,
            is_bulk: false,
        });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3 registros de muestra: uno normal, uno sin `subsystem` (cae a
    /// proceso) y uno con timestamp corrupto (entra igual con epoch 0).
    const SAMPLE: &str = r#"[
        {
            "timestamp": "2026-09-22 23:12:11.497633-0500",
            "eventMessage": "Conexion establecida",
            "messageType": "Info",
            "subsystem": "com.apple.network",
            "processImagePath": "/usr/libexec/networkd",
            "processID": 412,
            "category": "connection"
        },
        {
            "timestamp": "2026-09-22 23:12:12.001000-0500",
            "eventMessage": "Fallo al resolver DNS",
            "messageType": "Error",
            "subsystem": "",
            "processImagePath": "/usr/sbin/mDNSResponder",
            "processID": 88,
            "category": "dns"
        },
        {
            "timestamp": "no-es-tiempo",
            "eventMessage": "Evento sin tiempo valido",
            "messageType": "Debug",
            "subsystem": "com.apple.test",
            "processImagePath": "/bin/test",
            "processID": 1,
            "category": "misc"
        }
    ]"#;

    #[test]
    fn parsea_tres_registros() {
        let events = parse_macos(SAMPLE, "oslog:macos").unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn at_epoch_respeta_el_offset_de_zona() {
        let events = parse_macos(SAMPLE, "oslog:macos").unwrap();
        // "2026-09-22 23:12:11.497633-0500" en UTC es "2026-09-23T04:12:11Z".
        let expected = crate::timeutil::parse_rfc3339("2026-09-23T04:12:11Z").unwrap();
        assert_eq!(events[0].at_epoch, expected);
        assert_eq!(events[0].at, "2026-09-23T04:12:11Z");
    }

    #[test]
    fn title_level_actor_y_entity_desde_subsystem() {
        let events = parse_macos(SAMPLE, "oslog:macos").unwrap();
        let e0 = &events[0];
        assert_eq!(e0.title, "Conexion establecida");
        assert_eq!(e0.level, "INFO");
        assert_eq!(e0.actor.name, "networkd");
        assert_eq!(e0.touches[0].entity, "com.apple.network");
        assert_eq!(e0.touches[0].entity_type, "log-source");
        assert_eq!(e0.attrs.get("pid").map(String::as_str), Some("412"));
    }

    #[test]
    fn entity_cae_al_proceso_sin_subsystem() {
        let events = parse_macos(SAMPLE, "oslog:macos").unwrap();
        let e1 = &events[1];
        assert_eq!(e1.level, "ERROR");
        assert_eq!(e1.touches[0].entity, "mDNSResponder");
        assert_eq!(e1.actor.name, "mDNSResponder");
    }

    #[test]
    fn timestamp_invalido_entra_con_epoch_cero() {
        let events = parse_macos(SAMPLE, "oslog:macos").unwrap();
        let e2 = &events[2];
        assert_eq!(e2.at_epoch, 0);
        assert_eq!(e2.at, "");
        assert_eq!(e2.title, "Evento sin tiempo valido");
    }

    #[test]
    fn ids_deterministas_entre_ejecuciones() {
        let a = parse_macos(SAMPLE, "oslog:macos").unwrap();
        let b = parse_macos(SAMPLE, "oslog:macos").unwrap();
        for (ea, eb) in a.iter().zip(b.iter()) {
            assert_eq!(ea.id, eb.id);
            assert!(!ea.id.is_empty());
        }
    }

    #[test]
    fn array_invalido_es_error() {
        assert!(parse_macos("no es json", "oslog:macos").is_err());
        assert!(parse_macos(r#"{"no":"array"}"#, "oslog:macos").is_err());
    }
}
