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
