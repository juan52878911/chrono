//! `Store`: apertura/migración del índice SQLite v2 y operaciones de solo-lectura
//! o de mantenimiento que no forman parte de la ingesta transaccional (esa vive
//! en `writer.rs`).

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use crate::writer::Writer;
use crate::{Result, SCHEMA_VERSION};

/// Índice SQLite v2 de chrono (un único fichero, WAL, FTS5 externo).
pub struct Store {
    pub(crate) conn: Connection,
}

/// Una fuente registrada en la tabla `sources` (multi-fuente en un `.chrono/`).
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRow {
    /// `source_id` canónico: `<kind>:<ruta abs>`, igual que `events.source_id`.
    pub id: String,
    pub kind: String,
    pub path: String,
    pub watermark: String,
    pub manifest_json: String,
}

impl Store {
    /// Abre (o crea) el índice en `path`.
    ///
    /// Si el fichero es nuevo (no tiene tabla `meta`), aplica `schema.sql` y
    /// escribe `meta.schema_version = SCHEMA_VERSION`. Si ya existe con una
    /// versión de esquema distinta, devuelve `Err` (el llamador debe reindexar).
    /// Aplica los PRAGMAs de ingesta rápida (el índice es regenerable).
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;

        // PRAGMAs de conexión: se pierden al reabrir, así que se reaplican siempre.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;

