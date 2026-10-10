//! Churn: líneas +/- por entity en la ventana. Paridad con `Churn` en
//! `internal/metrics/metrics.go`.

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

/// Fila de churn (añadido/borrado) de una entity.
#[derive(Debug, Clone, PartialEq)]
pub struct ChurnRow {
    pub entity: String,
    pub added: i64,
    pub deleted: i64,
}

/// Suma de `added`/`deleted` (columnas enteras de `touches`, v5) por entity en
/// la ventana, top `limit`. Ignora binarios: si `added`/`deleted` son negativos
/// (git binario = -1), se tratan como 0 (`MAX(x,0)`, igual que el Go).
pub fn churn(conn: &Connection, since_epoch: i64, limit: usize, symbols_only: bool) -> Result<Vec<ChurnRow>> {
    let op = crate::symbol_type_op(symbols_only);
    // Alias `a`/`d` (como el Go), NO `added`/`deleted`: en el ORDER BY,
    // `deleted` resolvería a la columna `entities.deleted` (siempre 0 aquí)
    // en vez de al alias, y el orden quedaría solo por `added`.
    //
    // Sin ventana (`since_epoch == 0`) el JOIN a `events` solo aportaría
    // `ev.at_epoch >= 0`. Si NINGÚN evento tiene `at_epoch < 0` ese filtro no
    // descarta nada, así que eliminamos el JOIN (a escala son ~1M lookups
    // inútiles por la PK TEXT de `events`). Si hubiera epochs negativos (imports
    // históricos pre-1970) caemos al camino con JOIN (idéntico al actual). La
    // comprobación es O(log N) vía `idx_events_at`.
    let fast_path = since_epoch == 0 && !has_negative_epoch(conn)?;
    let sql = if fast_path {
        format!(
            "SELECT e.key,
                 SUM(MAX(t.added, 0)) AS a,
                 SUM(MAX(t.deleted, 0)) AS d
             FROM touches t
             JOIN entities e ON e.id = t.entity_id
             WHERE e.excluded = 0 AND e.type {op} 'symbol'
             GROUP BY e.id
             ORDER BY (a + d) DESC
             LIMIT ?1",
        )
    } else {
        format!(
            "SELECT e.key,
                 SUM(MAX(t.added, 0)) AS a,
                 SUM(MAX(t.deleted, 0)) AS d
             FROM touches t
             JOIN events ev ON ev.id = t.event_id
             JOIN entities e ON e.id = t.entity_id
             WHERE e.excluded = 0 AND e.type {op} 'symbol' AND ev.at_epoch >= ?1
             GROUP BY e.id
             ORDER BY (a + d) DESC
             LIMIT ?2",
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| {
        Ok(ChurnRow { entity: r.get(0)?, added: r.get(1)?, deleted: r.get(2)? })
    };
    let rows = if fast_path {
        stmt.query_map(params![limit as i64], map)?
    } else {
        stmt.query_map(params![since_epoch, limit as i64], map)?
    };
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
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
            .join(format!("chrono-p3-churn-{}-{}.db", std::process::id(), n));
        let _ = std::fs::remove_file(&p);
        Tmp(p)
    }

    /// `added`/`deleted` en `touches.attrs` (incluye un binario negativo que
    /// `MAX(x,0)` debe anular) y una entidad excluida (fuera en ambos caminos).
    fn seed(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO entities(id,key,type,size,deleted,excluded) VALUES
                (1,'src/hot.rs','file',0,0,0),
                (2,'src/warm.rs','file',0,0,0),
                (3,'src/sym','symbol',0,0,0),
                (4,'src/exc.rs','file',0,0,1);
             INSERT INTO sources(ref,id,kind) VALUES (1,'git:/r','git');
             INSERT INTO events(id,source_ref,kind,at,at_epoch,title) VALUES
                ('e1',1,'commit','',1000,''),
                ('e2',1,'commit','',2000,'');
             INSERT INTO touches(event_id,entity_id,weight,added,deleted,attrs) VALUES
                ('e1',1,12,10,2,'{\"added\":10,\"deleted\":2}'),
                ('e2',1,5,5,0,'{\"added\":5,\"deleted\":0}'),
                ('e1',2,2,1,1,'{\"added\":1,\"deleted\":1}'),
                ('e2',2,0,-1,-1,'{\"added\":-1,\"deleted\":-1}'),
                ('e1',3,3,9,9,'{\"added\":9,\"deleted\":9}'),
                ('e1',4,1,7,7,'{\"added\":7,\"deleted\":7}');",
        )
        .unwrap();
    }

    fn legacy(conn: &Connection, since: i64, limit: i64, symbols_only: bool) -> Vec<(String, i64, i64)> {
        let op = crate::symbol_type_op(symbols_only);
        let mut st = conn
            .prepare(&format!(
                "SELECT e.key,
                     SUM(MAX(CAST(COALESCE(json_extract(t.attrs, '$.added'), 0) AS INTEGER), 0)) AS a,
                     SUM(MAX(CAST(COALESCE(json_extract(t.attrs, '$.deleted'), 0) AS INTEGER), 0)) AS d
                 FROM touches t
                 JOIN events ev ON ev.id = t.event_id
                 JOIN entities e ON e.id = t.entity_id
                 WHERE e.excluded = 0 AND e.type {op} 'symbol' AND ev.at_epoch >= ?1
                 GROUP BY e.id
                 ORDER BY (a + d) DESC
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
            let new = churn(conn, 0, 10, symbols_only).unwrap();
            let old = legacy(conn, 0, 10, symbols_only);
            let new_t: Vec<_> =
                new.iter().map(|r| (r.entity.clone(), r.added, r.deleted)).collect();
            assert_eq!(new_t, old, "paridad since==0 symbols_only={symbols_only}");
        }
    }

    #[test]
    fn ventana_conserva_el_camino_con_join() {
        let _t = tmp();
        let store = Store::open(_t.0.as_path()).unwrap();
        let conn = store.conn();
        seed(conn);
        let new = churn(conn, 2000, 10, false).unwrap();
        let old = legacy(conn, 2000, 10, false);
        let new_t: Vec<_> = new.iter().map(|r| (r.entity.clone(), r.added, r.deleted)).collect();
        assert_eq!(new_t, old);
    }

    #[test]
    fn epoch_negativo_cae_al_join_y_sigue_identico() {
        // Con un evento `at_epoch < 0` el camino rápido NO aplica: debe caer al
        // JOIN y seguir byte-idéntico a la query legacy (que lo excluye por
        // `at_epoch >= 0`). El churn pre-1970 NO debe contar.
        let _t = tmp();
        let store = Store::open(_t.0.as_path()).unwrap();
        let conn = store.conn();
        seed(conn);
        conn.execute_batch(
            "INSERT INTO entities(id,key,type,size,deleted,excluded) VALUES
                (5,'src/ancient.rs','file',0,0,0);
             INSERT INTO events(id,source_ref,kind,at,at_epoch,title) VALUES
                ('eneg',1,'commit','',-5000,'');
             INSERT INTO touches(event_id,entity_id,weight,added,deleted,attrs) VALUES
                ('eneg',5,1,100,100,'{\"added\":100,\"deleted\":100}'),
                ('eneg',1,1,100,100,'{\"added\":100,\"deleted\":100}');",
        )
        .unwrap();
        assert!(super::has_negative_epoch(conn).unwrap(), "debe detectar el epoch negativo");
        for symbols_only in [false, true] {
            let new = churn(conn, 0, 10, symbols_only).unwrap();
            let old = legacy(conn, 0, 10, symbols_only);
            let new_t: Vec<_> =
                new.iter().map(|r| (r.entity.clone(), r.added, r.deleted)).collect();
            assert_eq!(new_t, old, "paridad con epoch negativo symbols_only={symbols_only}");
        }
        let new = churn(conn, 0, 10, false).unwrap();
        assert!(
            !new.iter().any(|r| r.entity == "src/ancient.rs"),
            "el churn pre-1970 debe quedar excluido como hoy"
        );
        // hot.rs conserva su churn sin el evento negativo: added=15, deleted=2.
        let hot = new.iter().find(|r| r.entity == "src/hot.rs").unwrap();
        assert_eq!((hot.added, hot.deleted), (15, 2));
    }

    #[test]
    fn el_plan_sin_ventana_no_toca_events() {
        let _t = tmp();
        let store = Store::open(_t.0.as_path()).unwrap();
        let conn = store.conn();
        seed(conn);
        let sql = "EXPLAIN QUERY PLAN
             SELECT e.key,
                 SUM(MAX(t.added, 0)) AS a,
                 SUM(MAX(t.deleted, 0)) AS d
             FROM touches t
             JOIN entities e ON e.id = t.entity_id
             WHERE e.excluded = 0 AND e.type != 'symbol'
             GROUP BY e.id
             ORDER BY (a + d) DESC
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
