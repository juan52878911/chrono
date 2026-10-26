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

/// Eventos de la fuente `?1` y, si `?2` no es NULL, solo los del fichero `?2`
/// de una fuente glob (dim `file`; sin ella, attrs).
const SELECTED_EVENTS: &str = "source_ref = (SELECT ref FROM sources WHERE id = ?1)
    AND (?2 IS NULL OR rid IN (SELECT d.event_rid FROM event_dims d JOIN dim_values v ON v.id = d.dim_id
                               WHERE v.key = 'file' AND v.value = ?2)
                    OR json_extract(attrs, '$.file') = ?2)";

/// Antes de borrar eventos, el `rid` más alto que ha existido queda en
/// `meta.rid_hwm` (solo sube). `events.rid` es `INTEGER PRIMARY KEY` sin
/// AUTOINCREMENT: SQLite daría a lo nuevo los rid de lo borrado si eran los más
/// altos, y quien sigue el índice por rid (`tail`, la marca `stale_dims`, los
/// watermarks de FTS y rollups) se saltaba esos eventos o los contaba mal. El
/// `Writer` numera por encima de este suelo (ver `Writer::add_event`).
pub(crate) const BUMP_RID_HWM: &str = "INSERT INTO meta(key, value)
    SELECT 'rid_hwm', (SELECT MAX(rid) FROM events) WHERE EXISTS (SELECT 1 FROM events)
    ON CONFLICT(key) DO UPDATE SET value = MAX(CAST(value AS INTEGER), CAST(excluded.value AS INTEGER))";

impl Store {
    /// Abre (o crea) el índice en `path`.
    ///
    /// Si el fichero es nuevo (no tiene tabla `meta`), aplica `schema.sql` y
    /// escribe `meta.schema_version = SCHEMA_VERSION`. Si ya existe con una
    /// versión de esquema distinta, devuelve `Err` (el llamador debe reindexar).
    /// Aplica los PRAGMAs de ingesta rápida (el índice es regenerable).
    pub fn open(path: &Path) -> Result<Store> {
        Store::open_with(path, true)
    }

    /// Con varias aperturas a la vez sobre un fichero nuevo, el paso a WAL y la
    /// creación del esquema pueden dar `SQLITE_BUSY` sin pasar por el
    /// `busy_timeout` (SQLite no espera si podría haber un interbloqueo): se
    /// reintenta la apertura entera durante unos segundos.
    fn open_with(path: &Path, create: bool) -> Result<Store> {
        let start = std::time::Instant::now();
        loop {
            match Store::try_open(path, create) {
                Err(e) if is_busy(&*e) && start.elapsed() < std::time::Duration::from_secs(10) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                r => return r,
            }
        }
    }

    fn try_open(path: &Path, create: bool) -> Result<Store> {
        let conn = if create {
            Connection::open(path)?
        } else {
            // Una consulta sobre una ruta sin índice no deja un `.db` vacío.
            if !path.is_file() {
                return Err(format!("no hay índice en {} (créalo con chrono init)", path.display()).into());
            }
            use rusqlite::OpenFlags;
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX)?
        };

