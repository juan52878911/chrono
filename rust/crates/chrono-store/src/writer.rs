//! `Writer`: una transacción de ingesta con sentencias preparadas cacheadas y
//! cachés en memoria de `actor.key -> id` / `entity.key -> id`, para no volver
//! a consultar por cada fila (igual que el `Writer` de Go).

use std::collections::HashMap;

use rusqlite::{params, Transaction};

use crate::json::attrs_to_json;
use crate::Result;

/// Cachés `clave -> rowid` de actores y entidades, preservadas ENTRE lotes de
/// ingesta (los rowids son válidos una vez commiteados, así que un lote nuevo
/// puede reusarlos sin re-consultar). Permite commitear por lotes sin perder la
/// aceleración de caché (clave para no desplomar la ingesta de git a escala).
#[derive(Debug, Default)]
pub struct WriterCaches {
    actors: HashMap<String, i64>,
    entities: HashMap<String, i64>,
    sources: HashMap<String, i64>,
}

impl WriterCaches {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Acumula una ingesta en una única transacción. Si se hace `Drop` sin llamar
/// a [`Writer::commit`], `rusqlite::Transaction` revierte automáticamente.
pub struct Writer<'a> {
    tx: Transaction<'a>,
    actors: HashMap<String, i64>,
    entities: HashMap<String, i64>,
    sources: HashMap<String, i64>,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(tx: Transaction<'a>) -> Self {
        Writer { tx, actors: HashMap::new(), entities: HashMap::new(), sources: HashMap::new() }
    }

    /// Como [`Writer::new`] pero sembrado con cachés de un lote anterior.
    pub(crate) fn new_seeded(tx: Transaction<'a>, caches: WriterCaches) -> Self {
        Writer { tx, actors: caches.actors, entities: caches.entities, sources: caches.sources }
    }

    /// Commitea el lote y DEVUELVE las cachés, para sembrar el siguiente lote.
    pub fn finish(self) -> Result<WriterCaches> {
        let caches = WriterCaches {
            actors: self.actors,
            entities: self.entities,
            sources: self.sources,
        };
        self.tx.commit()?;
        Ok(caches)
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
    /// Devuelve si el evento entró: con un id que ya estaba (re-lectura tras un
    /// watermark, o un `uid_field` repetido en otro fichero) no se toca nada, ni
    /// sus touches ni sus links (otra entidad lo contaría dos veces).
    pub fn add_event(&mut self, ev: &chrono_core::Event) -> Result<bool> {
        let actor_id = self.actor_id(&ev.actor)?;
        let source_ref = self.source_ref(&ev.source_id)?;

        // Sin nivel de la fuente: el que diga el texto, APARTE (`level` sigue
        // siendo el de la fuente para que las cifras coincidan con ella).
        let attrs_json = match (ev.level.is_empty() && ev.kind != "commit")
            .then(|| chrono_core::infer_level(&ev.title))
            .flatten()
        {
            Some(lv) if !ev.attrs.contains_key("level_inferred") => {
                let mut a = ev.attrs.clone();
                a.insert("level_inferred".to_string(), lv.to_string());
                attrs_to_json(&a)
            }
            _ => attrs_to_json(&ev.attrs),
        };
        let touches_n = ev.touches.len() as i64;
        let is_bulk = i64::from(ev.is_bulk);
        // u64 -> i64 reinterpretando bits (misma convención que el Go: int64(simhash)).
        let simhash = ev.simhash as i64;

        let inserted = self
            .tx
            .prepare_cached(
                "INSERT OR IGNORE INTO events
                    (id, source_ref, kind, at, at_epoch, actor_id, title, body, level,
                     simhash, is_bulk, touches_n, attrs)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            )?
            .execute(params![
                ev.id,
                source_ref,
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
        if inserted == 0 {
            return Ok(false);
        }

        for t in &ev.touches {
            let entity_id = self.entity_id(&t.entity, &t.entity_type)?;
            let touch_attrs = attrs_to_json(&t.attrs);
            // v5: added/deleted como columnas enteras (git las trae como string en
            // attrs; binario = "-1"). Se guarda el valor crudo; el clamp a >=0 lo
            // hace `churn` con MAX(x,0), igual que antes con json_extract.
            let (added, deleted) = touch_lines(&t.attrs);
            self.tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO touches(event_id, entity_id, weight, added, deleted, attrs)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                )?
                .execute(params![ev.id, entity_id, t.weight, added, deleted, touch_attrs])?;
        }

        for l in &ev.links {
            self.tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO links(event_id, rel, target) VALUES (?1,?2,?3)",
                )?
                .execute(params![ev.id, l.rel, l.target])?;
        }

        Ok(true)
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

    /// Resuelve el `source_id` string (`"git:/abs"`) al entero `sources.ref`,
    /// cacheado. Auto-registra un stub `INSERT OR IGNORE INTO sources(id, kind)`
    /// la primera vez: así el evento puede insertar su `source_ref` aunque el
    /// `upsert_source` del CLI (que enriquece path/watermark/manifest vía
    /// `ON CONFLICT(id)`) se llame DESPUÉS de la ingesta. El `kind` del stub es
    /// el prefijo del id (`"git:/abs"` → `"git"`), que coincide con el real
    /// (`canonical_source_id` usa `src.kind()` como prefijo) → sin divergencia.
    fn source_ref(&mut self, source_id: &str) -> Result<i64> {
        if let Some(&r) = self.sources.get(source_id) {
            return Ok(r);
        }
        let kind = source_id.split(':').next().unwrap_or("");
        self.tx
            .prepare_cached("INSERT OR IGNORE INTO sources(id, kind) VALUES (?1, ?2)")?
            .execute(params![source_id, kind])?;
        let r: i64 = self
            .tx
            .prepare_cached("SELECT ref FROM sources WHERE id = ?1")?
            .query_row(params![source_id], |x| x.get(0))?;
        self.sources.insert(source_id.to_string(), r);
        Ok(r)
    }
}

/// Lee `added`/`deleted` de las attrs de un touch (git las trae como string;
/// binario = `"-1"`). Ausente o no parseable → 0, idéntico al antiguo
/// `CAST(COALESCE(json_extract(...),0) AS INTEGER)` de churn para los casos
/// reales (solo git escribe estas claves, siempre enteras).
fn touch_lines(attrs: &std::collections::BTreeMap<String, String>) -> (i64, i64) {
    let parse = |k: &str| attrs.get(k).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    (parse("added"), parse("deleted"))
}
