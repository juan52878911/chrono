//! Hotspots: ficheros que más cambian ponderados por su tamaño actual.
//! Paridad con `Hotspots` en `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// ¿Existe algún evento con `at_epoch < 0`? (imports históricos pre-1970 o
/// epochs numéricos negativos). O(log N) vía `idx_events_at`: habilita el camino
/// rápido sin JOIN solo cuando `at_epoch >= 0` no descartaría nada.
fn has_negative_epoch(conn: &Connection) -> Result<bool> {
    let any: i64 =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE at_epoch < 0)", [], |r| r.get(0))?;
    Ok(any != 0)
}

/// Un fichero caliente: cambia mucho y/o pesa mucho.
#[derive(Debug, Clone, PartialEq)]
pub struct Hotspot {
    pub entity: String,
    pub changes: i64,
    pub size: i64,
    /// `raw = changes * size` normalizado por el máximo DENTRO del top
    /// devuelto (igual que el Go: el `LIMIT` se aplica en SQL antes de
    /// normalizar, así que el primer resultado siempre vale 1.0).
    pub score: f64,
}

/// Entidades más cambiadas ponderadas por `entities.size`. Filtra `deleted=0`,
/// `excluded=0`. Por defecto EXCLUYE símbolos (`type != 'symbol'`); con
/// `symbols_only=true` (consultas `--by symbol`) devuelve SOLO símbolos.
///
/// No se filtra por `type='file'`: en git las entidades de fichero son idénticas
/// al Go, pero así el adaptador de logs (entity_type='log-source') u otras
/// trazas también rankean. `MAX(size,1)` = fallback a frecuencia cuando no hay
/// tamaño (logs y símbolos sin S2), sin alterar el top de git (que sí lo tiene).
pub fn hotspots(conn: &Connection, since_epoch: i64, limit: usize, symbols_only: bool) -> Result<Vec<Hotspot>> {
    let op = crate::symbol_type_op(symbols_only);
    // Sin ventana (`since_epoch == 0`) el único filtro que aportaría el JOIN a
    // `events` sería `ev.at_epoch >= 0`. Si NINGÚN evento tiene `at_epoch < 0`
    // ese filtro no descarta nada, así que eliminamos el JOIN (a escala son ~1M
    // lookups aleatorios por la PK TEXT de `events`, inútiles) y consultamos solo
    // `touches`/`entities`. Si hubiera epochs negativos (imports históricos
    // pre-1970), el filtro SÍ descartaría, así que caemos al camino con JOIN
    // (idéntico al actual). La comprobación es O(log N) vía `idx_events_at`.
    let fast_path = since_epoch == 0 && !has_negative_epoch(conn)?;
    let sql = if fast_path {
        format!(
            "SELECT e.key, COUNT(*) AS changes, e.size
             FROM touches t
             JOIN entities e ON e.id = t.entity_id
             WHERE e.deleted = 0 AND e.excluded = 0 AND e.type {op} 'symbol'
             GROUP BY e.id
             ORDER BY (CAST(COUNT(*) AS REAL) * MAX(e.size, 1)) DESC
             LIMIT ?1",
        )
    } else {
        format!(
            "SELECT e.key, COUNT(*) AS changes, e.size
             FROM touches t
             JOIN events ev ON ev.id = t.event_id
             JOIN entities e ON e.id = t.entity_id
             WHERE e.deleted = 0 AND e.excluded = 0 AND e.type {op} 'symbol' AND ev.at_epoch >= ?1
             GROUP BY e.id
             ORDER BY (CAST(COUNT(*) AS REAL) * MAX(e.size, 1)) DESC
             LIMIT ?2",
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    };
    let rows = if fast_path {
        stmt.query_map(params![limit as i64], map)?
    } else {
        stmt.query_map(params![since_epoch, limit as i64], map)?
    };

    let mut out = Vec::new();
    let mut max_raw = 0f64;
    for row in rows {
        let (entity, changes, size) = row?;
        let raw = changes as f64 * size.max(1) as f64;
        if raw > max_raw {
            max_raw = raw;
        }
        out.push(Hotspot { entity, changes, size, score: raw });
    }
    if max_raw > 0.0 {
        for h in &mut out {
            h.score /= max_raw;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests_p3 {
    //! P3: el camino rápido sin JOIN a `events` (cuando `since_epoch == 0`)
    //! debe ser BYTE-IDÉNTICO al camino antiguo con JOIN, y el plan no debe
    //! tocar `events`.
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono_store::Store;
    use rusqlite::{params, Connection};

    use super::*;

    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            for s in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{s}", self.0.display()));
            }
        }
    }
    fn tmp() -> Tmp {
        static C: AtomicU64 = AtomicU64::new(0);
        let n = C.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir()
            .join(format!("chrono-p3-hot-{}-{}.db", std::process::id(), n));
        let _ = std::fs::remove_file(&p);
        Tmp(p)
    }

    /// Siembra entidades/eventos/touches por SQL directo. `src/hot.rs` (muy
    /// tocado), `src/warm.rs`, `src/cold.rs`; `src/sym` es un símbolo (se excluye
    /// por defecto); `src/del.rs` está borrada y `src/exc.rs` excluida (ambas
    /// fuera en los dos caminos).
    fn seed(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO entities(id,key,type,size,deleted,excluded) VALUES
                (1,'src/hot.rs','file',100,0,0),
                (2,'src/warm.rs','file',50,0,0),
                (3,'src/cold.rs','file',10,0,0),
                (4,'src/sym','symbol',0,0,0),
                (5,'src/del.rs','file',99,1,0),
                (6,'src/exc.rs','file',99,0,1);
             INSERT INTO events(id,source_id,kind,at,at_epoch,title) VALUES
                ('e1','git:/r','commit','',1000,''),
                ('e2','git:/r','commit','',2000,''),
                ('e3','git:/r','commit','',3000,'');
             INSERT INTO touches(event_id,entity_id,weight,attrs) VALUES
                ('e1',1,12,'{}'),('e2',1,5,'{}'),('e3',1,4,'{}'),
                ('e1',2,2,'{}'),('e3',2,1,'{}'),
                ('e3',3,1,'{}'),
                ('e1',4,3,'{}'),('e2',4,1,'{}'),
                ('e1',5,1,'{}'),
                ('e1',6,1,'{}');",
        )
        .unwrap();
    }

    /// Query ANTIGUA (con JOIN a events) para el contraste de paridad.
    fn legacy(conn: &Connection, since: i64, limit: i64, symbols_only: bool) -> Vec<(String, i64, i64)> {
        let op = crate::symbol_type_op(symbols_only);
        let mut st = conn
            .prepare(&format!(
                "SELECT e.key, COUNT(*) AS changes, e.size
                 FROM touches t
                 JOIN events ev ON ev.id = t.event_id
                 JOIN entities e ON e.id = t.entity_id
                 WHERE e.deleted = 0 AND e.excluded = 0 AND e.type {op} 'symbol' AND ev.at_epoch >= ?1
                 GROUP BY e.id
                 ORDER BY (CAST(COUNT(*) AS REAL) * MAX(e.size, 1)) DESC
                 LIMIT ?2",
            ))
            .unwrap();
        st.query_map(params![since, limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        })
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
    }

    #[test]
    fn fast_path_identico_al_join_sin_ventana() {
        let _t = tmp();
        let store = Store::open(_t.0.as_path()).unwrap();
        let conn = store.conn();
        seed(conn);

        for symbols_only in [false, true] {
            let new = hotspots(conn, 0, 10, symbols_only).unwrap();
            let old = legacy(conn, 0, 10, symbols_only);
            // Mismo nº de filas, mismo orden, mismos (entity, changes, size):
            // el `score` se deriva determinísticamente de estos, así que basta.
            let new_t: Vec<_> =
                new.iter().map(|h| (h.entity.clone(), h.changes, h.size)).collect();
            assert_eq!(new_t, old, "paridad since==0 symbols_only={symbols_only}");
        }
    }

    #[test]
    fn ventana_conserva_el_camino_con_join() {
        let _t = tmp();
        let store = Store::open(_t.0.as_path()).unwrap();
        let conn = store.conn();
        seed(conn);
        let new = hotspots(conn, 2000, 10, false).unwrap();
        let old = legacy(conn, 2000, 10, false);
        let new_t: Vec<_> = new.iter().map(|h| (h.entity.clone(), h.changes, h.size)).collect();
        assert_eq!(new_t, old);
        // La ventana (>=2000) descarta el touch de e1@1000 sobre warm.rs: solo
        // queda el de e3@3000 (1 cambio), frente a los 2 que tiene sin ventana.
        let warm = new.iter().find(|h| h.entity == "src/warm.rs").unwrap();
        assert_eq!(warm.changes, 1, "la ventana debe dejar solo el touch de e3");
    }

    #[test]
    fn epoch_negativo_cae_al_join_y_sigue_identico() {
        // Con un evento `at_epoch < 0` el camino rápido NO aplica: debe caer al
        // JOIN y seguir byte-idéntico a la query legacy (que lo excluye por
        // `at_epoch >= 0`). El touch pre-1970 NO debe aparecer.
        let _t = tmp();
        let store = Store::open(_t.0.as_path()).unwrap();
        let conn = store.conn();
        seed(conn);
        conn.execute_batch(
            "INSERT INTO entities(id,key,type,size,deleted,excluded) VALUES
                (7,'src/ancient.rs','file',77,0,0);
             INSERT INTO events(id,source_id,kind,at,at_epoch,title) VALUES
                ('eneg','git:/r','commit','',-5000,'');
             INSERT INTO touches(event_id,entity_id,weight,attrs) VALUES
                ('eneg',7,1,'{}'),('eneg',1,1,'{}');",
        )
        .unwrap();
        assert!(super::has_negative_epoch(conn).unwrap(), "debe detectar el epoch negativo");
        for symbols_only in [false, true] {
            let new = hotspots(conn, 0, 10, symbols_only).unwrap();
            let old = legacy(conn, 0, 10, symbols_only);
            let new_t: Vec<_> =
                new.iter().map(|h| (h.entity.clone(), h.changes, h.size)).collect();
            assert_eq!(new_t, old, "paridad con epoch negativo symbols_only={symbols_only}");
        }
        let new = hotspots(conn, 0, 10, false).unwrap();
        assert!(
            !new.iter().any(|h| h.entity == "src/ancient.rs"),
            "el touch pre-1970 debe quedar excluido como hoy"
        );
        // hot.rs sigue con 3 cambios: el touch de 'eneg' sobre hot.rs tampoco cuenta.
        let hot = new.iter().find(|h| h.entity == "src/hot.rs").unwrap();
        assert_eq!(hot.changes, 3);
    }

    #[test]
    fn el_plan_sin_ventana_no_toca_events() {
        let _t = tmp();
        let store = Store::open(_t.0.as_path()).unwrap();
        let conn = store.conn();
        seed(conn);
        // Mismo SQL que el camino rápido de `hotspots` con symbols_only=false.
        let sql = "EXPLAIN QUERY PLAN
             SELECT e.key, COUNT(*) AS changes, e.size
             FROM touches t
             JOIN entities e ON e.id = t.entity_id
             WHERE e.deleted = 0 AND e.excluded = 0 AND e.type != 'symbol'
             GROUP BY e.id
             ORDER BY (CAST(COUNT(*) AS REAL) * MAX(e.size, 1)) DESC
             LIMIT ?1";
        let mut st = conn.prepare(sql).unwrap();
        let plan: Vec<String> = st
            .query_map(params![10_i64], |r| r.get::<_, String>(3))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        let joined = plan.join("\n");
        assert!(
            !joined.to_lowercase().contains("events"),
            "el plan no debe tocar events:\n{joined}"
        );
    }
}
