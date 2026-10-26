//! `timeline`: conteo de eventos por bucket temporal, opcionalmente desglosado
//! por una dimensión de baja cardinalidad (`level`|`kind`). Consulta log-native
//! (R3 oleada 2). Ver `docs/DESIGN-GENERAL-CORE.md §3`.
//!
//! Dos caminos, con la MISMA salida byte a byte:
//! - Camino `events` (histórico): escanea `events` y agrupa por bucket. Es el
//!   único que sirve el desglose `--by level|kind`, los buckets sub-minuto y
//!   las ventanas no alineadas al bucket de rollup.
//! - Camino `rollups` (P3, escala): cuando NO hay desglose, el bucket pedido es
//!   múltiplo del bucket de rollup y la ventana está alineada a él, el total por
//!   bucket se obtiene SUMANDO los `rollups` precomputados (tabla pequeña:
//!   plantillas × buckets) en vez de barrer millones de `events`. Solo se activa
//!   si los rollups reflejan TODOS los eventos con tiempo (watermark al día); en
//!   cualquier duda se cae al camino `events`. Ver `timeline_from_rollups`.

use rusqlite::{params, Connection, OptionalExtension};

use crate::Result;

/// Un bucket temporal `[bucket_epoch, bucket_epoch + bucket_secs)`.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineBucket {
    /// Inicio del bucket en epoch UTC, alineado a un múltiplo de `bucket_secs`
    /// (`at_epoch - at_epoch.rem_euclid(bucket_secs)`).
    pub bucket_epoch: i64,
    /// Valor del desglose (`level`/`kind`) o "" si `group_by` es `None`.
    pub key: String,
    pub count: i64,
}

