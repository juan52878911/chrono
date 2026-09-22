//! `Writer`: una transacción de ingesta con sentencias preparadas cacheadas y
//! cachés en memoria de `actor.key -> id` / `entity.key -> id`, para no volver
//! a consultar por cada fila (igual que el `Writer` de Go).

use std::collections::HashMap;

use rusqlite::{params, Transaction};

use crate::json::attrs_to_json;
use crate::Result;

/// Acumula una ingesta en una única transacción. Si se hace `Drop` sin llamar
/// a [`Writer::commit`], `rusqlite::Transaction` revierte automáticamente.
pub struct Writer<'a> {
    tx: Transaction<'a>,
    actors: HashMap<String, i64>,
    entities: HashMap<String, i64>,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(tx: Transaction<'a>) -> Self {
        Writer { tx, actors: HashMap::new(), entities: HashMap::new() }
    }

    /// Inserta un evento COMPLETO: upsert de actor (y su alias en
    /// `actor_identities`), upsert de cada entidad tocada, la fila de `events`,
    /// sus `touches` y sus `links`.
    ///
    /// Nota: `chrono_core::Event` no lleva las `Label` (las produce el
    /// `Classifier` del núcleo por separado, no el evento). Para persistirlas
    /// tras clasificar, usa [`Writer::add_labels`] con el mismo `event.id`.
    ///
    /// Todas las inserciones usan `INSERT OR IGNORE` / upsert idempotente, así
    /// que reingestar el mismo evento es seguro (no falla ni duplica filas).
    pub fn add_event(&mut self, ev: &chrono_core::Event) -> Result<()> {
        let actor_id = self.actor_id(&ev.actor)?;

        let attrs_json = attrs_to_json(&ev.attrs);
        let touches_n = ev.touches.len() as i64;
        let is_bulk = i64::from(ev.is_bulk);
        // u64 -> i64 reinterpretando bits (misma convención que el Go: int64(simhash)).
        let simhash = ev.simhash as i64;

        self.tx
            .prepare_cached(
                "INSERT OR IGNORE INTO events
                    (id, source_id, kind, at, at_epoch, actor_id, title, body, level,
                     simhash, is_bulk, touches_n, attrs)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            )?
            .execute(params![
                ev.id,
                ev.source_id,
                ev.kind,
                ev.at,
                ev.at_epoch,
                actor_id,
                ev.title,
                ev.body,
                ev.level,
                simhash,
                is_bulk,
                touches_n,
                attrs_json,
            ])?;

        for t in &ev.touches {
            let entity_id = self.entity_id(&t.entity, &t.entity_type)?;
            let touch_attrs = attrs_to_json(&t.attrs);
            self.tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO touches(event_id, entity_id, weight, attrs)
                     VALUES (?1,?2,?3,?4)",
                )?
                .execute(params![ev.id, entity_id, t.weight, touch_attrs])?;
        }

        for l in &ev.links {
            self.tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO links(event_id, rel, target) VALUES (?1,?2,?3)",
                )?
                .execute(params![ev.id, l.rel, l.target])?;
        }

        Ok(())
    }

    /// Inserta las etiquetas de clasificación de `event_id` (una fila por
    /// `task`, `INSERT OR IGNORE`: la primera etiqueta de cada tarea gana).
    pub fn add_labels(&mut self, event_id: &str, labels: &[chrono_core::Label]) -> Result<()> {
        for lb in labels {
            let evidence = lb.evidence.join("\n");
            self.tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO labels(event_id, task, label, confidence, source, evidence)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                )?
                .execute(params![event_id, lb.task, lb.label, lb.confidence, lb.source, evidence])?;
        }
        Ok(())
    }

    /// Cierra la transacción de ingesta.
    pub fn commit(self) -> Result<()> {
        self.tx.commit()?;
        Ok(())
    }

    /// Upsert de actor por `key`; añade también el alias en `actor_identities`.
    /// Un `Actor` con `key` vacía no tiene identidad unificable: se guarda el
    /// evento sin `actor_id` (columna nullable).
    fn actor_id(&mut self, actor: &chrono_core::Actor) -> Result<Option<i64>> {
        if actor.key.is_empty() {
            return Ok(None);
        }
        if let Some(&id) = self.actors.get(&actor.key) {
            return Ok(Some(id));
        }

        self.tx
            .prepare_cached("INSERT OR IGNORE INTO actors(key, display_name) VALUES (?1, ?2)")?
            .execute(params![actor.key, actor.name])?;
        let id: i64 = self
            .tx
            .prepare_cached("SELECT id FROM actors WHERE key = ?1")?
            .query_row(params![actor.key], |r| r.get(0))?;

        // alias = la propia key (email/host que unifica identidades).
        self.tx
            .prepare_cached(
                "INSERT OR IGNORE INTO actor_identities(alias, name, actor_id) VALUES (?1,?2,?3)",
            )?
            .execute(params![actor.key, actor.name, id])?;

        self.actors.insert(actor.key.clone(), id);
        Ok(Some(id))
    }

    /// Upsert de entidad por `key`; se crea con `size = 0` si no existía.
    fn entity_id(&mut self, key: &str, entity_type: &str) -> Result<i64> {
        if let Some(&id) = self.entities.get(key) {
            return Ok(id);
        }

        self.tx
            .prepare_cached("INSERT OR IGNORE INTO entities(key, type, size) VALUES (?1,?2,0)")?
            .execute(params![key, entity_type])?;
        let id: i64 = self
            .tx
            .prepare_cached("SELECT id FROM entities WHERE key = ?1")?
            .query_row(params![key], |r| r.get(0))?;

        self.entities.insert(key.to_string(), id);
        Ok(id)
    }
}
