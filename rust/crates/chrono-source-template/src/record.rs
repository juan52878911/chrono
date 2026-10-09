//! Mapeo de las capturas de una línea (ya casada con la plantilla) a un
//! `chrono_core::Event`, según el ROL de cada marcador.

use crate::matcher::Plantilla;
use crate::simhash::{fnv1a64_line, simhash, tokenize};
use crate::timeutil::{epoch_to_iso8601, parse_time_str};
use chrono_core::{Actor, Event, Touch};
use std::collections::BTreeMap;

/// Tope de caracteres de la línea cruda usada como título de respaldo.
const TITULO_MAX: usize = 200;

const ALIAS_TIEMPO: &[&str] = &["time", "ts", "timestamp"];
const ALIAS_MENSAJE: &[&str] = &["message", "msg", "text"];
const ALIAS_NIVEL: &[&str] = &["level", "severity", "lvl"];
const ALIAS_ENTIDAD: &[&str] = &["entity", "service", "source", "logger"];
const ALIAS_ACTOR: &[&str] = &["actor", "host", "user"];

/// Qué marcador de la plantilla cumple cada rol (nombre exacto del marcador).
#[derive(Debug, Clone, Default)]
pub struct Roles {
    pub tiempo: Option<String>,
    pub mensaje: Option<String>,
    pub nivel: Option<String>,
    pub entidad: Option<String>,
    pub actor: Option<String>,
}

/// Resuelve un rol: override explícito (`<clave>_field`, debe existir en la
/// plantilla) o, si no, el primer marcador (orden de plantilla) cuyo nombre en
/// minúsculas esté en `alias`.
fn resolver(
    nombres: &[&str],
    opts: &BTreeMap<String, String>,
    clave: &str,
    alias: &[&str],
) -> Result<Option<String>, String> {
    if let Some(v) = opts.get(clave).map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return if nombres.contains(&v) {
            Ok(Some(v.to_string()))
        } else {
            Err(format!("la opción '{clave}' = \"{v}\" no es un marcador de la plantilla"))
        };
    }
    Ok(nombres
        .iter()
        .find(|n| alias.contains(&n.to_lowercase().as_str()))
        .map(|n| n.to_string()))
}

impl Roles {
    pub fn resolve(p: &Plantilla, opts: &BTreeMap<String, String>) -> Result<Self, String> {
        let n = p.nombres();
        Ok(Self {
            tiempo: resolver(&n, opts, "time_field", ALIAS_TIEMPO)?,
            mensaje: resolver(&n, opts, "message_field", ALIAS_MENSAJE)?,
            nivel: resolver(&n, opts, "level_field", ALIAS_NIVEL)?,
            entidad: resolver(&n, opts, "entity_field", ALIAS_ENTIDAD)?,
            actor: resolver(&n, opts, "actor_field", ALIAS_ACTOR)?,
        })
    }

    fn usa(&self, nombre: &str) -> bool {
        [&self.tiempo, &self.mensaje, &self.nivel, &self.entidad, &self.actor]
            .iter()
            .any(|r| r.as_deref() == Some(nombre))
    }
}

fn valor<'a>(caps: &[(&'a str, &'a str)], nombre: &Option<String>) -> &'a str {
    match nombre {
        Some(n) => caps.iter().find(|(k, _)| k == n).map(|(_, v)| *v).unwrap_or(""),
        None => "",
    }
}

/// Convierte una línea casada en `Event`. `raw_line` va sin salto final y
/// `offset` es la posición en bytes de su inicio en el fichero (entra en el
/// `id` para que líneas idénticas en distintos puntos no colisionen).
pub fn line_to_event(caps: &[(&str, &str)], roles: &Roles, raw_line: &str, offset: u64, source_id: &str) -> Event {
    let id = format!("{:016x}", fnv1a64_line(raw_line.as_bytes(), offset));

    let (at_epoch, at) = match parse_time_str(valor(caps, &roles.tiempo)) {
        Some(ms) => (ms, epoch_to_iso8601(ms)),
        None => (0, String::new()),
    };

    let msg = valor(caps, &roles.mensaje);
    let title = if msg.is_empty() {
        raw_line.trim().chars().take(TITULO_MAX).collect::<String>()
    } else {
        msg.to_string()
    };

    let level = valor(caps, &roles.nivel).to_uppercase();

    let actor_v = valor(caps, &roles.actor);
    let actor = Actor { name: actor_v.to_string(), key: actor_v.to_string() };

    let ent_v = valor(caps, &roles.entidad);
    let entity = if !ent_v.is_empty() {
        ent_v.to_string()
    } else if !actor_v.is_empty() {
        actor_v.to_string()
    } else {
        "unknown".to_string()
    };

    let mut attrs = BTreeMap::new();
    for (k, v) in caps {
        if !roles.usa(k) && !v.is_empty() {
            attrs.insert(k.to_string(), v.to_string());
        }
    }

    let simhash = simhash(&tokenize(&title));

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
        attrs,
        touches: vec![Touch {
            entity,
            entity_type: "log-source".to_string(),
            weight: 1,
            attrs: BTreeMap::new(),
        }],
        links: Vec::new(),
        simhash,
        is_bulk: false,
    }
}