/// Conteo de eventos por bucket temporal.
///
/// - `since_epoch`/`until_epoch`: ventana `[since, until)` en epoch UTC; `0` en
///   cualquiera de los dos = sin ese límite (`until_epoch == 0` significa "hasta
///   el final").
/// - `bucket_secs`: tamaño de bucket en segundos (debe ser > 0).
/// - `group_by`: `Some("level")` | `Some("kind")` desglosa por esa columna
///   nativa; `None` cuenta todos los eventos del bucket juntos (`key=""`).
///
/// Orden determinista: por `bucket_epoch` ASC, luego `key` ASC. Solo cuenta
/// eventos con `at_epoch > 0` (los eventos sin tiempo válido no entran).
///
/// `limit` acota el nº de filas devueltas (la salida es "bounded for an AI"):
/// se aplica DESPUÉS del orden, así que se conservan los buckets más antiguos;
/// para acotar por tiempo usa `since_epoch`/`until_epoch`. `limit == 0` = sin
/// tope (para llamadas internas que quieran todo).
pub fn timeline(
    conn: &Connection,
    since_epoch: i64,
    until_epoch: i64,
    bucket_secs: i64,
    group_by: Option<&str>,
    limit: usize,
) -> Result<Vec<TimelineBucket>> {
    if bucket_secs <= 0 {
        return Err("timeline: bucket_secs must be > 0".into());
    }
    // Columna nativa para el desglose; "" (literal SQL) cuenta todo junto.
    // Solo se acepta un allowlist fijo de dos valores, así que interpolar el
    // nombre de columna en el SQL es seguro (no llega texto de usuario a la
    // sentencia).
    let key_expr = match group_by {
        None => "''",
        Some("level") => "level",
        Some("kind") => "kind",
        Some(other) => {
            return Err(format!("timeline: unsupported group_by: {other}").into());
        }
    };

    // P3 (escala): sin desglose, intenta servir los conteos desde `rollups`
    // (tabla pequeña) en vez de barrer `events`. Devuelve `Some` solo cuando es
    // EXACTAMENTE equivalente al camino de events (ver `timeline_from_rollups`);
    // `None` => cae al camino de events de abajo, sin cambiar nada.
    if group_by.is_none() {
        if let Some(rows) = timeline_from_rollups(conn, since_epoch, until_epoch, bucket_secs, limit)? {
            return Ok(rows);
        }
    }

    // `at_epoch - ((at_epoch % ?1 + ?1) % ?1)` reproduce
    // `at_epoch - at_epoch.rem_euclid(bucket_secs)` en SQL: el `%` de SQLite
    // sigue el signo del dividendo (como el `%` de Rust), así que se normaliza
    // a resto no negativo a mano antes de restarlo.
    let sql = format!(
        "SELECT bucket_epoch, key, COUNT(*) AS count FROM (
             SELECT at_epoch - ((at_epoch % ?1 + ?1) % ?1) AS bucket_epoch,
                    {key_expr} AS key
             FROM events
             WHERE at_epoch > 0
               AND (?2 = 0 OR at_epoch >= ?2)
               AND (?3 = 0 OR at_epoch < ?3)
         )
         GROUP BY bucket_epoch, key
         ORDER BY bucket_epoch ASC, key ASC
         LIMIT ?4"
    );

    // `limit == 0` -> sin tope (SQLite trata LIMIT -1 como ilimitado).
    let sql_limit: i64 = if limit == 0 { -1 } else { limit as i64 };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![bucket_secs, since_epoch, until_epoch, sql_limit], |r| {
        Ok(TimelineBucket { bucket_epoch: r.get(0)?, key: r.get(1)?, count: r.get(2)? })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Qué eventos cuenta una consulta acotada (`timeline`, `stats`, `top`,
/// `patterns`, `events`). Todos opcionales; vacío = sin filtro. Las mismas
/// reglas en todas, para que CLI y MCP filtren igual:
/// - `source`: tipo de fuente (`jsonl`, `cmd`…) o prefijo de su id.
/// - `kind`: exacto, o prefijo con `*` (`ci.*`).
/// - `level`: uno o varios separados por coma (`FATAL,ERROR`), sin distinguir
///   mayúsculas.
/// - `grep`: subcadena del título (sin distinguir mayúsculas en ASCII).
/// - `entity`: una o varias entidades exactas separadas por coma (el servicio
///   en logs, el fichero en git).
/// - `attr`: `clave:valor` sobre los atributos del evento (p. ej.
///   `container:crm_aura_sandbox_postgres`); varios separados por coma se
///   exigen todos. El valor admite `*` al final como prefijo.
/// - `infer_level`: el filtro `level` casa también las líneas SIN nivel de la
///   fuente cuyo texto lo dice (`attrs.level_inferred`, ver `chrono_core::infer_level`).
/// - `until_epoch`: fin de la ventana en ms (exclusivo; 0 = sin fin).
#[derive(Debug, Clone, Default)]
pub struct EventScope<'a> {
    pub source: Option<&'a str>,
    pub grep: Option<&'a str>,
    pub level: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub entity: Option<&'a str>,
    pub attr: Option<&'a str>,
    pub infer_level: bool,
    pub until_epoch: i64,
}

/// Expresión SQL del nivel efectivo de un evento: el de la fuente, o el
/// deducido del texto si la fuente no lo trae y se pidió `infer_level`.
pub(crate) fn level_expr(t: &str, infer: bool) -> String {
    if infer {
        format!("COALESCE(NULLIF({t}.level, ''), json_extract({t}.attrs, '$.level_inferred'), '')")
    } else {
        format!("{t}.level")
    }
}

/// `,A,B,` en mayúsculas y sin espacios: el parámetro de una lista separada por
/// comas, para casar con `instr(?, ',' || x || ',')`.
fn comma_list(raw: &str, upper: bool) -> String {
    let items: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| if upper { s.to_uppercase() } else { s.to_string() })
        .collect();
    format!(",{},", items.join(","))
}

