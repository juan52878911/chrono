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
/// `dim` no es una dimensión soportada.
pub fn top(conn: &Connection, dim: &str, since_epoch: i64, limit: usize) -> Result<Vec<TopValue>> {
    if let Some(key) = dim.strip_prefix("attr:") {
        if !valid_attr_key(key) {
            return Err(format!("top: clave de attr inválida: {key:?}").into());
        }
        let path = format!("$.{key}");
        let mut stmt = conn.prepare(
            "SELECT value, COUNT(*) AS count FROM (
                 SELECT json_extract(events.attrs, ?1) AS value
                 FROM events
                 WHERE (?2 = 0 OR events.at_epoch >= ?2)
             )
             WHERE value IS NOT NULL AND value != ''
             GROUP BY value
             ORDER BY count DESC, value ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![path, since_epoch, limit as i64], |r| {
            Ok(TopValue { value: r.get(0)?, count: r.get(1)? })
        })?;
        return collect(rows);
    }

    let (from_clause, value_expr) = match dim {
        "level" => ("events", "events.level"),
        "kind" => ("events", "events.kind"),
        "actor" => ("events JOIN actors ON actors.id = events.actor_id", "actors.key"),
        "entity" => (
            "touches JOIN events ON events.id = touches.event_id \
             JOIN entities ON entities.id = touches.entity_id",
            "entities.key",
        ),
        other => return Err(format!("top: dimensión no soportada: {other}").into()),
    };

    let sql = format!(
        "SELECT value, COUNT(*) AS count FROM (
             SELECT {value_expr} AS value
             FROM {from_clause}
             WHERE (?1 = 0 OR events.at_epoch >= ?1)
         )
         WHERE value IS NOT NULL AND value != ''
         GROUP BY value
         ORDER BY count DESC, value ASC
         LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![since_epoch, limit as i64], |r| {
        Ok(TopValue { value: r.get(0)?, count: r.get(1)? })
    })?;
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
}
