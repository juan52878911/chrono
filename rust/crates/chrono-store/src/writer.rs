//! `Writer`: una transacción de ingesta con sentencias preparadas cacheadas y
//! cachés en memoria de `actor.key -> id` / `entity.key -> id`, para no volver
//! a consultar por cada fila (igual que el `Writer` de Go).

use std::collections::{HashMap, HashSet};

use rusqlite::{params, OptionalExtension, Transaction};

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
    dim_values: HashMap<(String, String), i64>,
    link_targets: HashMap<(String, String), i64>,
    attr_keys: HashSet<(i64, String)>,
    dims: Vec<String>,
}

impl WriterCaches {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cachés vacías con las `dims` de la fuente: esas claves de `attrs` van a
    /// `event_dims` (ver [`Writer::set_dims`]).
    pub fn with_dims(dims: Vec<String>) -> Self {
        WriterCaches { dims, ..Self::default() }
    }
}

/// Acumula una ingesta en una única transacción. Si se hace `Drop` sin llamar
/// a [`Writer::commit`], `rusqlite::Transaction` revierte automáticamente.
pub struct Writer<'a> {
    tx: Transaction<'a>,
    c: WriterCaches,
    /// `(id, rid)` del último evento insertado: `add_labels` va justo detrás.
    last: Option<(String, i64)>,
    /// `rid` del siguiente evento: por encima del máximo de `events` y de
    /// `meta.rid_hwm`, para no reutilizar los de lo borrado. Se calcula al
    /// primer evento del lote.
    next_rid: Option<i64>,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(tx: Transaction<'a>) -> Self {
        Writer { tx, c: WriterCaches::default(), last: None, next_rid: None }
    }

    /// Como [`Writer::new`] pero sembrado con cachés de un lote anterior.
    pub(crate) fn new_seeded(tx: Transaction<'a>, caches: WriterCaches) -> Self {
        Writer { tx, c: caches, last: None, next_rid: None }
    }

    /// Commitea el lote y DEVUELVE las cachés, para sembrar el siguiente lote.
    pub fn finish(self) -> Result<WriterCaches> {
        self.tx.commit()?;
        Ok(self.c)
    }

    /// Claves de `attrs` que se guardan como dims (P-REL-1): salen del JSON y
    /// van a `event_dims`, con el valor una vez en `dim_values`. Se aplica a los
    /// eventos que se añadan después.
    pub fn set_dims(&mut self, dims: Vec<String>) {
        self.c.dims = dims;
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
        let mut attrs = ev.attrs.clone();
        if let Some(lv) = (ev.level.is_empty() && ev.kind != "commit").then(|| chrono_core::infer_level(&ev.title)).flatten() {
            attrs.entry("level_inferred".to_string()).or_insert_with(|| lv.to_string());
        }
        // Las dims salen del JSON (van a `event_dims` tras insertar el evento).
        let dims: Vec<(String, String)> =
            self.c.dims.iter().filter_map(|k| attrs.remove(k).map(|v| (k.clone(), v))).collect();
        let attrs_json = attrs_to_json(&attrs);
        let touches_n = ev.touches.len() as i64;
        let is_bulk = i64::from(ev.is_bulk);
        // u64 -> i64 reinterpretando bits (misma convención que el Go: int64(simhash)).
        let simhash = ev.simhash as i64;

        let rid = match self.next_rid {
            Some(r) => r,
            None => {
                self.tx.query_row(
                    "SELECT MAX(COALESCE((SELECT MAX(rid) FROM events), 0),
                                COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'rid_hwm'), 0)) + 1",
                    [],
                    |r| r.get(0),
                )?
            }
        };
        let inserted = self
            .tx
            .prepare_cached(
                "INSERT OR IGNORE INTO events
                    (rid, id, source_ref, kind, at_epoch, actor_id, title, body, level,
                     simhash, is_bulk, touches_n, attrs)
                 VALUES (?13,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            )?
            .execute(params![
                ev.id,
                source_ref,
                ev.kind,
                ev.at_epoch,
                actor_id,
                ev.title,
                ev.body,
                ev.level,
                simhash,
                is_bulk,
                touches_n,
                attrs_json,
                rid,
            ])?;
        self.next_rid = Some(rid + i64::from(inserted > 0));
        if inserted == 0 {
            return Ok(false);
        }
        self.last = Some((ev.id.clone(), rid));

        for (key, value) in dims {
            let dim_id = self.dim_id(key, value)?;
            self.tx
                .prepare_cached("INSERT OR IGNORE INTO event_dims(dim_id, event_rid) VALUES (?1, ?2)")?
                .execute(params![dim_id, rid])?;
        }
        for key in attrs.keys() {
            if !self.c.attr_keys.contains(&(source_ref, key.clone())) {
                self.tx
                    .prepare_cached("INSERT OR IGNORE INTO attr_keys(source_ref, key) VALUES (?1, ?2)")?
                    .execute(params![source_ref, key])?;
                self.c.attr_keys.insert((source_ref, key.clone()));
            }
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
                    "INSERT OR IGNORE INTO touches(event_rid, entity_id, weight, added, deleted, attrs)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                )?
                .execute(params![rid, entity_id, t.weight, added, deleted, touch_attrs])?;
        }

        for l in &ev.links {
            let target_id = self.link_target_id(&l.rel, &l.target)?;
            self.tx
                .prepare_cached("INSERT OR IGNORE INTO event_links(event_rid, target_id) VALUES (?1, ?2)")?
                .execute(params![rid, target_id])?;
        }

        Ok(true)
    }

    /// Inserta las etiquetas de clasificación de `event_id` (una fila por
    /// `task`, `INSERT OR IGNORE`: la primera etiqueta de cada tarea gana).
    pub fn add_labels(&mut self, event_id: &str, labels: &[chrono_core::Label]) -> Result<()> {
        if labels.is_empty() {
            return Ok(());
        }
        let rid = match &self.last {
            Some((id, rid)) if id == event_id => *rid,
            _ => self.tx.prepare_cached("SELECT rid FROM events WHERE id = ?1")?.query_row(params![event_id], |r| r.get(0))?,
        };
        for lb in labels {
            let evidence = lb.evidence.join("\n");
            self.tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO labels(event_rid, task, label, confidence, source, evidence)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                )?
                .execute(params![rid, lb.task, lb.label, lb.confidence, lb.source, evidence])?;
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
        if let Some(&id) = self.c.actors.get(&actor.key) {
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

        self.c.actors.insert(actor.key.clone(), id);
        Ok(Some(id))
    }

    /// Upsert de entidad por `key`; se crea con `size = 0` si no existía.
    fn entity_id(&mut self, key: &str, entity_type: &str) -> Result<i64> {
        if let Some(&id) = self.c.entities.get(key) {
            return Ok(id);
        }

        self.tx
            .prepare_cached("INSERT OR IGNORE INTO entities(key, type, size) VALUES (?1,?2,0)")?
            .execute(params![key, entity_type])?;
        let id: i64 = self
            .tx
            .prepare_cached("SELECT id FROM entities WHERE key = ?1")?
            .query_row(params![key], |r| r.get(0))?;

        self.c.entities.insert(key.to_string(), id);
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
        if let Some(&r) = self.c.sources.get(source_id) {
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
        self.c.sources.insert(source_id.to_string(), r);
        if !self.c.dims.is_empty() {
            for key in &self.c.dims {
                self.tx
                    .prepare_cached("INSERT OR IGNORE INTO source_dims(source_ref, key) VALUES (?1, ?2)")?
                    .execute(params![r, key])?;
            }
        }
        Ok(r)
    }

    /// Entidad `(key, type)` del primer evento ya escrito (en el índice o en
    /// este lote) con el link `(rel, target)`, sin contar `unknown`.
    pub fn linked_entity(&mut self, rel: &str, target: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .tx
            .prepare_cached(
                "SELECT en.key, en.type FROM link_targets lt
                 JOIN event_links el ON el.target_id = lt.id
                 JOIN touches t ON t.event_rid = el.event_rid
                 JOIN entities en ON en.id = t.entity_id
                 WHERE lt.rel = ?1 AND lt.target = ?2 AND en.key <> 'unknown'
                 ORDER BY el.event_rid LIMIT 1",
            )?
            .query_row(params![rel, target], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?)
    }

    /// Upsert de `(rel, target)` en `link_targets`, cacheado (v7).
    fn link_target_id(&mut self, rel: &str, target: &str) -> Result<i64> {
        let k = (rel.to_string(), target.to_string());
        if let Some(&id) = self.c.link_targets.get(&k) {
            return Ok(id);
        }
        self.tx
            .prepare_cached("INSERT OR IGNORE INTO link_targets(rel, target) VALUES (?1, ?2)")?
            .execute(params![k.0, k.1])?;
        let id: i64 = self
            .tx
            .prepare_cached("SELECT id FROM link_targets WHERE rel = ?1 AND target = ?2")?
            .query_row(params![k.0, k.1], |r| r.get(0))?;
        self.c.link_targets.insert(k, id);
        Ok(id)
    }

    /// Upsert de `(clave, valor)` en `dim_values`, cacheado.
    fn dim_id(&mut self, key: String, value: String) -> Result<i64> {
        let k = (key, value);
        if let Some(&id) = self.c.dim_values.get(&k) {
            return Ok(id);
        }
        self.tx
            .prepare_cached("INSERT OR IGNORE INTO dim_values(key, value) VALUES (?1, ?2)")?
            .execute(params![k.0, k.1])?;
        let id: i64 = self
            .tx
            .prepare_cached("SELECT id FROM dim_values WHERE key = ?1 AND value = ?2")?
            .query_row(params![k.0, k.1], |r| r.get(0))?;
        self.c.dim_values.insert(k, id);
        Ok(id)
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
