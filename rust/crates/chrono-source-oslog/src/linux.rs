//! Parseo de la salida de `journalctl -o json` (Linux) a `chrono_core::Event`.
//! Función pura sobre `&str`, sin invocar el binario `journalctl`.
//!
//! Formato de entrada: JSONL (un objeto JSON por línea, a diferencia del
//! array único de `log show` en macOS). Cada línea trae, entre otros:
//! `__REALTIME_TIMESTAMP` (microsegundos-epoch en texto), `MESSAGE`,
//! `PRIORITY` ("0".."7" syslog), `SYSLOG_IDENTIFIER` o `_SYSTEMD_UNIT`,
//! `_HOSTNAME`, `_PID`.

use crate::simhash::{simhash, tokenize};
use crate::timeutil::epoch_to_iso8601;
use crate::util::{as_string, compute_id, truncate_chars};
use chrono_core::{Actor, Event, Touch};
use serde_json::Value;
use std::collections::BTreeMap;

/// Longitud máxima del `title` (mensaje del log), ver encargo §4.
const TITLE_MAX_CHARS: usize = 500;

/// `__REALTIME_TIMESTAMP` viene en MICROsegundos desde epoch, como texto
/// (no es el mismo formato que acepta `timeutil::parse_time_str`, pensado
/// para segundos/milisegundos). `None` si no es un entero.
fn parse_realtime_us(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok().map(|us| us.div_euclid(1_000_000))
}

/// Prioridad syslog (0..7, texto) al nombre de nivel, tal como pide el
/// encargo: EMERG/ALERT/CRIT/ERROR/WARN/NOTICE/INFO/DEBUG. "" si no es
/// reconocida (0..7).
fn priority_to_level(p: &str) -> String {
    match p.trim() {
        "0" => "EMERG",
        "1" => "ALERT",
        "2" => "CRIT",
        "3" => "ERROR",
        "4" => "WARN",
        "5" => "NOTICE",
        "6" => "INFO",
        "7" => "DEBUG",
        _ => "",
    }
    .to_string()
}

/// Convierte la salida JSONL cruda de `journalctl -o json` en eventos. Una
/// línea vacía o no parseable como objeto JSON se descarta (igual criterio
/// que `chrono-source-jsonl`): no aborta la ingesta por un registro suelto.
pub(crate) fn parse_journald(jsonl_text: &str, source_id: &str) -> Vec<Event> {
    let mut events = Vec::new();
    for (idx, line) in jsonl_text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) else { continue };

        let ts_raw = as_string(obj.get("__REALTIME_TIMESTAMP"));
        let time_epoch = parse_realtime_us(&ts_raw);
        let at_epoch = time_epoch.unwrap_or(0);
        let at = if time_epoch.is_some() { epoch_to_iso8601(at_epoch) } else { String::new() };

        let message = as_string(obj.get("MESSAGE"));
        let title = truncate_chars(&message, TITLE_MAX_CHARS);

        let priority = as_string(obj.get("PRIORITY"));
        let level = priority_to_level(&priority);

        let identifier = as_string(obj.get("SYSLOG_IDENTIFIER"));
        let unit = as_string(obj.get("_SYSTEMD_UNIT"));
        let proc_name = if !identifier.is_empty() { identifier.clone() } else { unit.clone() };
        let entity = if !proc_name.is_empty() { proc_name.clone() } else { "system".to_string() };

        let hostname = as_string(obj.get("_HOSTNAME"));
        let pid = as_string(obj.get("_PID"));

        let mut attrs = BTreeMap::new();
        if !identifier.is_empty() {
            attrs.insert("identifier".to_string(), identifier);
        }
        if !unit.is_empty() {
            attrs.insert("unit".to_string(), unit);
        }
        if !hostname.is_empty() {
            attrs.insert("hostname".to_string(), hostname);
        }
        if !pid.is_empty() {
            attrs.insert("pid".to_string(), pid);
        }
        if !priority.is_empty() {
            attrs.insert("priority".to_string(), priority);
        }

        let id = compute_id(&ts_raw, &message, idx);
        let hash = simhash(&tokenize(&title));

        events.push(Event {
            id,
            source_id: source_id.to_string(),
            kind: "log".to_string(),
            at,
            at_epoch,
            actor: Actor { name: proc_name.clone(), key: proc_name },
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
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        r#"{"__REALTIME_TIMESTAMP": "1696000000000000", "MESSAGE": "Servicio iniciado", "PRIORITY": "6", "SYSLOG_IDENTIFIER": "sshd", "_HOSTNAME": "host1", "_PID": "1234"}"#,
        "\n",
        r#"{"__REALTIME_TIMESTAMP": "1696000005000000", "MESSAGE": "Fallo critico", "PRIORITY": "2", "_SYSTEMD_UNIT": "nginx.service", "_HOSTNAME": "host1", "_PID": "5678"}"#,
        "\n",
    );

    #[test]
    fn parsea_dos_lineas() {
        let events = parse_journald(SAMPLE, "oslog:linux");
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn microsegundos_a_epoch_segundos() {
        let events = parse_journald(SAMPLE, "oslog:linux");
        assert_eq!(events[0].at_epoch, 1_696_000_000);
        assert_eq!(events[1].at_epoch, 1_696_000_005);
    }

    #[test]
    fn prioridad_a_nivel() {
        let events = parse_journald(SAMPLE, "oslog:linux");
        assert_eq!(events[0].level, "INFO"); // PRIORITY 6
        assert_eq!(events[1].level, "CRIT"); // PRIORITY 2
    }

    #[test]
    fn entity_desde_identifier_o_unit() {
        let events = parse_journald(SAMPLE, "oslog:linux");
        assert_eq!(events[0].touches[0].entity, "sshd");
        assert_eq!(events[0].actor.name, "sshd");
        assert_eq!(events[1].touches[0].entity, "nginx.service");
        assert_eq!(events[1].actor.name, "nginx.service");
    }

    #[test]
    fn title_y_attrs() {
        let events = parse_journald(SAMPLE, "oslog:linux");
        assert_eq!(events[0].title, "Servicio iniciado");
        assert_eq!(events[0].attrs.get("pid").map(String::as_str), Some("1234"));
        assert_eq!(events[0].attrs.get("hostname").map(String::as_str), Some("host1"));
    }

    #[test]
    fn ids_deterministas_entre_ejecuciones() {
        let a = parse_journald(SAMPLE, "oslog:linux");
        let b = parse_journald(SAMPLE, "oslog:linux");
        for (ea, eb) in a.iter().zip(b.iter()) {
            assert_eq!(ea.id, eb.id);
        }
    }

    #[test]
    fn linea_no_json_se_descarta() {
        let text = "esto no es json\n{\"MESSAGE\": \"valido\", \"__REALTIME_TIMESTAMP\": \"1000000\"}\n";
        let events = parse_journald(text, "oslog:linux");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "valido");
    }
}
