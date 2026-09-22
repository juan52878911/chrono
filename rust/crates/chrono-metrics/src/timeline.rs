//! `timeline`: conteo de eventos por bucket temporal, opcionalmente desglosado
//! por una dimensión de baja cardinalidad (`level`|`kind`). Consulta log-native
//! (R3 oleada 2). Lee `events` crudos (sin rollups; los rollups son una oleada
//! posterior). Ver `docs/DESIGN-GENERAL-CORE.md §3`.

use rusqlite::{params, Connection};

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
        return Err("timeline: bucket_secs debe ser > 0".into());
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
            return Err(format!("timeline: group_by no soportado: {other}").into());
        }
    };

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