impl EventScope<'_> {
    pub fn is_empty(&self) -> bool {
        self.until_epoch <= 0
            && [self.source, self.grep, self.level, self.kind, self.entity, self.attr].iter().all(|v| v.map_or(true, str::is_empty))
    }

    /// Solo filtra por fuente y/o texto: lo único que saben responder los
    /// rollups de `patterns`.
    pub fn only_source_grep(&self) -> bool {
        self.until_epoch <= 0 && [self.level, self.kind, self.entity, self.attr].iter().all(|v| v.map_or(true, str::is_empty))
    }

    /// Como `sql`, para otros crates (la cabecera del MCP cuenta con los mismos filtros).
    pub fn sql_public(&self, first: usize) -> (String, Vec<String>) {
        self.sql_on("e", first)
    }

    /// `(cláusulas SQL sobre el alias `e`, parámetros)`, numerados desde `first`.
    pub(crate) fn sql(&self, first: usize) -> (String, Vec<String>) {
        self.sql_on("e", first)
    }

    /// Como [`EventScope::sql`], sobre la tabla o alias `t` de `events`.
    pub(crate) fn sql_on(&self, t: &str, first: usize) -> (String, Vec<String>) {
        let mut sql = String::new();
        let mut params: Vec<String> = Vec::new();
        let mut push = |clause: &str, value: String| {
            let n = first + params.len();
            sql.push_str(&format!(" AND {}", clause.replace("?N", &format!("?{n}")).replace("$t", t)));
            params.push(value);
        };
        if let Some(s) = self.source.filter(|s| !s.trim().is_empty()) {
            let items: Vec<&str> = s.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
            if items.len() > 1 && items.iter().all(|x| !x.contains(':')) {
                // Lista de kinds (`delega,nod`): cualquiera de ellos.
                push("$t.source_ref IN (SELECT ref FROM sources WHERE instr(?N, ',' || kind || ',') > 0)", comma_list(s, false));
            } else {
                let like = if s.contains(':') { format!("{s}%") } else { format!("{s}:%") };
                push("$t.source_ref IN (SELECT ref FROM sources WHERE id LIKE ?N)", like);
            }
        }
        if let Some(g) = self.grep.filter(|g| !g.is_empty()) {
            push("$t.title LIKE ?N", format!("%{g}%"));
        }
        if let Some(l) = self.level.filter(|l| !l.trim().is_empty()) {
            let expr = level_expr("$t", self.infer_level);
            push(&format!("instr(?N, ',' || UPPER({expr}) || ',') > 0"), comma_list(l, true));
        }
        if let Some(k) = self.kind.filter(|k| k.contains(',')) {
            // Lista (`delega.*,nod.job.*`): exactos o prefijos con `*`, cualquiera de ellos.
            let items: Vec<String> = k
                .split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|x| format!("\"{}\"", x.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect();
            push(
                "EXISTS (SELECT 1 FROM json_each(?N) j WHERE (substr(j.value, -1) = '*' AND $t.kind LIKE substr(j.value, 1, length(j.value) - 1) || '%') OR $t.kind = j.value)",
                format!("[{}]", items.join(",")),
            );
        } else if let Some(k) = self.kind.filter(|k| !k.is_empty()) {
            match k.strip_suffix('*') {
                Some(prefix) => push("$t.kind LIKE ?N", format!("{prefix}%")),
                None => push("$t.kind = ?N", k.to_string()),
            }
        }
        if let Some(en) = self.entity.filter(|e| !e.trim().is_empty()) {
            push(
                "EXISTS (SELECT 1 FROM touches tq JOIN entities enq ON enq.id = tq.entity_id
                         WHERE tq.event_id = $t.id AND instr(?N, ',' || enq.key || ',') > 0)",
                comma_list(en, false),
            );
        }
        for cond in self.attr.unwrap_or("").split(',').map(str::trim).filter(|c| !c.is_empty()) {
            let (k, v) = cond.split_once(':').or_else(|| cond.split_once('=')).unwrap_or((cond, ""));
            // La clave va en el path JSON: solo caracteres seguros (como en `top attr:`).
            let k = k.trim();
            if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
                continue;
            }
            let path = format!("$.{k}");
            match v.trim().strip_suffix('*') {
                Some(prefix) => {
                    push(&format!("json_extract($t.attrs, '{path}') LIKE ?N"), format!("{prefix}%"));
                }
                None => push(&format!("json_extract($t.attrs, '{path}') = ?N"), v.trim().to_string()),
            }
        }
        if self.until_epoch > 0 {
            // Texto contra columna INTEGER: SQLite le aplica la afinidad numérica.
            push("$t.at_epoch < ?N", self.until_epoch.to_string());
        }
        (sql, params)
    }
}