        // PRAGMAs de conexión: se pierden al reabrir, así que se reaplican siempre.
        // `foreign_keys = OFF`: el índice es un derivado regenerable y el `Writer`
        // ya inserta el evento antes que sus touches/labels, así que la integridad
        // referencial está garantizada por construcción; tener los FK activos
        // añadía una búsqueda aleatoria extra en la PK de `events` por cada
        // touch/label (medido: cuello de ingesta a escala). `wal_autocheckpoint`
        // acota el WAL entre lotes de ingesta (ver `ingest` por lotes en el CLI).
        // `busy_timeout` primero: el cambio a WAL también espera si otro proceso
        // está creando el mismo índice.
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = OFF;
             PRAGMA wal_autocheckpoint = 10000;",
        )?;

        // El esquema se crea en una transacción IMMEDIATE y se vuelve a mirar
        // dentro: con varias consultas a la vez sobre una ruta nueva, otra veía
        // `meta` sin `schema_version` todavía y daba un falso «esquema ?».
        let mut is_new = !table_exists(&conn, "meta")?;
        if is_new {
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let r = (|| -> Result<bool> {
                if table_exists(&conn, "meta")? {
                    return Ok(false);
                }
                conn.execute_batch(crate::schema_sql())?;
                conn.execute(
                    "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![SCHEMA_VERSION.to_string()],
                )?;
                Ok(true)
            })();
            match r {
                Ok(created) => {
                    conn.execute_batch("COMMIT")?;
                    is_new = created;
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
        }
        if !is_new {
            let version: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'schema_version'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let want = SCHEMA_VERSION.to_string();
            if version.as_deref() == Some("6") {
                migrate_v6_links(&conn)?;
            } else if version.as_deref() != Some(want.as_str()) {
                return Err(crate::SchemaMismatch::new(version).into());
            }
        }

        // Índice regenerable desde la fuente: prioriza velocidad de ingesta.
        // `cache_size = -262144` → 256 MB de page cache: mantiene los B-trees de
        // las PK (TEXT aleatorias) en memoria mucho más allá de la caché por
        // defecto de 2 MB, que era la causa del desplome de caudal hacia 1M.
        // `temp_store = FILE` (no MEMORY) → VACUUM/ORDER BY grandes no construyen
        // la copia en RAM (a 10M, `temp_store=MEMORY` reventaba la memoria).
        conn.execute_batch(
            "PRAGMA synchronous = OFF;
             PRAGMA cache_size = -262144;
             PRAGMA temp_store = FILE;
             PRAGMA mmap_size = 268435456;",
        )?;

        Ok(Store { conn })
    }

    /// Abre el índice para consultar: como `open`, pero sin la caché de ingesta.
    ///
    /// La caché de 256 MB y el mmap de 256 MB de `open` se llenan con las páginas
    /// que toca un recorrido, y eso es RSS del proceso aunque la consulta devuelva
    /// 50 filas. Medido a 1M eventos (bench/escala): 16 MB de caché sin mmap baja
    /// el RSS de 440-613 MB a 24-77 MB con el mismo tiempo en las consultas
    /// filtradas; `informe` tarda un 27 % más (9,5 → 12,1 s) mientras recorra `events`.
    pub fn open_read(path: &Path) -> Result<Store> {
        let store = Store::open_with(path, false)?;
        store.conn.execute_batch(
            "PRAGMA cache_size = -16384;
             PRAGMA mmap_size = 0;",
        )?;
        Ok(store)
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

    /// Borra una clave de `meta` (no-op si no está).
    pub fn delete_meta(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
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
    /// `(entity_key, entity_type, weight, added, deleted, attrs_json)`.
    /// Idempotente: reinsertar el mismo (event, entity) actualiza
    /// peso/added/deleted/attrs. NO toca `touches_n` ni `is_bulk` del evento
    /// (esos reflejan solo los touches de fichero).
    pub fn add_touches(&self, event_id: &str, touches: &[(String, String, i64, i64, i64, String)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut upsert_entity =
                tx.prepare("INSERT INTO entities(key, type) VALUES (?1, ?2) ON CONFLICT(key) DO NOTHING")?;
            let mut entity_id = tx.prepare("SELECT id FROM entities WHERE key = ?1")?;
            let rid: i64 = tx.query_row("SELECT rid FROM events WHERE id = ?1", params![event_id], |r| r.get(0))?;
            let mut add_touch = tx.prepare(
                "INSERT INTO touches(event_rid, entity_id, weight, added, deleted, attrs) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(event_rid, entity_id) DO UPDATE SET
                    weight = excluded.weight, added = excluded.added, deleted = excluded.deleted, attrs = excluded.attrs",
            )?;
            for (key, typ, weight, added, deleted, attrs) in touches {
                upsert_entity.execute(params![key, typ])?;
                let id: i64 = entity_id.query_row(params![key], |r| r.get(0))?;
                add_touch.execute(params![rid, id, weight, added, deleted, attrs])?;
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
        self.delete_events_where(source_id, None)?;
        // La re-ingesta escribe las dims de la config de entonces.
        self.conn.execute("DELETE FROM source_dims WHERE source_ref = (SELECT ref FROM sources WHERE id = ?1)", params![source_id])?;
        // Ya no queda ningún evento con las dims de antes (ver `set_source_dims`).
        self.conn.execute("DELETE FROM meta WHERE key = ?1", params![format!("stale_dims:{source_id}")])?;
        Ok(())
    }

    /// Quita una fuente del índice: sus eventos (con touches, labels, links y
    /// dims), su fila de `sources` y lo que cuelga de ella (rollups, plantillas,
    /// claves de attrs, marcadores y sus `meta`). Actores y entidades se quedan:
    /// son compartidos. Devuelve cuántos eventos tenía. FTS y rollups quedan por
    /// reconstruir (lo hace quien llama).
    pub fn remove_source(&self, source_id: &str) -> Result<usize> {
        let n = self.count_source_events(source_id, None)?;
        self.delete_source_events(source_id)?;
        let tx = self.conn.unchecked_transaction()?;
        for table in ["rollups", "dim_rollups", "attr_keys", "source_dims"] {
            tx.execute(&format!("DELETE FROM {table} WHERE source_ref = (SELECT ref FROM sources WHERE id = ?1)"), params![source_id])?;
        }
        for table in ["templates", "markers"] {
            tx.execute(&format!("DELETE FROM {table} WHERE source_id = ?1"), params![source_id])?;
        }
        for key in ["sync_error", "stale_dims", "glob_state"] {
            tx.execute("DELETE FROM meta WHERE key = ?1", params![format!("{key}:{source_id}")])?;
        }
        tx.execute("DELETE FROM sources WHERE id = ?1", params![source_id])?;
        tx.commit()?;
        Ok(n)
    }

    /// Cambia las dims con que se ingiere la fuente SIN reindexarla (no se puede
    /// releer entera): lo nuevo entra con `keys` y los eventos que ya estaban
    /// (rid hasta el máximo de hoy) conservan las de antes. Eso queda en
    /// `meta.stale_dims:<fuente>` (el rid tope) para que `sources` lo cuente.
    pub fn set_source_dims(&self, source_id: &str, keys: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM source_dims WHERE source_ref = (SELECT ref FROM sources WHERE id = ?1)", params![source_id])?;
        for key in keys {
            tx.execute("INSERT OR IGNORE INTO source_dims(source_ref, key) SELECT ref, ?2 FROM sources WHERE id = ?1", params![source_id, key])?;
        }
        tx.execute(
            "INSERT INTO meta(key, value) SELECT ?1, COALESCE(MAX(rid), 0) FROM events WHERE source_ref = (SELECT ref FROM sources WHERE id = ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format!("stale_dims:{source_id}"), source_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Eventos de la fuente (o, con `file`, de ese fichero de una fuente glob).
    pub fn count_source_events(&self, source_id: &str, file: Option<&str>) -> Result<usize> {
        let n: i64 = self.conn.query_row(&format!("SELECT COUNT(*) FROM events WHERE {SELECTED_EVENTS}"), params![source_id, file], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Eventos de una fuente por su `file` (dim o, sin ella, `attrs.file`;
    /// `None` = sin fichero): lo que necesita una fuente glob para saber cuáles
    /// se pueden releer.
    pub fn source_file_counts(&self, source_id: &str) -> Result<Vec<(Option<String>, usize)>> {
        let mut st = self.conn.prepare(
            "SELECT COALESCE((SELECT v.value FROM event_dims d JOIN dim_values v ON v.id = d.dim_id
                              WHERE d.event_rid = e.rid AND v.key = 'file'), json_extract(e.attrs, '$.file')) AS f, COUNT(*)
             FROM events e WHERE e.source_ref = (SELECT ref FROM sources WHERE id = ?1) GROUP BY f",
        )?;
        let rows = st.query_map(params![source_id], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)? as usize)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Segunda pasada de `entity_via`, al acabar una ingesta: los eventos que
    /// siguen en `unknown` toman la entidad del primer evento con su mismo link
    /// `rel` (el resultado que se leyó antes que su uso, p. ej. en otro fichero
    /// de la glob). Solo los que aún no cuentan los rollups (`rid` mayor que su
    /// watermark): cambiar la entidad de algo ya agregado descuadraría
    /// `dim_rollups`. Devuelve cuántos se arreglaron.
    pub fn repair_entity_via(&self, rel: &str) -> Result<usize> {
        let after: i64 = self.meta("rollup_max_rowid")?.and_then(|v| v.parse().ok()).unwrap_or(0);
        let linked = "SELECT t2.entity_id FROM event_links el
                      JOIN link_targets lt ON lt.id = el.target_id AND lt.rel = ?1
                      JOIN event_links el2 ON el2.target_id = el.target_id AND el2.event_rid <> el.event_rid
                      JOIN touches t2 ON t2.event_rid = el2.event_rid
                      JOIN entities en2 ON en2.id = t2.entity_id AND en2.key <> 'unknown'
                      WHERE el.event_rid = touches.event_rid
                      ORDER BY el2.event_rid LIMIT 1";
        Ok(self.conn.execute(
            &format!(
                "UPDATE touches SET entity_id = ({linked})
                 WHERE event_rid > ?2 AND entity_id = (SELECT id FROM entities WHERE key = 'unknown')
                   AND EXISTS ({linked})"
            ),
            params![rel, after],
        )?)
    }

    /// Dims con que se ingirió la fuente (ordenadas; vacío si ninguna).
    pub fn source_dims(&self, source_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.key FROM source_dims d JOIN sources s ON s.ref = d.source_ref WHERE s.id = ?1 ORDER BY d.key",
        )?;
        let rows = stmt.query_map(params![source_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Añade claves a las dims con que se ingirió la fuente (la firma de
    /// opciones de una fuente indexada antes de que existiera).
    pub fn add_source_dims(&self, source_id: &str, keys: &[String]) -> Result<()> {
        for key in keys {
            self.conn.execute(
                "INSERT OR IGNORE INTO source_dims(source_ref, key) SELECT ref, ?2 FROM sources WHERE id = ?1",
                params![source_id, key],
            )?;
        }
        Ok(())
    }

    /// ¿Tiene la fuente algún evento en el índice?
    pub fn source_has_events(&self, source_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE source_ref = (SELECT ref FROM sources WHERE id = ?1))",
            params![source_id],
            |r| r.get(0),
        )?)
    }

    /// Como [`Store::delete_source_events`], solo los eventos de UN fichero de
    /// una fuente glob (los que llevan la dim `file = file`).
    pub fn delete_source_file_events(&self, source_id: &str, file: &str) -> Result<()> {
        self.delete_events_where(source_id, Some(file))
    }

    fn delete_events_where(&self, source_id: &str, file: Option<&str>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        // v5: events.source_ref es entero; se resuelve el string a su `ref` con
        // una subconsulta. Si la fuente no existe, `(SELECT ref …)` da NULL y
        // `source_ref = NULL` no casa nada (no-op, igual que antes). NO se borra
        // la fila de `sources`: su `ref` se conserva para la re-ingesta.
        // v6: `file` es una dim de la fuente glob (ver el CLI); sin ella, attrs.
        let selected = SELECTED_EVENTS;
        // Los rids primero: la selección por fichero lee `event_dims`, que se borra.
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS del_rids(rid INTEGER PRIMARY KEY); DELETE FROM del_rids;")?;
        tx.execute(&format!("INSERT INTO del_rids SELECT rid FROM events WHERE {selected}"), params![source_id, file])?;
        tx.execute(BUMP_RID_HWM, [])?;
        for child in ["touches", "labels", "event_links", "event_dims"] {
            tx.execute(&format!("DELETE FROM {child} WHERE event_rid IN (SELECT rid FROM del_rids)"), [])?;
        }
        tx.execute("DELETE FROM events WHERE rid IN (SELECT rid FROM del_rids)", [])?;
        // Los rollups aún cuentan lo borrado: sin watermark, nadie los da por al
        // día (`dim_rollups`, `timeline`) y el siguiente build los rehace enteros.
        tx.execute("DELETE FROM meta WHERE key = 'rollup_max_rowid'", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Borra todos los datos de ingesta (para reindex). Conserva `meta`.
    ///
    /// El orden de borrado respeta las claves foráneas: hijos antes que padres.
    pub fn reset(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(BUMP_RID_HWM, [])?;
        for table in [
            "touches",
            "event_links",
            "link_targets",
            "labels",
            "event_dims",
            "dim_values",
            "source_dims",
            "attr_keys",
            "dim_rollups",
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
        // Los watermarks incrementales (FTS/rollups) apuntan a rowids que ya no
        // existen: se borran para que el próximo build completo los recalcule
        // desde 0. Los rid nuevos siguen por encima de `rid_hwm`, no desde 1.
        tx.execute("DELETE FROM meta WHERE key IN ('fts_max_rowid', 'rollup_max_rowid')", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Inicia una transacción de ingesta. Si se hace `Drop` sin `commit`, revierte.
    pub fn writer(&mut self) -> Result<Writer<'_>> {
        let tx = self.conn.transaction()?;
        Ok(Writer::new(tx))
    }

    /// Como [`Store::writer`] pero sembrando las cachés de un lote anterior
    /// (para ingesta por lotes: commitear cada N eventos sin re-consultar
    /// actores/entidades ya vistos). Ciérralo con [`Writer::finish`].
    pub fn writer_seeded(&mut self, caches: crate::WriterCaches) -> Result<Writer<'_>> {
        let tx = self.conn.transaction()?;
        Ok(Writer::new_seeded(tx, caches))
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
    /// Reconstruye rollups + templates desde cero (barre TODOS los eventos).
    /// Para `init`, `reset` y tras una divergencia (los rowids se reasignan, así
    /// que el watermark incremental ya no vale). Deja `rollup_max_rowid` al tope.
    pub fn build_rollups(&self, bucket_ms: i64) -> Result<(usize, usize)> {
        self.build_rollups_from(bucket_ms, true)
    }

    /// Agrega en rollups SOLO los eventos nuevos (rowid > `rollup_max_rowid`),
    /// acumulando `count` sobre los buckets existentes (UPSERT). O(delta), no
    /// O(N). Para `sync`/`add` cuando el índice solo creció (sin divergencia).
    pub fn build_rollups_sync(&self, bucket_ms: i64) -> Result<(usize, usize)> {
        self.build_rollups_from(bucket_ms, false)
    }

    /// Núcleo común de rollups. `reset=true` vacía las tablas y barre todo;
    /// `reset=false` parte del watermark `rollup_max_rowid` y acumula el delta.
    /// En ambos casos el UPSERT por `(source_id, template_id, bucket)` suma los
    /// conteos, así que repetir un `sync` sobre las mismas filas es seguro
    /// siempre que el watermark avance (lo hace: queda en MAX(rowid) al final).
    fn build_rollups_from(&self, bucket_ms: i64, reset: bool) -> Result<(usize, usize)> {
        use std::collections::BTreeMap;
        if bucket_ms <= 0 {
            return Err("build_rollups: bucket_ms debe ser > 0".into());
        }
        // Incremental sin watermark (nunca construidos, o eventos borrados
        // después): sumar desde 0 sobre lo que haya contaría dos veces.
        let wm: Option<i64> = self.meta("rollup_max_rowid")?.and_then(|v| v.parse().ok());
        let reset = reset || wm.is_none();
        let since_rowid = if reset { 0 } else { wm.unwrap_or(0) };
        let drb = crate::DIM_ROLLUP_BUCKET_MS;

        // Fase 1: escanear eventos (solo el delta si no es reset) y agregar en
        // memoria. `rollups`: (source_ref, template, bucket) -> (count, last_rid),
        // solo eventos con tiempo. `dim_rollups`: además kind, nivel, nivel
        // efectivo y entidad, TODOS los eventos (los sin tiempo en el bucket -1).
        type DimKey = (i64, String, String, String, String, i64, i64);
        let mut agg: BTreeMap<(i64, String, i64), (i64, i64)> = BTreeMap::new();
        let mut dagg: BTreeMap<DimKey, (i64, i64)> = BTreeMap::new();
        {
            // ORDER BY e.rid conserva el determinismo del last_rid. El LEFT JOIN
            // cuenta los touches del evento.
            let mut stmt = self.conn.prepare(
                "SELECT e.rid, e.source_ref, e.kind, e.level,
                        COALESCE(NULLIF(e.level, ''), json_extract(e.attrs, '$.level_inferred'), ''),
                        e.at_epoch, e.title, COUNT(t.entity_id), COALESCE(MIN(t.entity_id), 0)
                 FROM events e
                 LEFT JOIN touches t ON t.event_rid = e.rid
                 WHERE e.rid > ?1 GROUP BY e.rid ORDER BY e.rid",
            )?;
            let mut rows = stmt.query(params![since_rowid])?;
            while let Some(r) = rows.next()? {
                let rid: i64 = r.get(0)?;
                let source_ref: i64 = r.get(1)?;
                let kind: String = r.get(2)?;
                let level: String = r.get(3)?;
                let level_inf: String = r.get(4)?;
                let at_epoch: i64 = r.get(5)?;
                let title: String = r.get(6)?;
                let n_touches: i64 = r.get(7)?;
                let one_entity: i64 = r.get(8)?;
                let template = chrono_core::templatize(&title);
                // Un commit recibe touches de símbolos después: nunca por rollup.
                let entity = match n_touches {
                    _ if kind == "commit" => -1,
                    0 => 0,
                    1 => one_entity,
                    _ => -1,
                };
                let dbucket = if at_epoch <= 0 { -1 } else { at_epoch - at_epoch.rem_euclid(drb) };
                let d = dagg.entry((source_ref, template.clone(), kind, level, level_inf, entity, dbucket)).or_insert((0, 0));
                d.0 += 1;
                d.1 = rid;
                if at_epoch > 0 {
                    let bucket = at_epoch - at_epoch.rem_euclid(bucket_ms);
                    let entry = agg.entry((source_ref, template, bucket)).or_insert((0, 0));
                    entry.0 += 1;
                    entry.1 = rid; // último (iteración en orden de rid → determinista)
                }
            }
        }

        // Fase 2: UPSERT de templates + rollups. En reset se vacían primero (no
        // habrá conflictos); en incremental el ON CONFLICT suma sobre lo previo.
        let tx = self.conn.unchecked_transaction()?;
        if reset {
            tx.execute("DELETE FROM rollups", [])?;
            tx.execute("DELETE FROM dim_rollups", [])?;
            tx.execute("DELETE FROM templates", [])?;
        }
        let mut n_templates = 0usize;
        let mut n_rollups = 0usize;
        {
            let mut upsert_tpl = tx.prepare(
                "INSERT INTO templates(source_id, template) VALUES (?1, ?2)
                 ON CONFLICT(source_id, template) DO NOTHING",
            )?;
            let mut tpl_id = tx.prepare("SELECT id FROM templates WHERE source_id = ?1 AND template = ?2")?;
            let mut ins_rollup = tx.prepare(
                "INSERT INTO rollups(source_ref, template_id, bucket_epoch, count, last_rid)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(source_ref, template_id, bucket_epoch) DO UPDATE SET
                    count = count + excluded.count,
                    last_rid = excluded.last_rid",
            )?;
            let mut ins_dim = tx.prepare(
                "INSERT INTO dim_rollups(source_ref, template_id, kind, level, level_inf, entity_id, bucket_epoch, count, last_rid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(source_ref, template_id, kind, level, level_inf, entity_id, bucket_epoch) DO UPDATE SET
                    count = count + excluded.count,
                    last_rid = excluded.last_rid",
            )?;
            let mut source_ids: BTreeMap<i64, String> = BTreeMap::new();
            {
                let mut st = tx.prepare("SELECT ref, id FROM sources")?;
                let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
                for row in rows {
                    let (r, id) = row?;
                    source_ids.insert(r, id);
                }
            }
            let mut tpl_cache: std::collections::HashMap<(String, String), i64> = std::collections::HashMap::new();
            let mut tpl = |source_id: &str, template: &str, n: &mut usize| -> Result<i64> {
                if let Some(&id) = tpl_cache.get(&(source_id.to_string(), template.to_string())) {
                    return Ok(id);
                }
                if upsert_tpl.execute(params![source_id, template])? > 0 {
                    *n += 1;
                }
                let id: i64 = tpl_id.query_row(params![source_id, template], |r| r.get(0))?;
                tpl_cache.insert((source_id.to_string(), template.to_string()), id);
                Ok(id)
            };
            for ((source_ref, template, bucket), (count, last_rid)) in &agg {
                let source_id = source_ids.get(source_ref).map(String::as_str).unwrap_or("");
                let id = tpl(source_id, template, &mut n_templates)?;
                ins_rollup.execute(params![source_ref, id, bucket, count, last_rid])?;
                n_rollups += 1;
            }
            for ((source_ref, template, kind, level, level_inf, entity, bucket), (count, last_rid)) in &dagg {
                let source_id = source_ids.get(source_ref).map(String::as_str).unwrap_or("");
                let id = tpl(source_id, template, &mut n_templates)?;
                ins_dim.execute(params![source_ref, id, kind, level, level_inf, entity, bucket, count, last_rid])?;
            }
        }
        // Watermark al tope actual: el próximo sync parte de aquí.
        let max_rowid: i64 =
            tx.query_row("SELECT COALESCE(MAX(rid), 0) FROM events", [], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES ('rollup_max_rowid', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![max_rowid.to_string()],
        )?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES ('dim_rollup_bucket_ms', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![drb.to_string()],
        )?;
        tx.commit()?;
        Ok((n_templates, n_rollups))
    }

    /// Reconstruye `events_fts` (contenido externo) desde cero. Idempotente
    /// (vacía antes). Deja el watermark `fts_max_rowid` en el máximo actual para
    /// que los `sync` siguientes sean incrementales ([`Store::fts_sync`]). Úsalo
    /// en `init` y tras una divergencia (donde los rowids se reasignan).
    pub fn rebuild_fts(&self) -> Result<()> {
        self.conn.execute("INSERT INTO events_fts(events_fts) VALUES ('delete-all')", [])?;
        self.conn.execute(
            "INSERT INTO events_fts(rowid, title, body) SELECT rowid, title, body FROM events",
            [],
        )?;
        self.set_fts_watermark()?;
        Ok(())
    }

    /// Indexa en `events_fts` SOLO los eventos nuevos (rowid > watermark), sin
    /// reescribir todo el índice. Para `sync`/`add` cuando no hubo divergencia
    /// (los rowids solo crecen). O(delta), no O(N).
    pub fn fts_sync(&self) -> Result<()> {
        let wm: i64 = self
            .meta("fts_max_rowid")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO events_fts(rowid, title, body)
             SELECT rowid, title, body FROM events WHERE rowid > ?1",
            params![wm],
        )?;
        self.set_fts_watermark()?;
        Ok(())
    }

    /// Fija `meta.fts_max_rowid` al máximo rowid actual de `events`.
    fn set_fts_watermark(&self) -> Result<()> {
        let max: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(rowid), 0) FROM events", [], |r| r.get(0))?;
        self.set_meta("fts_max_rowid", &max.to_string())
    }

    /// Cierre ligero tras la ingesta: FTS `optimize` + estadísticas +
    /// `wal_checkpoint(TRUNCATE)`. **Sin `VACUUM`**: a escala (≥10M) el VACUUM
    /// copiaba el `.db` entero en RAM (OOM) y costaba segundos en cada `sync`.
    /// Para compactar el fichero explícitamente, usa [`Store::compact`].
    ///
    /// `full_analyze`: en `init`/reconstrucción completa se hace `ANALYZE`
    /// entero (estadísticas deterministas → `.db` byte-reproducible). En un
    /// `sync` incremental se usa `PRAGMA optimize`, que solo re-analiza las
    /// tablas cuyas estadísticas quedaron obsoletas: a 2M filas un `ANALYZE`
    /// completo costaba ~15 s en CADA sync (O(N)); `PRAGMA optimize` es gratis
    /// cuando el delta es pequeño y mantiene buenos planes al crecer el índice.
    pub fn optimize(&self, full_analyze: bool) -> Result<()> {
        self.conn.execute("INSERT INTO events_fts(events_fts) VALUES ('optimize')", [])?;
        if full_analyze {
            self.conn.execute_batch("ANALYZE;")?;
        } else {
            self.conn.execute_batch("PRAGMA optimize;")?;
        }
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Compacta el fichero (`VACUUM`), recuperando páginas libres. Opcional y
    /// explícito (`chrono init --compact`): a gran escala es caro en tiempo y
    /// memoria, por eso NO se hace en cada ingesta.
    pub fn compact(&self) -> Result<()> {
        self.conn.execute_batch("VACUUM;")?;
        Ok(())
    }
}

/// v6 → v7: `links(event_rid, rel, target)` pasa a `link_targets` + `event_links`
/// (cada target una vez, unido por entero) y `links` queda como vista. Es la
/// única diferencia entre las dos versiones: no hace falta reindexar.
///
/// Dos procesos que abren a la vez el mismo índice v6 (los hooks de varios
/// worktrees) leen los dos `6`: el segundo esperaba al primero en `BEGIN
/// IMMEDIATE` y luego fallaba renombrando una `links` que ya era vista. Ahora la
/// versión se relee dentro de la transacción (si otro ya migró, no hay nada que
/// hacer) y la espera no corta a los 5 s del `busy_timeout`: copiar los links de
/// un índice grande tarda más.
fn migrate_v6_links(conn: &Connection) -> Result<()> {
    let wait = MIGRATION_WAIT_MS.with(std::cell::Cell::get);
    conn.busy_timeout(std::time::Duration::from_millis(wait))?;
    let r = migrate_v6_links_locked(conn);
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    // Tras la espera, un «database is locked» a secas no decía qué pasaba.
    r.map_err(|e| {
        if is_busy(&*e) {
            format!(
                "otro proceso sigue migrando el índice (v6 → v7) tras {} s de espera: deja que acabe y repite ({e})",
                wait / 1000
            )
            .into()
        } else {
            e
        }
    })
}

thread_local! {
    /// Cuánto espera la migración v6 → v7 a otra que esté en curso (ms; los
    /// tests lo acortan en su hilo).
    pub(crate) static MIGRATION_WAIT_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(600_000) };
}

fn migrate_v6_links_locked(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let version: Option<String> =
        conn.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).optional()?;
    if version.as_deref() != Some("6") {
        conn.execute_batch("COMMIT")?;
        return Ok(());
    }
    let r = conn.execute_batch(
        "ALTER TABLE links RENAME TO links_v6;
         CREATE TABLE link_targets (
             id     INTEGER PRIMARY KEY,
             rel    TEXT NOT NULL,
             target TEXT NOT NULL,
             UNIQUE (rel, target)
         );
         CREATE TABLE event_links (
             event_rid INTEGER NOT NULL REFERENCES events(rid),
             target_id INTEGER NOT NULL REFERENCES link_targets(id),
             PRIMARY KEY (event_rid, target_id)
         ) WITHOUT ROWID;
         INSERT INTO link_targets(rel, target) SELECT DISTINCT rel, target FROM links_v6 ORDER BY rel, target;
         INSERT INTO event_links(event_rid, target_id)
             SELECT l.event_rid, t.id FROM links_v6 l JOIN link_targets t ON t.rel = l.rel AND t.target = l.target;
         DROP TABLE links_v6;
         CREATE VIEW links AS
             SELECT l.event_rid AS event_rid, t.rel AS rel, t.target AS target
             FROM event_links l JOIN link_targets t ON t.id = l.target_id;
         CREATE INDEX idx_event_links_target ON event_links(target_id, event_rid);
         CREATE INDEX idx_link_targets_target ON link_targets(target);
         UPDATE meta SET value = '7' WHERE key = 'schema_version';
         COMMIT;",
    );
    if r.is_err() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    Ok(r?)
}

/// Error de SQLite «ocupado» o «bloqueado».
fn is_busy(e: &(dyn std::error::Error + 'static)) -> bool {
    e.downcast_ref::<rusqlite::Error>()
        .and_then(rusqlite::Error::sqlite_error_code)
        .is_some_and(|c| matches!(c, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}
