//! `top <dim>`: valores más frecuentes de una dimensión. Consulta log-native
//! (R3 oleada 2). Lee `events`/`touches`/`actors` crudos. Ver
//! `docs/DESIGN-GENERAL-CORE.md §3`.

use rusqlite::{params, Connection};

use crate::Result;

/// Un valor de la dimensión con su frecuencia.
#[derive(Debug, Clone, PartialEq)]
pub struct TopValue {
    pub value: String,
    pub count: i64,
}

/// Recorre las filas ya preparadas y las junta en un `Vec`, propagando el
/// primer error de fila que aparezca.
fn collect(rows: impl Iterator<Item = rusqlite::Result<TopValue>>) -> Result<Vec<TopValue>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Valida la clave de un `attr:<clave>` con un allowlist de caracteres
/// (`[A-Za-z0-9_.-]`), para poder incrustarla en el *path* JSON sin arriesgar
/// una inyección (el propio valor de la clave nunca se concatena a SQL, pero
/// sí forma parte del *path* `$.<clave>` que se pasa como parámetro).
fn valid_attr_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Plantilla SQL común de las dimensiones no-`attr`. La subconsulta proyecta el
/// `value` desde `from_clause` y el filtro de ventana; la externa cuenta y
/// ordena. Extraída para que los tests verifiquen el MISMO SQL que producción
/// (p.ej. que `level`/`actor` enganchan sus índices covering).
///
/// Nota de escala (P3): con `idx_events_level(level, at_epoch)` e
/// `idx_events_actor(actor_id, at_epoch)` (bases covering añadidas aparte),
/// `top level` y `top actor` resuelven este SQL por índice covering —igual que
/// `top kind` con `idx_events_kind`— sin SCAN de la tabla `events`
/// (`EXPLAIN QUERY PLAN`: `SCAN events USING COVERING INDEX idx_events_level`;
/// para actor, `SCAN actors` + `SEARCH events USING COVERING INDEX
/// idx_events_actor`). El SQL es el de siempre; el plan lo fija el índice.
fn top_sql(from_clause: &str, value_expr: &str) -> String {
    top_sql_with(from_clause, value_expr, "")
}

fn top_sql_with(from_clause: &str, value_expr: &str, filters: &str) -> String {
    format!(
        "SELECT value, COUNT(*) AS count FROM (
             SELECT {value_expr} AS value
             FROM {from_clause}
             WHERE (?1 = 0 OR events.at_epoch >= ?1)
               {filters}
         )
         WHERE value IS NOT NULL AND value != ''
         GROUP BY value
         ORDER BY count DESC, value ASC
         LIMIT ?2"
    )
}

/// Valores más frecuentes de la dimensión `dim`, por nº de eventos.
///
/// Dimensiones soportadas:
/// - `"level"` | `"kind"`: columna nativa de `events` (cuenta 1 por evento).
/// - `"actor"`: `actors.key` del evento (cuenta 1 por evento).
/// - `"entity"`: `entities.key` vía `touches` (cuenta 1 por touch; en logs hay
///   un touch por evento, así que coincide con "por evento").
/// - `"attr:<clave>"`: `json_extract(events.attrs, '$.<clave>')` (cuenta 1 por
///   evento que tenga esa clave con valor no nulo).
///
/// `since_epoch`: 0 = sin límite. Los valores vacíos/NULL se descartan.
/// Orden determinista: por `count` DESC, luego `value` ASC. Devuelve error si
/// `dim` no es una dimensión soportada. Cuenta TODOS los eventos que encajan,
/// con o sin tiempo (`at_epoch <= 0` incluidos cuando `since_epoch == 0`), a
/// diferencia de `timeline`, que solo cuenta los de `at_epoch > 0`.
pub fn top(conn: &Connection, dim: &str, since_epoch: i64, limit: usize) -> Result<Vec<TopValue>> {
    top_filtered(conn, dim, since_epoch, &TopFilter::default(), limit)
}

