//! Mapeo de una línea ya parseada por el preset activo (`preset::ParsedLine`)
//! a un `chrono_core::Event`. Única implementación del mapeo; la usan tanto
//! el cursor de streaming como los tests.

use crate::preset::ParsedLine;
use crate::simhash::{fnv1a64_line, simhash, tokenize};
use crate::timeutil::epoch_to_iso8601;
use chrono_core::{Actor, Event, Touch};
use std::collections::BTreeMap;

/// Deriva el nivel nativo de un status HTTP: 2xx/3xx -> INFO (respuesta
/// normal, incluidas redirecciones); 4xx -> WARN (error de cliente); 5xx ->
/// ERROR (error de servidor). Cualquier otra cosa (1xx, o status no
/// numérico) -> "" (sin nivel nativo claro). Decisión documentada en el
/// encargo, no es un estándar universal pero es la lectura operativa más
/// común de un access log.
fn status_to_level(status: &str) -> String {
    let code: i32 = match status.parse() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    match code {
        200..=399 => "INFO",
        400..=499 => "WARN",
        500..=599 => "ERROR",
        _ => "",
    }
    .to_string()
}

fn insert_if_nonempty(m: &mut BTreeMap<String, String>, k: &str, v: &str) {
    if !v.is_empty() {
        m.insert(k.to_string(), v.to_string());
    }
}

/// Convierte una línea ya parseada (`parsed`, `raw_line` sin salto final,
/// `offset` = posición en bytes del inicio de la línea en el fichero) en un
/// `Event`. `id` siempre es FNV-1a 64 de (línea cruda + offset) en hex: a
/// diferencia de jsonl, ninguno de los dos presets de texto trae un campo de
/// id explícito reconocible de forma genérica.
pub fn line_to_event(parsed: &ParsedLine, raw_line: &str, offset: u64, source_id: &str) -> Event {
    let id = format!("{:016x}", fnv1a64_line(raw_line.as_bytes(), offset));

    match parsed {
        ParsedLine::Nginx(f) => {
            let at = epoch_to_iso8601(f.epoch);
            let level = status_to_level(&f.status);
            // method/path siempre no vacíos: `nginx::parse` los exige.
            let title = format!("{} {} {}", f.method, f.path, f.status);

            let mut attrs = BTreeMap::new();
            insert_if_nonempty(&mut attrs, "method", &f.method);
            insert_if_nonempty(&mut attrs, "path", &f.path);
            insert_if_nonempty(&mut attrs, "status", &f.status);
            insert_if_nonempty(&mut attrs, "bytes", &f.bytes);
            insert_if_nonempty(&mut attrs, "referer", &f.referer);
            insert_if_nonempty(&mut attrs, "user_agent", &f.user_agent);
            insert_if_nonempty(&mut attrs, "ip", &f.ip);

            let toks = tokenize(&title);
            let hash = simhash(&toks);

            Event {
                id,
                source_id: source_id.to_string(),
                kind: "log".to_string(),
                at,
                at_epoch: f.epoch,
                actor: Actor { name: f.ip.clone(), key: f.ip.clone() },
                title,
                body: String::new(),
                level,
                attrs,
                touches: vec![Touch {
                    entity: f.path.clone(),
                    entity_type: "endpoint".to_string(),
                    weight: 1,
                    attrs: BTreeMap::new(),
                }],
                links: Vec::new(),
                simhash: hash,
                is_bulk: false,
            }
        }
        ParsedLine::Syslog(f) => {
            let at = epoch_to_iso8601(f.epoch);
            // entity = TAG/APP (programa); si no vino (nil en RFC5424), cae al
            // host, y si tampoco hay host, a "unknown" (mismo patrón que jsonl).
            let entity = if !f.tag.is_empty() {
                f.tag.clone()
            } else if !f.host.is_empty() {
                f.host.clone()
            } else {
                "unknown".to_string()
            };

            let mut attrs = BTreeMap::new();
            insert_if_nonempty(&mut attrs, "host", &f.host);
            insert_if_nonempty(&mut attrs, "tag", &f.tag);
            insert_if_nonempty(&mut attrs, "pid", &f.pid);
            insert_if_nonempty(&mut attrs, "facility", &f.facility);
            insert_if_nonempty(&mut attrs, "severity", &f.severity);

            let toks = tokenize(&f.msg);
            let hash = simhash(&toks);

            Event {
                id,
                source_id: source_id.to_string(),
                kind: "log".to_string(),
                at,
                at_epoch: f.epoch,
                actor: Actor { name: f.host.clone(), key: f.host.clone() },
                title: f.msg.clone(),
                body: String::new(),
                level: f.level.clone(),
                attrs,
                touches: vec![Touch {
                    entity,
                    entity_type: "log-source".to_string(),
                    weight: 1,
                    attrs: BTreeMap::new(),
                }],
                links: Vec::new(),
                simhash: hash,
                is_bulk: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nginx::NginxFields;
    use crate::syslog::SyslogFields;

    #[test]
    fn nginx_status_a_level() {
        assert_eq!(status_to_level("200"), "INFO");
        assert_eq!(status_to_level("301"), "INFO");
        assert_eq!(status_to_level("404"), "WARN");
        assert_eq!(status_to_level("500"), "ERROR");
        assert_eq!(status_to_level("100"), "");
        assert_eq!(status_to_level("no-numero"), "");
    }

    #[test]
    fn nginx_evento_mapea_entity_actor_y_title() {
        let f = NginxFields {
            ip: "1.2.3.4".to_string(),
            user: String::new(),
            epoch: 1_000,
            method: "GET".to_string(),
            path: "/x".to_string(),
            status: "200".to_string(),
            bytes: "10".to_string(),
            referer: String::new(),
            user_agent: String::new(),
        };
        let ev = line_to_event(&ParsedLine::Nginx(f), "raw", 0, "textlog:/tmp/a.log");
        assert_eq!(ev.touches[0].entity, "/x");
        assert_eq!(ev.touches[0].entity_type, "endpoint");
        assert_eq!(ev.actor.name, "1.2.3.4");
        assert_eq!(ev.title, "GET /x 200");
        assert_eq!(ev.level, "INFO");
        assert_eq!(ev.kind, "log");
    }

    #[test]
    fn syslog_evento_mapea_entity_actor_y_title() {
        let f = SyslogFields {
            level: "ERROR".to_string(),
            facility: "1".to_string(),
            severity: "3".to_string(),
            host: "host1".to_string(),
            tag: "sshd".to_string(),
            pid: "42".to_string(),
            msg: "algo paso".to_string(),
            epoch: 5_000,
        };
        let ev = line_to_event(&ParsedLine::Syslog(f), "raw", 0, "textlog:/tmp/a.log");
        assert_eq!(ev.touches[0].entity, "sshd");
        assert_eq!(ev.touches[0].entity_type, "log-source");
        assert_eq!(ev.actor.name, "host1");
        assert_eq!(ev.title, "algo paso");
        assert_eq!(ev.level, "ERROR");
        assert_eq!(ev.attrs["pid"], "42");
    }

    #[test]
    fn id_es_determinista_para_misma_linea_y_offset() {
        let f = SyslogFields { host: "h".to_string(), tag: "t".to_string(), ..Default::default() };
        let a = line_to_event(&ParsedLine::Syslog(f.clone()), "raw", 7, "textlog:/tmp/a.log");
        let b = line_to_event(&ParsedLine::Syslog(f), "raw", 7, "textlog:/tmp/a.log");
        assert_eq!(a.id, b.id);
        assert!(!a.id.is_empty());
    }
}