/// Como [`timeline`], contando solo los eventos de `scope`: «¿desde cuándo
/// aparece este error?», «¿cuántos despliegues fallidos por semana?». Siempre
/// barre `events` (los rollups no guardan nivel ni kind).
pub fn timeline_scoped(
    conn: &Connection,
    since_epoch: i64,
    until_epoch: i64,
    bucket_secs: i64,
    group_by: Option<&str>,
    scope: &EventScope,
    limit: usize,
) -> Result<Vec<TimelineBucket>> {
    if scope.is_empty() {
        return timeline(conn, since_epoch, until_epoch, bucket_secs, group_by, limit);
    }
    if bucket_secs <= 0 {
        return Err("timeline: bucket_secs must be > 0".into());
    }
    let key_expr = match group_by {
        None => "''",
        Some("level") => "e.level",
        Some("kind") => "e.kind",
        Some(other) => return Err(format!("timeline: unsupported group_by: {other}").into()),
    };
    let (filters, extra) = scope.sql(5);
    let sql = format!(
        "SELECT bucket_epoch, key, COUNT(*) AS count FROM (
             SELECT e.at_epoch - ((e.at_epoch % ?1 + ?1) % ?1) AS bucket_epoch, {key_expr} AS key
             FROM events e
             WHERE e.at_epoch > 0
               AND (?2 = 0 OR e.at_epoch >= ?2)
               AND (?3 = 0 OR e.at_epoch < ?3)
               {filters}
         )
         GROUP BY bucket_epoch, key
         ORDER BY bucket_epoch ASC, key ASC
         LIMIT ?4"
    );
    let sql_limit: i64 = if limit == 0 { -1 } else { limit as i64 };
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&bucket_secs, &since_epoch, &until_epoch, &sql_limit];
    for p in &extra {
        bound.push(p);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bound), |r| {
        Ok(TimelineBucket { bucket_epoch: r.get(0)?, key: r.get(1)?, count: r.get(2)? })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Lee un valor de `meta` como `i64`, o `None` si la clave falta o no parsea.
fn meta_i64(conn: &Connection, key: &str) -> Result<Option<i64>> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
        .optional()?;
    Ok(v.and_then(|s| s.parse::<i64>().ok()))
}