        let is_new = !table_exists(&conn, "meta")?;
        if is_new {
            conn.execute_batch(crate::schema_sql())?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![SCHEMA_VERSION.to_string()],
            )?;
        } else {
            let version: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'schema_version'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let want = SCHEMA_VERSION.to_string();
            if version.as_deref() != Some(want.as_str()) {
                return Err(format!(
                    "chrono-store: schema_version del índice es {:?}, se requiere {} \
                     (reindexa: borra el índice o usa Store::reset)",
                    version, SCHEMA_VERSION
                )
                .into());
            }
        }

        // Índice regenerable desde la fuente: prioriza velocidad de ingesta.
        conn.execute_batch(
            "PRAGMA synchronous = OFF;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 268435456;",
        )?;

        Ok(Store { conn })
    }

    /// Escribe (o reemplaza) un valor del manifiesto `meta`.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Lee un valor del manifiesto `meta`, o `None` si no existe.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?;
        Ok(v)
    }

    /// Registra o actualiza una fuente en la tabla `sources` (multi-fuente).
    /// `manifest` se serializa a JSON determinista. `id` es el `source_id`
    /// canónico (`<kind>:<ruta abs>`), el mismo que llevan los `events`.
    pub fn upsert_source(
        &self,
        id: &str,
        kind: &str,
        path: &str,
        watermark: &str,
        manifest: &std::collections::BTreeMap<String, String>,
        last_sync_at: &str,
    ) -> Result<()> {
        let manifest_json = crate::json::attrs_to_json(manifest);
        self.conn.execute(
            "INSERT INTO sources(id, kind, path, watermark, manifest_json, last_sync_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               kind = excluded.kind, path = excluded.path, watermark = excluded.watermark,
               manifest_json = excluded.manifest_json, last_sync_at = excluded.last_sync_at",
            params![id, kind, path, watermark, manifest_json, last_sync_at],
        )?;
        Ok(())
    }

    /// Actualiza el watermark (y `last_sync_at`) de una fuente ya registrada.
    pub fn set_source_watermark(&self, id: &str, watermark: &str, last_sync_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sources SET watermark = ?2, last_sync_at = ?3 WHERE id = ?1",
            params![id, watermark, last_sync_at],
        )?;
        Ok(())
    }

    /// Lista las fuentes registradas, orden estable por `id`.
    pub fn list_sources(&self) -> Result<Vec<SourceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, path, COALESCE(watermark,''), COALESCE(manifest_json,'{}')
             FROM sources ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                path: r.get(2)?,
                watermark: r.get(3)?,
                manifest_json: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Inserta touches "de enriquecimiento" (p.ej. símbolos S1) para eventos ya
    /// ingeridos, upsertando las entidades por `key`. Cada tupla es
    /// `(entity_key, entity_type, weight, attrs_json)`. Idempotente: reinsertar
    /// el mismo (event, entity) actualiza peso/attrs. NO toca `touches_n` ni
    /// `is_bulk` del evento (esos reflejan solo los touches de fichero).
    pub fn add_touches(&self, event_id: &str, touches: &[(String, String, i64, String)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut upsert_entity =
                tx.prepare("INSERT INTO entities(key, type) VALUES (?1, ?2) ON CONFLICT(key) DO NOTHING")?;
            let mut entity_id = tx.prepare("SELECT id FROM entities WHERE key = ?1")?;
            let mut add_touch = tx.prepare(
                "INSERT INTO touches(event_id, entity_id, weight, attrs) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(event_id, entity_id) DO UPDATE SET weight = excluded.weight, attrs = excluded.attrs",
            )?;
            for (key, typ, weight, attrs) in touches {
                upsert_entity.execute(params![key, typ])?;
                let id: i64 = entity_id.query_row(params![key], |r| r.get(0))?;
                add_touch.execute(params![event_id, id, weight, attrs])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Borra las entidades de un `type` dado y sus touches (para recomputar S1
    /// de símbolos sin tocar ficheros/logs). Usado al reindexar símbolos.
    pub fn delete_entities_of_type(&self, entity_type: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM touches WHERE entity_id IN (SELECT id FROM entities WHERE type = ?1)",
            params![entity_type],
        )?;
        tx.execute("DELETE FROM entities WHERE type = ?1", params![entity_type])?;
        tx.commit()?;
        Ok(())
    }

    /// Borra los eventos de una fuente (y sus `touches`/`labels`/`links`), para
    /// re-ingesta tras divergencia. NO toca `entities`/`actors` (compartidos
    /// entre fuentes; sus flags se recalculan en el enriquecimiento).
    pub fn delete_source_events(&self, source_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for child in ["touches", "labels", "links"] {
            tx.execute(
                &format!(
                    "DELETE FROM {child} WHERE event_id IN (SELECT id FROM events WHERE source_id = ?1)"
                ),
                params![source_id],
            )?;
        }
        tx.execute("DELETE FROM events WHERE source_id = ?1", params![source_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Borra todos los datos de ingesta (para reindex). Conserva `meta`.
    ///
    /// El orden de borrado respeta las claves foráneas: hijos antes que padres.
    pub fn reset(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        for table in [
            "touches",
            "links",
            "labels",
            "events",
            "actor_identities",
            "actors",
            "entities",
            "markers",
            "blob_lines",
            "issues",
            "sources",
            "rollups",
            "templates",
        ] {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
        }
        // events_fts es de contenido externo: hay que vaciarlo explícitamente.
        tx.execute("INSERT INTO events_fts(events_fts) VALUES ('delete-all')", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Inicia una transacción de ingesta. Si se hace `Drop` sin `commit`, revierte.
    pub fn writer(&mut self) -> Result<Writer<'_>> {
        let tx = self.conn.transaction()?;
        Ok(Writer::new(tx))
    }

    /// Acceso de solo lectura a la conexión (para `chrono-metrics`).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Rutas (`entity.key`) de tipo `file` no borradas, ordenadas por número de
    /// `touches` descendente, top `limit`.
    pub fn top_changed_entities(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.key FROM touches t
             JOIN entities e ON e.id = t.entity_id
             WHERE e.type = 'file' AND e.deleted = 0
             GROUP BY e.id
             ORDER BY COUNT(*) DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Devuelve el conteo de líneas cacheado para los OIDs de blob pedidos.
    /// Los OIDs no presentes en la caché simplemente no aparecen en el mapa.
    pub fn get_blob_lines(&self, oids: &[String]) -> Result<HashMap<String, i64>> {
        let mut out = HashMap::with_capacity(oids.len());
        let mut stmt = self.conn.prepare("SELECT lines FROM blob_lines WHERE oid = ?1")?;
        for oid in oids {
            let lines: Option<i64> = stmt.query_row(params![oid], |r| r.get(0)).optional()?;
            if let Some(lines) = lines {
                out.insert(oid.clone(), lines);
            }
        }
        Ok(out)
    }

    /// Guarda conteos de líneas recién calculados (upsert por OID).
    pub fn put_blob_lines(&self, rows: &[(String, i64)]) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO blob_lines(oid, lines) VALUES (?1, ?2)
             ON CONFLICT(oid) DO UPDATE SET lines = excluded.lines",
        )?;
        for (oid, lines) in rows {
            stmt.execute(params![oid, lines])?;
        }
        Ok(())
    }

    /// Fija `entities.size` para `key` (dimensionado de hotspots).
    pub fn set_entity_size(&self, key: &str, size: i64) -> Result<()> {
        self.conn.execute("UPDATE entities SET size = ?1 WHERE key = ?2", params![size, key])?;
        Ok(())
    }

    /// Reconstruye `templates`/`rollups` desde cero: normaliza el `title` de
    /// cada evento con tiempo válido a una plantilla Drain-light y cuenta por
    /// (fuente, plantilla, bucket de `bucket_secs`). La agregación es EN MEMORIA
    /// pero acotada por el nº de plantillas×buckets (pequeño: ese es el punto de
    /// los rollups), no por el nº de eventos. Devuelve (nº plantillas, nº filas
    /// de rollup). `bucket_secs` debe ser > 0.
    pub fn build_rollups(&self, bucket_secs: i64) -> Result<(usize, usize)> {
        use std::collections::BTreeMap;
        if bucket_secs <= 0 {
            return Err("build_rollups: bucket_secs debe ser > 0".into());
        }
        // Fase 1: escanear eventos y agregar en memoria (sin escribir).
        // Clave: (source_id, template, bucket) -> (count, first_id, last_id).
        let mut agg: BTreeMap<(String, String, i64), (i64, String, String)> = BTreeMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT id, source_id, at_epoch, title FROM events WHERE at_epoch > 0 ORDER BY rowid",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (id, source_id, at_epoch, title) = row?;
                let template = chrono_core::templatize(&title);
                let bucket = at_epoch - at_epoch.rem_euclid(bucket_secs);
                let entry = agg.entry((source_id, template, bucket)).or_insert((0, id.clone(), String::new()));
                entry.0 += 1;
                entry.2 = id; // last_id (iteración en orden de rowid → determinista)
            }
        }

        // Fase 2: reescribir templates + rollups desde la agregación.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM rollups", [])?;
        tx.execute("DELETE FROM templates", [])?;
        let mut n_templates = 0usize;
        let mut n_rollups = 0usize;
        {
            let mut upsert_tpl = tx.prepare(
                "INSERT INTO templates(source_id, template) VALUES (?1, ?2)
                 ON CONFLICT(source_id, template) DO NOTHING",
            )?;
            let mut tpl_id = tx.prepare("SELECT id FROM templates WHERE source_id = ?1 AND template = ?2")?;
            let mut ins_rollup = tx.prepare(
                "INSERT INTO rollups(source_id, template_id, bucket_epoch, count, first_id, last_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for ((source_id, template, bucket), (count, first_id, last_id)) in &agg {
                let inserted = upsert_tpl.execute(params![source_id, template])?;
                if inserted > 0 {
                    n_templates += 1;
                }
                let id: i64 = tpl_id.query_row(params![source_id, template], |r| r.get(0))?;
                ins_rollup.execute(params![source_id, id, bucket, count, first_id, last_id])?;
                n_rollups += 1;
            }
        }
        tx.commit()?;
        Ok((n_templates, n_rollups))
    }

    /// Rellena `events_fts` (contenido externo) desde `events`. Se llama al
    /// final de la ingesta; es idempotente (vacía antes de rellenar).
    pub fn rebuild_fts(&self) -> Result<()> {
        self.conn.execute("INSERT INTO events_fts(events_fts) VALUES ('delete-all')", [])?;
        self.conn.execute(
            "INSERT INTO events_fts(rowid, title, body) SELECT rowid, title, body FROM events",
            [],
        )?;
        Ok(())
    }

    /// Compacta el índice al terminar la ingesta: FTS `optimize` + `ANALYZE` +
    /// `wal_checkpoint(TRUNCATE)` + `VACUUM`.
    pub fn optimize(&self) -> Result<()> {
        self.conn.execute("INSERT INTO events_fts(events_fts) VALUES ('optimize')", [])?;
        self.conn.execute_batch(
            "ANALYZE;
             PRAGMA wal_checkpoint(TRUNCATE);
             VACUUM;",
        )?;
        Ok(())
    }
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}