/// Acota `top` con los mismos filtros que `events`/`patterns`/`timeline` (ver
/// [`crate::EventScope`]): fuente, texto, nivel(es), tipo, entidad(es) y fin de
/// ventana. En un índice que mezcla git, Nod y logs, sin esto la dimensión
/// `entity` la dominan las herramientas de Nod; con `level` responde «en qué
/// servicios caen los FATAL».
#[derive(Default)]
pub struct TopFilter<'a> {
    pub source: Option<&'a str>,
    pub grep: Option<&'a str>,
    pub level: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub entity: Option<&'a str>,
    pub attr: Option<&'a str>,
    /// `level` casa también lo deducido del texto, y `dim=level` cuenta por el nivel efectivo.
    pub infer_level: bool,
    pub until_epoch: i64,
}

pub fn top_filtered(conn: &Connection, dim: &str, since_epoch: i64, f: &TopFilter, limit: usize) -> Result<Vec<TopValue>> {
    let scope = crate::EventScope {
        source: f.source,
        grep: f.grep,
        level: f.level,
        kind: f.kind,
        entity: f.entity,
        attr: f.attr,
        infer_level: f.infer_level,
        until_epoch: f.until_epoch,
    };
    let map = |r: &rusqlite::Row| Ok(TopValue { value: r.get(0)?, count: r.get(1)? });
    let limit = limit as i64;
    if let Some(key) = dim.strip_prefix("attr:") {
        if !valid_attr_key(key) {
            return Err(format!("top: invalid attr key: {key:?}").into());
        }
        let path = format!("$.{key}");
        let (filters, extra) = scope.sql_on("events", 4);
        let sql = format!(
            "SELECT value, COUNT(*) AS count FROM (
                 SELECT json_extract(events.attrs, ?1) AS value
                 FROM events
                 WHERE (?2 = 0 OR events.at_epoch >= ?2)
                   {filters}
             )
             WHERE value IS NOT NULL AND value != ''
             GROUP BY value
             ORDER BY count DESC, value ASC
             LIMIT ?3"
        );
        let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&path, &since_epoch, &limit];
        for p in &extra {
            bound.push(p);
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bound), map)?;
        return collect(rows);
    }

    let level_value = crate::timeline::level_expr("events", f.infer_level);
    let (from_clause, value_expr) = match dim {
        "level" => ("events", level_value.as_str()),
        "kind" => ("events", "events.kind"),
        "actor" => (
            "events JOIN actors ON actors.id = events.actor_id",
            "COALESCE(actors.display_name, actors.key)",
        ),
        "entity" => (
            "touches JOIN events ON events.id = touches.event_id \
             JOIN entities ON entities.id = touches.entity_id",
            "entities.key",
        ),
        other => return Err(format!("top: unsupported dimension: {other}").into()),
    };

    // Sin filtros, la consulta de siempre: con condiciones presentes, aunque
    // valgan NULL, SQLite deja de usar los índices covering y recorre `events`.
    if scope.is_empty() && !f.infer_level {
        let mut stmt = conn.prepare(&top_sql(from_clause, value_expr))?;
        let rows = stmt.query_map(params![since_epoch, limit], map)?;
        return collect(rows);
    }
    let (filters, extra) = scope.sql_on("events", 3);
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&since_epoch, &limit];
    for p in &extra {
        bound.push(p);
    }
    let mut stmt = conn.prepare(&top_sql_with(from_clause, value_expr, &filters))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bound), map)?;
    collect(rows)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono_core::{Actor, Event, Touch};
    use chrono_store::Store;

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("chrono-metrics-test-top-{name}-{}-{}.db", std::process::id(), n));
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

    fn actor(key: &str) -> Actor {
        Actor { name: key.to_string(), key: key.to_string() }
    }

    fn touch(entity: &str) -> Touch {
        Touch { entity: entity.to_string(), entity_type: "file".to_string(), weight: 1, attrs: BTreeMap::new() }
    }

    fn mk_event(
        id: &str,
        kind: &str,
        level: &str,
        at_epoch: i64,
        who: Actor,
        env: Option<&str>,
        touches: Vec<Touch>,
    ) -> Event {
        let mut attrs = BTreeMap::new();
        if let Some(env) = env {
            attrs.insert("env".to_string(), env.to_string());
        }
        Event {
            id: id.to_string(),
            source_id: "src".to_string(),
            kind: kind.to_string(),
            at: format!("1970-01-01T00:00:{at_epoch:02}Z"),
            at_epoch,
            actor: who,
            title: format!("evento {id}"),
            level: level.to_string(),
            attrs,
            touches,
            ..Default::default()
        }
    }

    fn build_fixture() -> (TempDb, Store) {
        let db = TempDb::new("fixture");
        let mut store = Store::open(db.path()).unwrap();
        let alice = actor("alice@example.com");
        let bob = actor("bob@example.com");

        let evs = [
            mk_event("e1", "commit", "ERROR", 1, alice.clone(), Some("prod"), vec![touch("src/hot.rs")]),
            mk_event("e2", "commit", "ERROR", 2, alice.clone(), Some("prod"), vec![touch("src/warm.rs")]),
            mk_event("e3", "log", "INFO", 3, bob.clone(), Some("staging"), vec![touch("src/hot.rs")]),
            mk_event("e4", "log", "WARN", 4, bob.clone(), None, vec![touch("src/cold.rs")]),
            mk_event("e5", "log", "INFO", 5, alice.clone(), Some("prod"), vec![touch("src/hot.rs")]),
        ];
        let mut w = store.writer().unwrap();
        for e in &evs {
            w.add_event(e).unwrap();
        }
        w.commit().unwrap();
        (db, store)
    }

    #[test]
    fn top_level_cuenta_y_ordena_por_frecuencia_con_desempate_alfabetico() {
        let (_db, store) = build_fixture();
        // ERROR: e1,e2 = 2; INFO: e3,e5 = 2; WARN: e4 = 1.
        // Empate ERROR/INFO en count -> desempata por value ASC ("ERROR" < "INFO").
        let vals = top(store.conn(), "level", 0, 10).unwrap();
        assert_eq!(
            vals,
            vec![
                TopValue { value: "ERROR".to_string(), count: 2 },
                TopValue { value: "INFO".to_string(), count: 2 },
                TopValue { value: "WARN".to_string(), count: 1 },
            ]
        );
    }

    #[test]
    fn top_kind_cuenta_por_evento() {
        let (_db, store) = build_fixture();
        // commit: e1,e2 = 2; log: e3,e4,e5 = 3.
        let vals = top(store.conn(), "kind", 0, 10).unwrap();
        assert_eq!(
            vals,
            vec![
                TopValue { value: "log".to_string(), count: 3 },
                TopValue { value: "commit".to_string(), count: 2 },
            ]
        );
    }

    #[test]
    fn top_actor_cuenta_por_evento() {
        let (_db, store) = build_fixture();
        // alice: e1,e2,e5 = 3; bob: e3,e4 = 2.
        let vals = top(store.conn(), "actor", 0, 10).unwrap();
        assert_eq!(
            vals,
            vec![
                TopValue { value: "alice@example.com".to_string(), count: 3 },
                TopValue { value: "bob@example.com".to_string(), count: 2 },
            ]
        );
    }

    #[test]
    fn top_entity_cuenta_por_touch_y_desempata_alfabeticamente() {
        let (_db, store) = build_fixture();
        // hot.rs: e1,e3,e5 = 3; warm.rs: e2 = 1; cold.rs: e4 = 1.
        // Empate warm.rs/cold.rs -> "cold.rs" < "warm.rs".
        let vals = top(store.conn(), "entity", 0, 10).unwrap();
        assert_eq!(
            vals,
            vec![
                TopValue { value: "src/hot.rs".to_string(), count: 3 },
                TopValue { value: "src/cold.rs".to_string(), count: 1 },
                TopValue { value: "src/warm.rs".to_string(), count: 1 },
            ]
        );
    }

    #[test]
    fn top_attr_extrae_json_y_descarta_eventos_sin_la_clave() {
        let (_db, store) = build_fixture();
        // env=prod: e1,e2,e5 = 3; env=staging: e3 = 1; e4 no tiene "env" -> fuera.
        let vals = top(store.conn(), "attr:env", 0, 10).unwrap();
        assert_eq!(
            vals,
            vec![
                TopValue { value: "prod".to_string(), count: 3 },
                TopValue { value: "staging".to_string(), count: 1 },
            ]
        );
    }

    #[test]
    fn top_respeta_since_epoch_y_limit() {
        let (_db, store) = build_fixture();
        let vals = top(store.conn(), "kind", 4, 10).unwrap();
        // Desde epoch 4: solo e4 (log,4) y e5 (log,5) -> log=2, commit ausente.
        assert_eq!(vals, vec![TopValue { value: "log".to_string(), count: 2 }]);

        let limited = top(store.conn(), "kind", 0, 1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].value, "log");
    }

    #[test]
    fn top_rechaza_dimension_no_soportada_y_clave_de_attr_invalida() {
        let (_db, store) = build_fixture();
        let conn = store.conn();
        assert!(top(conn, "no-existe", 0, 10).is_err());
        assert!(top(conn, "attr:", 0, 10).is_err());
        assert!(top(conn, "attr:mal clave", 0, 10).is_err());
        assert!(top(conn, "attr:mal;clave", 0, 10).is_err());
    }
    /// `top_filtered` acota por fuente (tipo o prefijo de id), por texto del título y por nivel; sin filtros
    /// da lo mismo que `top`.
    #[test]
    fn top_filtered_acota_por_fuente_titulo_y_nivel() {
        let db = TempDb::new("filtros");
        let mut store = Store::open(db.path()).unwrap();
        let mut a = mk_event("t1", "log", "FAIL", 1, actor("ci"), None, vec![touch("EP-002")]);
        a.source_id = "template:/logs/tests.log".into();
        a.title = "listen EADDRINUSE :::3000".into();
        let mut b = mk_event("t2", "log", "FAIL", 2, actor("ci"), None, vec![touch("EP-003")]);
        b.source_id = "template:/logs/tests.log".into();
        b.title = "listen EADDRINUSE :::3000".into();
        let mut c = mk_event("t3", "log", "INFO", 3, actor("ci"), None, vec![touch("EP-003")]);
        c.source_id = "template:/logs/tests.log".into();
        c.title = "suite: 40 tests, 0 fallan".into();
        let mut d = mk_event("n1", "nod.tool.call", "INFO", 4, actor("agente"), None, vec![touch("tool/Bash")]);
        d.source_id = "nod:/eventos.jsonl".into();
        let mut w = store.writer().unwrap();
        for e in [&a, &b, &c, &d] {
            w.add_event(e).unwrap();
        }
        w.commit().unwrap();
        let tv = |v: &str, n: i64| TopValue { value: v.to_string(), count: n };
        let f = |source, grep, level| TopFilter { source, grep, level, ..Default::default() };
        let conn = store.conn();
        assert_eq!(top_filtered(conn, "entity", 0, &f(Some("template"), None, None), 10).unwrap(),
                   vec![tv("EP-003", 2), tv("EP-002", 1)]);
        assert_eq!(top_filtered(conn, "entity", 0, &f(Some("template"), Some("EADDRINUSE"), None), 10).unwrap(),
                   vec![tv("EP-002", 1), tv("EP-003", 1)]);
        assert_eq!(top_filtered(conn, "entity", 0, &f(Some("nod:/eventos"), None, None), 10).unwrap(), vec![tv("tool/Bash", 1)]);
        assert_eq!(top_filtered(conn, "level", 0, &f(None, None, Some("FAIL")), 10).unwrap(), vec![tv("FAIL", 2)]);
        assert_eq!(top_filtered(conn, "entity", 0, &TopFilter::default(), 10).unwrap(), top(conn, "entity", 0, 10).unwrap());
    }
}