/// Camino rápido de `timeline` (solo SIN desglose): suma los `rollups`
/// precomputados en vez de barrer `events`. Devuelve `Some(filas)` cuando el
/// resultado es GARANTIZADO byte-idéntico al camino de events, o `None` cuando
/// cualquier condición no se cumple (el llamador cae al camino de events).
///
/// Por qué cada condición preserva la paridad:
/// - Los rollups solo agregan eventos con `at_epoch > 0` (igual que el camino de
///   events, que filtra `at_epoch > 0`): los eventos sin tiempo quedan fuera en
///   ambos, así que no hay discrepancia por ese borde.
/// - `rb` = tamaño del bucket de rollup (de `meta`, normalmente 60_000 ms). Cada
///   evento con tiempo cae en EXACTAMENTE un bucket de rollup; sumar sus `count`
///   equivale a contar los eventos.
/// - `bucket_ms % rb == 0`: re-bucketizar los buckets de rollup (todos múltiplos
///   de `rb`, no negativos) con `b - (b % bucket_ms)` da el MISMO bucket que
///   `at_epoch - at_epoch.rem_euclid(bucket_ms)` aplicado al evento (demostrable
///   porque `bucket_ms` es múltiplo de `rb` y los buckets son >= 0). Para buckets
///   sub-`rb` no hay forma de desagregar: `None`.
/// - `since % rb == 0 && until % rb == 0`: con la ventana alineada al bucket de
///   rollup, filtrar por `bucket_epoch` (nivel bucket) selecciona exactamente los
///   mismos eventos que filtrar por `at_epoch` (nivel evento). Una ventana que
///   parte un bucket de rollup por la mitad no es reproducible: `None`.
/// - Watermark al día (`rollup_max_rowid == MAX(events.rowid)`): no hay eventos
///   sin agregar en rollups. El índice es append-only (los `sync` acumulan el
///   delta) y una divergencia reconstruye rollups desde cero, así que watermark
///   al día implica que los rollups reflejan TODOS los eventos. Si el watermark
///   se quedó atrás (p. ej. eventos añadidos sin re-rollup), `None`: paridad por
///   encima de velocidad.
fn timeline_from_rollups(
    conn: &Connection,
    since_epoch: i64,
    until_epoch: i64,
    bucket_ms: i64,
    limit: usize,
) -> Result<Option<Vec<TimelineBucket>>> {
    // Tamaño del bucket de rollup (autoritativo en `meta`). Sin él, no sabemos
    // la granularidad precomputada: cae a events.
    let rb = match meta_i64(conn, "rollup_bucket_ms")? {
        Some(v) if v > 0 => v,
        _ => return Ok(None),
    };
    // El bucket pedido debe ser múltiplo entero del de rollup, y la ventana
    // debe estar alineada a él (0 = sin límite, también alineado).
    if bucket_ms % rb != 0 || since_epoch % rb != 0 || until_epoch % rb != 0 {
        return Ok(None);
    }
    // Rollups al día: watermark == MAX(rowid). Si `meta` no tiene watermark, los
    // rollups nunca se construyeron: cae a events.
    let wm = match meta_i64(conn, "rollup_max_rowid")? {
        Some(v) => v,
        None => return Ok(None),
    };
    let max_rowid: i64 = conn.query_row("SELECT COALESCE(MAX(rowid), 0) FROM events", [], |r| r.get(0))?;
    if wm != max_rowid {
        return Ok(None);
    }

    // `bucket_epoch >= 0` siempre (bucket de un `at_epoch > 0`), así que `%` de
    // SQLite no necesita normalización de resto negativo aquí.
    let sql_limit: i64 = if limit == 0 { -1 } else { limit as i64 };
    let mut stmt = conn.prepare(
        "SELECT bucket_epoch - (bucket_epoch % ?1) AS bucket, SUM(count) AS count
         FROM rollups
         WHERE (?2 = 0 OR bucket_epoch >= ?2)
           AND (?3 = 0 OR bucket_epoch < ?3)
         GROUP BY bucket
         ORDER BY bucket ASC
         LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![bucket_ms, since_epoch, until_epoch, sql_limit], |r| {
        // `key` siempre "" (sin desglose), igual que el camino de events con
        // `group_by == None`.
        Ok(TimelineBucket { bucket_epoch: r.get(0)?, key: String::new(), count: r.get(1)? })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono_core::{Actor, Event};
    use chrono_store::Store;

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Fichero temporal único; borra `.db`, `.db-wal` y `.db-shm` al hacer `Drop`.
    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("chrono-metrics-test-timeline-{name}-{}-{}.db", std::process::id(), n));
            let _ = std::fs::remove_file(&path);
            TempDb { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }

    fn mk_event(id: &str, source_id: &str, kind: &str, at_epoch: i64, level: &str) -> Event {
        Event {
            id: id.to_string(),
            source_id: source_id.to_string(),
            kind: kind.to_string(),
            at: format!("1970-01-01T00:{:02}:{:02}Z", (at_epoch / 60) % 60, at_epoch % 60),
            at_epoch,
            actor: Actor::default(),
            title: format!("evento {id}"),
            level: level.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn timeline_sin_group_by_alinea_buckets_y_cuenta() {
        let db = TempDb::new("sin-group");
        let mut store = Store::open(db.path()).unwrap();
        let evs = [
            mk_event("e1", "src", "commit", 5, ""),
            mk_event("e2", "src", "commit", 15, ""),
            mk_event("e3", "src", "commit", 105, ""),
            mk_event("e4", "src", "commit", 205, ""),
            mk_event("e5", "src", "commit", 208, ""),
        ];
        {
            let mut w = store.writer().unwrap();
            for e in &evs {
                w.add_event(e).unwrap();
            }
            w.commit().unwrap();
        }

        let buckets = timeline(store.conn(), 0, 0, 100, None, 0).unwrap();
        assert_eq!(
            buckets,
            vec![
                TimelineBucket { bucket_epoch: 0, key: String::new(), count: 2 },
                TimelineBucket { bucket_epoch: 100, key: String::new(), count: 1 },
                TimelineBucket { bucket_epoch: 200, key: String::new(), count: 2 },
            ]
        );
    }

    #[test]
    fn timeline_con_group_by_desglosa_por_columna_y_ordena_por_bucket_y_key() {
        let db = TempDb::new("con-group");
        let mut store = Store::open(db.path()).unwrap();
        let evs = [
            mk_event("e1", "src", "commit", 10, "ERROR"),
            mk_event("e2", "src", "log", 20, "INFO"),
            mk_event("e3", "src", "commit", 30, "ERROR"),
            mk_event("e4", "src", "log", 110, "WARN"),
        ];
        {
            let mut w = store.writer().unwrap();
            for e in &evs {
                w.add_event(e).unwrap();
            }
            w.commit().unwrap();
        }
        let conn = store.conn();

        let by_level = timeline(conn, 0, 0, 100, Some("level"), 0).unwrap();
        assert_eq!(
            by_level,
            vec![
                TimelineBucket { bucket_epoch: 0, key: "ERROR".to_string(), count: 2 },
                TimelineBucket { bucket_epoch: 0, key: "INFO".to_string(), count: 1 },
                TimelineBucket { bucket_epoch: 100, key: "WARN".to_string(), count: 1 },
            ]
        );

        let by_kind = timeline(conn, 0, 0, 100, Some("kind"), 0).unwrap();
        assert_eq!(
            by_kind,
            vec![
                TimelineBucket { bucket_epoch: 0, key: "commit".to_string(), count: 2 },
                TimelineBucket { bucket_epoch: 0, key: "log".to_string(), count: 1 },
                TimelineBucket { bucket_epoch: 100, key: "log".to_string(), count: 1 },
            ]
        );
    }

    #[test]
    fn timeline_respeta_ventana_since_until_y_excluye_at_epoch_no_positivo() {
        let db = TempDb::new("ventana");
        let mut store = Store::open(db.path()).unwrap();
        let evs = [
            mk_event("e0", "src", "commit", 0, ""), // at_epoch inválido: fuera siempre.
            mk_event("e1", "src", "commit", 50, ""),
            mk_event("e2", "src", "commit", 150, ""),
            mk_event("e3", "src", "commit", 250, ""),
        ];
        {
            let mut w = store.writer().unwrap();
            for e in &evs {
                w.add_event(e).unwrap();
            }
            w.commit().unwrap();
        }
        let conn = store.conn();

        // [100, 250): incluye e2 (150), excluye e1 (50) y e3 (250, límite excluyente).
        let windowed = timeline(conn, 100, 250, 1000, None, 0).unwrap();
        let total: i64 = windowed.iter().map(|b| b.count).sum();
        assert_eq!(total, 1);

        // Sin límites: entran e1, e2, e3 (3), e0 queda fuera por at_epoch<=0.
        let unbounded = timeline(conn, 0, 0, 1000, None, 0).unwrap();
        let total_unbounded: i64 = unbounded.iter().map(|b| b.count).sum();
        assert_eq!(total_unbounded, 3);
    }

    #[test]
    fn timeline_rechaza_bucket_secs_invalido_y_group_by_no_soportado() {
        let db = TempDb::new("errores");
        let store = Store::open(db.path()).unwrap();
        let conn = store.conn();

        assert!(timeline(conn, 0, 0, 0, None, 0).is_err());
        assert!(timeline(conn, 0, 0, -10, None, 0).is_err());
        assert!(timeline(conn, 0, 0, 100, Some("actor"), 0).is_err());
    }

    #[test]
    fn timeline_limit_acota_conservando_los_buckets_mas_antiguos() {
        let db = TempDb::new("limit");
        let mut store = Store::open(db.path()).unwrap();
        let evs = [
            mk_event("e1", "src", "commit", 5, ""),   // bucket 0
            mk_event("e2", "src", "commit", 105, ""), // bucket 100
            mk_event("e3", "src", "commit", 205, ""), // bucket 200
        ];
        {
            let mut w = store.writer().unwrap();
            for e in &evs {
                w.add_event(e).unwrap();
            }
            w.commit().unwrap();
        }
        let conn = store.conn();

        // limit 2 -> los dos buckets más antiguos (0 y 100), no el 200.
        let capped = timeline(conn, 0, 0, 100, None, 2).unwrap();
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].bucket_epoch, 0);
        assert_eq!(capped[1].bucket_epoch, 100);

        // limit 0 -> sin tope (los tres).
        assert_eq!(timeline(conn, 0, 0, 100, None, 0).unwrap().len(), 3);
    }
}

/// P3: camino rápido de `timeline` desde `rollups`. La invariante es PARIDAD
/// byte-idéntica con el camino de events; estos tests bloquean regresiones
/// tanto de la equivalencia como de las condiciones de caída a events.
#[cfg(test)]
mod tests_p3 {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono_core::{Actor, Event};
    use chrono_store::Store;
    use rusqlite::params;

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        path: PathBuf,
    }
    impl TempDb {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("chrono-metrics-test-tlp3-{name}-{}-{}.db", std::process::id(), n));
            let _ = std::fs::remove_file(&path);
            TempDb { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            for s in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{s}", self.path.display()));
            }
        }
    }

    const RB: i64 = 60_000; // bucket de rollup (1 min en ms)

    fn ev(id: &str, at_epoch: i64, level: &str) -> Event {
        Event {
            id: id.to_string(),
            source_id: "jsonl:/log".to_string(),
            kind: "log".to_string(),
            at: if at_epoch > 0 { format!("ts{at_epoch}") } else { String::new() },
            at_epoch,
            actor: Actor::default(),
            // Título variado para que haya varias plantillas por bucket (como un
            // log real): el camino de rollups debe sumar TODAS las plantillas.
            title: format!("request took {id} ms"),
            level: level.to_string(),
            ..Default::default()
        }
    }

    /// Store con eventos repartidos en varios buckets de minuto, un par de
    /// eventos sin tiempo (`at_epoch=0`, borde), rollups construidos y el
    /// `rollup_bucket_ms` en `meta` (lo pone el CLI en `finalize`, no
    /// `build_rollups`). Devuelve también el nº de eventos para aserciones.
    fn fixture(name: &str) -> (TempDb, Store) {
        let db = TempDb::new(name);
        let mut store = Store::open(db.path()).unwrap();
        {
            let mut w = store.writer().unwrap();
            // Minuto 0: 2, minuto 1: 3, minuto 5: 1; distintos niveles.
            w.add_event(&ev("a", RB + 1_000, "ERROR")).unwrap();
            w.add_event(&ev("b", RB + 2_000, "INFO")).unwrap();
            w.add_event(&ev("c", 2 * RB + 500, "WARN")).unwrap();
            w.add_event(&ev("d", 2 * RB + 40_000, "INFO")).unwrap();
            w.add_event(&ev("e", 2 * RB + 59_999, "ERROR")).unwrap();
            w.add_event(&ev("f", 6 * RB + 123, "INFO")).unwrap();
            // Eventos SIN tiempo: deben quedar fuera de timeline Y de rollups.
            w.add_event(&ev("z1", 0, "TRACE")).unwrap();
            w.add_event(&ev("z2", 0, "TRACE")).unwrap();
            w.commit().unwrap();
        }
        store.build_rollups(RB).unwrap();
        store.set_meta("rollup_bucket_ms", &RB.to_string()).unwrap();
        (db, store)
    }

    /// Fuerza el camino de events quitando `rollup_bucket_ms` de `meta`, para
    /// capturar el resultado de referencia del MISMO dato.
    fn via_events(store: &Store, since: i64, until: i64, bucket: i64) -> Vec<TimelineBucket> {
        store.conn().execute("DELETE FROM meta WHERE key = 'rollup_bucket_ms'", []).unwrap();
        let out = timeline(store.conn(), since, until, bucket, None, 0).unwrap();
        store.set_meta("rollup_bucket_ms", &RB.to_string()).unwrap();
        out
    }

    #[test]
    fn rollups_identico_a_events_en_bucket_multiplo_y_ventana_alineada() {
        let (_db, store) = fixture("parity");
        let conn = store.conn();
        // Para varios buckets múltiplos de RB y ventanas alineadas, el resultado
        // del camino de rollups debe ser EXACTAMENTE el del camino de events.
        for bucket in [RB, 5 * RB, 60 * RB] {
            for (since, until) in [(0, 0), (RB, 0), (0, 3 * RB), (RB, 6 * RB)] {
                let got = timeline(conn, since, until, bucket, None, 0).unwrap();
                let expected = via_events(&store, since, until, bucket);
                assert_eq!(got, expected, "bucket={bucket} since={since} until={until}");
            }
        }
    }

    #[test]
    fn rollups_excluyen_eventos_sin_tiempo_igual_que_events() {
        let (_db, store) = fixture("timeless");
        // 6 eventos con tiempo, 2 sin tiempo. El total de timeline (ambos
        // caminos) debe ser 6: los `at_epoch=0` quedan fuera en los dos.
        let total: i64 = timeline(store.conn(), 0, 0, RB, None, 0).unwrap().iter().map(|b| b.count).sum();
        assert_eq!(total, 6);
    }

    #[test]
    fn usa_rollups_cuando_elegible() {
        let (_db, store) = fixture("uses");
        // Inflar el bucket 2*RB sin tocar `events` (watermark sigue al día): si
        // timeline lee rollups, verá el inflado; si leyera events, daría 3.
        // El bucket puede tener varias filas de rollup (una por plantilla), así
        // que el delta es 100 × nº de filas de ese bucket.
        let n_rows: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM rollups WHERE bucket_epoch = ?1", params![2 * RB], |r| r.get(0))
            .unwrap();
        store
            .conn()
            .execute("UPDATE rollups SET count = count + 100 WHERE bucket_epoch = ?1", params![2 * RB])
            .unwrap();
        let rows = timeline(store.conn(), 0, 0, RB, None, 0).unwrap();
        let bucket2 = rows.iter().find(|b| b.bucket_epoch == 2 * RB).unwrap();
        assert_eq!(bucket2.count, 3 + 100 * n_rows, "3 reales + inyectado => leyó rollups");
    }

    #[test]
    fn sub_minuto_cae_a_events() {
        let (_db, store) = fixture("submin");
        // Inflar rollups: si cayera a events (correcto), NO se ve el inflado.
        store.conn().execute("UPDATE rollups SET count = count + 100", []).unwrap();
        // 30s no es múltiplo de RB (60s) => camino de events => conteos reales.
        let rows = timeline(store.conn(), 0, 0, 30_000, None, 0).unwrap();
        let total: i64 = rows.iter().map(|b| b.count).sum();
        assert_eq!(total, 6, "sub-minuto debe barrer events (6 reales), no rollups");
    }

    #[test]
    fn ventana_no_alineada_cae_a_events() {
        let (_db, store) = fixture("unaligned");
        store.conn().execute("UPDATE rollups SET count = count + 100", []).unwrap();
        // since no alineado a RB => no se puede filtrar por bucket => events.
        let rows = timeline(store.conn(), RB + 30_000, 0, RB, None, 0).unwrap();
        let total: i64 = rows.iter().map(|b| b.count).sum();
        // Reales con at_epoch >= 90_000: c(120500), d(160000), e(179999), f => 4.
        assert_eq!(total, 4, "ventana no alineada debe barrer events");
    }

    #[test]
    fn desglose_by_level_cae_a_events() {
        let (_db, store) = fixture("bylevel");
        store.conn().execute("UPDATE rollups SET count = count + 100", []).unwrap();
        // Con --by level, los rollups no sirven: debe desglosar desde events.
        let rows = timeline(store.conn(), 0, 0, RB, Some("level"), 0).unwrap();
        let total: i64 = rows.iter().map(|b| b.count).sum();
        assert_eq!(total, 6, "desglose por nivel debe barrer events");
        assert!(rows.iter().all(|b| !b.key.is_empty()), "cada fila lleva su nivel");
    }

    #[test]
    fn watermark_desfasado_cae_a_events() {
        let (_db, store) = fixture("stale");
        store.conn().execute("UPDATE rollups SET count = count + 100", []).unwrap();
        // Simula rollups desfasados: watermark por detrás de MAX(rowid).
        store.set_meta("rollup_max_rowid", "0").unwrap();
        let rows = timeline(store.conn(), 0, 0, RB, None, 0).unwrap();
        let total: i64 = rows.iter().map(|b| b.count).sum();
        assert_eq!(total, 6, "watermark desfasado => camino de events (conteos reales)");
    }

    #[test]
    fn sin_rollup_bucket_ms_cae_a_events() {
        let (_db, store) = fixture("nometa");
        store.conn().execute("UPDATE rollups SET count = count + 100", []).unwrap();
        store.conn().execute("DELETE FROM meta WHERE key = 'rollup_bucket_ms'", []).unwrap();
        let rows = timeline(store.conn(), 0, 0, RB, None, 0).unwrap();
        let total: i64 = rows.iter().map(|b| b.count).sum();
        assert_eq!(total, 6, "sin rollup_bucket_ms no se usan rollups");
    }
}
