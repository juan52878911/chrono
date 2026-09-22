//! Modelo de dominio general. Un `Event` es un registro temporal abstracto;
//! un commit git es UNA proyección de `Event` (ver el adaptador `source-git`).
//!
//! Convención del contrato (decisión del dueño): `id` (no `sha`) y `entity`
//! (no `path`). Los mapas usan `BTreeMap` para que el orden sea determinista.

use std::collections::BTreeMap;

/// Quién o qué origina el evento (autor, host, servicio, usuario).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Actor {
    pub name: String,
    /// Clave de unificación de identidades: email | host | nombre de servicio.
    pub key: String,
}

/// Una entidad afectada por el evento. `entity` es una clave JERÁRQUICA con "/"
/// ("src/bun.js/socket.zig", "api/users/{id}", "host-a/nginx"): eso hace que
/// hotspots/coupling/owners/churn funcionen igual para código, endpoints o hosts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Touch {
    pub entity: String,
    /// "file" | "endpoint" | "host" | "service" | "table"…
    pub entity_type: String,
    /// Magnitud: added+deleted en git; 1 en logs; lo que decida el adaptador.
    pub weight: i64,
    pub attrs: BTreeMap<String, String>,
}

/// Relación determinista con otro id (interno o externo).
/// rel: "ticket" | "reverts" | "parent" | "deploy-of" | "pr" | "trace".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Link {
    pub rel: String,
    pub target: String,
}

/// Etiqueta de clasificación multitarea (Level-0 reglas o Level-1 JEV).
/// `task`: "kind" | "is_fix" | "bug_category" | "severity" | "log_category".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Label {
    pub task: String,
    pub label: String,
    pub confidence: f64,
    /// "rules" | "jev" | "forge" — de dónde vino la etiqueta.
    pub source: String,
    /// Evidencia legible (top features o la regla que disparó).
    pub evidence: Vec<String>,
}

/// El registro temporal general.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Event {
    /// Único dentro de la fuente: sha | hash(línea+offset) | uuid.
    pub id: String,
    /// "git:<abs>" | "jsonl:<abs>" | "journald:<unit>".
    pub source_id: String,
    /// "commit" | "log" | "deploy" | "incident" | "release" | "row".
    pub kind: String,
    /// ISO-8601 SIEMPRE en UTC ("2026-08-30T10:00:00Z"). Para la salida.
    pub at: String,
    /// Segundos UTC desde epoch. Lo que usan índices y ventanas temporales.
    pub at_epoch: i64,
    pub actor: Actor,
    pub title: String,
    pub body: String,
    /// Severidad NATIVA de la fuente si existe (ERROR/WARN/INFO, 5xx…); "" si no.
    pub level: String,
    /// Atributos planos del adaptador (status=500, method=GET, unit=nginx…).
    pub attrs: BTreeMap<String, String>,
    pub touches: Vec<Touch>,
    pub links: Vec<Link>,
    pub simhash: u64,
    /// > umbral de touches → se excluye de coupling (commit/evento masivo).
    pub is_bulk: bool,
}

impl Event {
    /// Marca el evento como masivo si toca más entidades que `threshold`.
    /// Un evento masivo (reformateo, import inicial) contamina el coupling.
    pub fn mark_bulk(&mut self, threshold: usize) {
        self.is_bulk = self.touches.len() > threshold;
    }

    /// Primer segmento de cada entidad tocada (feature/dimensión "top:<dir>").
    /// Determinista y ordenado; útil para JEV y para agregados por directorio.
    pub fn top_segments(&self) -> Vec<String> {
        let mut segs: Vec<String> = self
            .touches
            .iter()
            .map(|t| t.entity.split('/').next().unwrap_or("").to_string())
            .filter(|s| !s.is_empty())
            .collect();
        segs.sort();
        segs.dedup();
        segs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_bulk_respeta_el_umbral() {
        let mut e = Event {
            touches: (0..60)
                .map(|i| Touch {
                    entity: format!("src/f{i}.rs"),
                    entity_type: "file".into(),
                    weight: 1,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        e.mark_bulk(50);
        assert!(e.is_bulk);
        e.mark_bulk(100);
        assert!(!e.is_bulk);
    }

    #[test]
    fn top_segments_ordena_y_deduplica() {
        let e = Event {
            touches: vec![
                Touch { entity: "src/a.rs".into(), ..Default::default() },
                Touch { entity: "docs/x.md".into(), ..Default::default() },
                Touch { entity: "src/b.rs".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(e.top_segments(), vec!["docs".to_string(), "src".to_string()]);
    }
}