/// P3: `top level`/`top actor` deben resolverse por índice covering (sin SCAN
/// de la tabla `events`), igual que `top kind`. Estos tests bloquean una
/// regresión del plan y documentan el borde de eventos sin tiempo.
#[cfg(test)]
mod tests_p3 {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono_core::{Actor, Event};
    use chrono_store::Store;

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        path: PathBuf,
    }
    impl TempDb {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("chrono-metrics-test-topp3-{name}-{}-{}.db", std::process::id(), n));
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

    fn mk(id: &str, level: &str, at_epoch: i64, who: &str) -> Event {
        Event {
            id: id.to_string(),
            source_id: "jsonl:/log".to_string(),
            kind: "log".to_string(),
            at: if at_epoch > 0 { format!("ts{at_epoch}") } else { String::new() },
            at_epoch,
            actor: Actor { name: who.to_string(), key: who.to_string() },
            title: format!("evento {id}"),
            level: level.to_string(),
            attrs: BTreeMap::new(),
            ..Default::default()
        }
    }

    fn fixture(name: &str) -> (TempDb, Store) {
        let db = TempDb::new(name);
        let mut store = Store::open(db.path()).unwrap();
        let mut w = store.writer().unwrap();
        w.add_event(&mk("e1", "ERROR", 1_000, "host-a")).unwrap();
        w.add_event(&mk("e2", "INFO", 2_000, "host-a")).unwrap();
        w.add_event(&mk("e3", "INFO", 3_000, "host-b")).unwrap();
        w.add_event(&mk("e4", "WARN", 4_000, "host-b")).unwrap();
        // Evento sin tiempo (borde): cuenta en top con since=0, no con since>0.
        w.add_event(&mk("z1", "TRACE", 0, "host-z")).unwrap();
        w.commit().unwrap();
        (db, store)
    }

    /// Texto del `EXPLAIN QUERY PLAN` del SQL (una línea por paso del plan). El
    /// SQL lleva `?1` (since) y `?2` (limit); se enlazan valores de muestra para
    /// que el planificador elija como en la consulta real (parametrizada, no con
    /// literales que pudiera plegar).
    fn plan(store: &Store, sql: &str) -> String {
        let eqp = format!("EXPLAIN QUERY PLAN {sql}");
        let mut stmt = store.conn().prepare(&eqp).unwrap();
        let rows = stmt.query_map(params![0_i64, 25_i64], |r| r.get::<_, String>(3)).unwrap();
        rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn top_level_usa_indice_covering() {
        let (_db, store) = fixture("level-eqp");
        let p = plan(&store, &top_sql("events", "events.level"));
        assert!(p.contains("idx_events_level"), "plan debería usar idx_events_level:\n{p}");
    }

    #[test]
    fn top_actor_usa_indice_covering() {
        let (_db, store) = fixture("actor-eqp");
        let sql = top_sql(
            "events JOIN actors ON actors.id = events.actor_id",
            "COALESCE(actors.display_name, actors.key)",
        );
        let p = plan(&store, &sql);
        assert!(p.contains("idx_events_actor"), "plan debería usar idx_events_actor:\n{p}");
    }

    #[test]
    fn top_kind_sigue_usando_indice_covering() {
        // Referencia: `kind` (no tocado) resuelve por su índice covering.
        let (_db, store) = fixture("kind-eqp");
        let p = plan(&store, &top_sql("events", "events.kind"));
        assert!(p.contains("idx_events_kind"), "plan debería usar idx_events_kind:\n{p}");
    }

    #[test]
    fn top_cuenta_eventos_sin_tiempo_solo_con_since_cero() {
        let (_db, store) = fixture("timeless");
        let conn = store.conn();
        // since=0: incluye el evento sin tiempo (TRACE, at_epoch=0).
        let all = top(conn, "level", 0, 10).unwrap();
        assert!(all.iter().any(|v| v.value == "TRACE" && v.count == 1), "con since=0 cuenta TRACE: {all:?}");
        // since>0: el filtro at_epoch>=since deja fuera el at_epoch=0.
        let windowed = top(conn, "level", 1, 10).unwrap();
        assert!(!windowed.iter().any(|v| v.value == "TRACE"), "con since>0 excluye TRACE: {windowed:?}");
    }

}
