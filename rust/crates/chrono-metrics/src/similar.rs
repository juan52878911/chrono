//! Similar: eventos casi-duplicados por distancia de Hamming del simhash.
//! Paridad con `Similar` en `internal/metrics/metrics.go`.

use rusqlite::{params, Connection};

use crate::Result;

/// Un evento casi-duplicado.
#[derive(Debug, Clone, PartialEq)]
pub struct SimilarRow {
    pub id: String,
    pub title: String,
    pub distance: u32,
}

/// Umbral del camino de bandas (pigeonhole). El simhash de 64 bits se parte en
/// 4 bandas de 16 bits (`idx_events_sh0..3`). Dos valores a distancia Hamming
/// `<= 3` difieren en `<= 3` bits, que tocan a lo sumo 3 de las 4 bandas, así
/// que COMPARTEN al menos una banda idéntica: recuperar candidatos por igualdad
/// de banda da recall EXACTO (no se pierde ningún resultado). A partir de
/// `max_dist` 4 NO está garantizado (4 bits pueden tocar las 4 bandas), así que
/// ahí se cae al escaneo completo para no perder resultados. En ambos casos la
/// salida es byte-idéntica a la implementación histórica (mismo filtro y orden).
const BAND_MAX_DIST: u32 = 3;

/// Casi-duplicados del evento `id` por distancia de Hamming del `simhash`
/// (`(a ^ b).count_ones()`). Orden asc por distancia, top `limit`.
///
/// Recuperación en dos regímenes, ambos con salida idéntica a la histórica:
/// - `max_dist <= 3`: candidatos por bandas SimHash (usa los índices de
///   expresión vía la OR-optimization de SQLite), refinados con Hamming exacto
///   en Rust. O(candidatos) en vez de O(N).
/// - `max_dist` 4 o más: escaneo completo (lo que hacía siempre), porque las
///   bandas no garantizan recall a esa distancia.
pub fn similar(conn: &Connection, id: &str, max_dist: u32, limit: usize) -> Result<Vec<SimilarRow>> {
    // Resuelve por prefijo (igual que `sha LIKE ?||'%'` en el Go): la CLI y
    // los ejemplos de `bugs`/`branches` usan ids cortos de 8 caracteres.
    let (id, target): (String, i64) = conn.query_row(
        "SELECT id, simhash FROM events WHERE id LIKE ?1||'%' ORDER BY id LIMIT 1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let id = id.as_str();
    let target = target as u64;

    // Mismo conjunto filtrado por ambos caminos; el camino de bandas devuelve
    // los candidatos en orden de `rowid` (el orden natural del escaneo), para
    // que tras la ordenación estable por distancia el empate quede igual que en
    // el escaneo completo. El escaneo completo es literalmente el código
    // histórico.
    let mut out = if max_dist <= BAND_MAX_DIST {
        candidatos_por_bandas(conn, id, target, max_dist)?
    } else {
        escaneo_completo(conn, id, target, max_dist)?
    };
    // `sort_by` es estable: dentro de la misma distancia se conserva el orden
    // de entrada (orden natural de la tabla = rowid asc). No se desempata por
    // `id` porque la implementación histórica tampoco lo hace.
    out.sort_by(|a, b| a.distance.cmp(&b.distance));
    out.truncate(limit);
    Ok(out)
}

/// Escaneo completo en orden natural de la tabla: idéntico a la implementación
/// histórica. Devuelve los candidatos a distancia `<= max_dist` sin ordenar por
/// distancia (eso lo hace el llamador, igual que antes).
fn escaneo_completo(conn: &Connection, id: &str, target: u64, max_dist: u32) -> Result<Vec<SimilarRow>> {
    let mut stmt = conn.prepare("SELECT id, title, simhash FROM events WHERE id != ?1")?;
    let rows = stmt.query_map(params![id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (other_id, title, sh) = row?;
        let dist = (target ^ (sh as u64)).count_ones();
        if dist <= max_dist {
            out.push(SimilarRow { id: other_id, title, distance: dist });
        }
    }
    Ok(out)
}

/// Recuperación por bandas SimHash (solo válida para `max_dist <= BAND_MAX_DIST`).
/// Calcula las 4 bandas de 16 bits del ancla y pide a SQLite los eventos que
/// coinciden en AL MENOS una banda; SQLite resuelve el `OR` con los 4 índices
/// de expresión (`idx_events_sh0..3`) sin escanear la tabla. Luego refina con
/// Hamming exacto en Rust. Por pigeonhole esto recupera TODOS los eventos a
/// distancia `<= 3`, así que el conjunto filtrado es idéntico al del escaneo.
///
/// Los candidatos se devuelven en orden de `rowid` (el mismo orden natural en
/// que el escaneo completo recorre la tabla), de modo que la ordenación estable
/// por distancia del llamador produzca EXACTAMENTE el mismo orden que el camino
/// histórico (incluidos los empates).
fn candidatos_por_bandas(conn: &Connection, id: &str, target: u64, max_dist: u32) -> Result<Vec<SimilarRow>> {
    // Las bandas se calculan igual que las expresiones indexadas: `& 65535`
    // toma los 16 bits bajos del desplazamiento. Caben en i64 (0..=65535).
    let b0 = ((target >> 48) & 0xFFFF) as i64;
    let b1 = ((target >> 32) & 0xFFFF) as i64;
    let b2 = ((target >> 16) & 0xFFFF) as i64;
    let b3 = (target & 0xFFFF) as i64;

    // Las expresiones del WHERE son textualmente las de los índices
    // `idx_events_sh0..3`, condición para que SQLite use la OR-optimization.
    let mut stmt = conn.prepare(
        "SELECT rowid, id, title, simhash FROM events \
         WHERE id != ?1 AND ( \
             ((simhash >> 48) & 65535) = ?2 OR \
             ((simhash >> 32) & 65535) = ?3 OR \
             ((simhash >> 16) & 65535) = ?4 OR \
             (simhash & 65535) = ?5)",
    )?;
    let rows = stmt.query_map(params![id, b0, b1, b2, b3], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;

    let mut cands: Vec<(i64, SimilarRow)> = Vec::new();
    for row in rows {
        let (rowid, other_id, title, sh) = row?;
        let dist = (target ^ (sh as u64)).count_ones();
        if dist <= max_dist {
            cands.push((rowid, SimilarRow { id: other_id, title, distance: dist }));
        }
    }
    // Orden natural de la tabla (rowid asc): replica el orden en que el escaneo
    // completo entrega las filas, para empates byte-idénticos.
    cands.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(cands.into_iter().map(|(_, row)| row).collect())
}

#[cfg(test)]
mod tests_p3 {
    use super::*;
    use chrono_core::{Actor, Event};
    use chrono_store::Store;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// DB temporal que se borra (con WAL/SHM) al soltar.
    struct TempDb {
        path: PathBuf,
    }
    impl TempDb {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("chrono-metrics-p3-{}-{}.db", std::process::id(), n));
            for s in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{s}", path.display()));
            }
            TempDb { path }
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            for s in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{s}", self.path.display()));
            }
        }
    }

    fn ev(id: &str, title: &str, simhash: u64) -> Event {
        Event {
            id: id.to_string(),
            source_id: "jsonl:/p3".to_string(),
            kind: "log".to_string(),
            at: String::new(),
            at_epoch: 0,
            actor: Actor { name: "a".to_string(), key: "a".to_string() },
            title: title.to_string(),
            body: String::new(),
            level: String::new(),
            attrs: BTreeMap::new(),
            touches: vec![],
            links: vec![],
            simhash,
            is_bulk: false,
        }
    }

    /// Referencia: la implementación HISTÓRICA exacta de `similar` (escaneo
    /// completo, orden natural, `sort_by` estable por distancia, `truncate`).
    /// La salida del `similar()` real debe ser byte-idéntica a esta para
    /// cualquier `max_dist`.
    fn referencia(conn: &Connection, id: &str, max_dist: u32, limit: usize) -> Vec<SimilarRow> {
        let (id, target): (String, i64) = conn
            .query_row(
                "SELECT id, simhash FROM events WHERE id LIKE ?1||'%' ORDER BY id LIMIT 1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let target = target as u64;
        let mut stmt = conn.prepare("SELECT id, title, simhash FROM events WHERE id != ?1").unwrap();
        let rows = stmt
            .query_map(params![id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })
            .unwrap();
        let mut out = Vec::new();
        for row in rows {
            let (other_id, title, sh) = row.unwrap();
            let dist = (target ^ (sh as u64)).count_ones();
            if dist <= max_dist {
                out.push(SimilarRow { id: other_id, title, distance: dist });
            }
        }
        out.sort_by(|a, b| a.distance.cmp(&b.distance));
        out.truncate(limit);
        out
    }

    /// Inserta un conjunto variado y DETERMINISTA de eventos: ruido pseudo-
    /// aleatorio (LCG), cúmulos de casi-duplicados a 1-3 bits de un ancla y
    /// duplicados exactos (distancia 0) para forzar empates y ejercitar el
    /// orden de desempate.
    fn fixture() -> (TempDb, Store) {
        let db = TempDb::new();
        let mut store = Store::open(&db.path).unwrap();
        let mut evs: Vec<Event> = Vec::new();

        // Ruido: 200 simhashes pseudo-aleatorios (LCG de Numerical Recipes).
        let mut state: u64 = 0x1234_5678_9abc_def0;
        for i in 0..200u32 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            evs.push(ev(&format!("n{i:03}"), &format!("ruido evento numero {i}"), state));
        }

        // Ancla conocida y vecinos a distancia 1, 2, 3, 4 (una posición a la
        // vez, en bits de bandas distintas para cubrir todas).
        let anchor: u64 = 0xABCD_1234_5678_9ABC;
        evs.push(ev("anchor", "ancla de referencia", anchor));
        evs.push(ev("d1a", "vecino d1 a", anchor ^ (1 << 0)));
        evs.push(ev("d1b", "vecino d1 b", anchor ^ (1 << 50)));
        evs.push(ev("d2a", "vecino d2 a", anchor ^ (1 << 1) ^ (1 << 33)));
        evs.push(ev("d2b", "vecino d2 b", anchor ^ (1 << 20) ^ (1 << 60)));
        evs.push(ev("d3a", "vecino d3 a", anchor ^ (1 << 2) ^ (1 << 18) ^ (1 << 34)));
        evs.push(ev("d3b", "vecino d3 b", anchor ^ (1 << 5) ^ (1 << 40) ^ (1 << 63)));
        // Distancia 4: NO debe salir con max_dist<=3; verifica el límite.
        evs.push(ev("d4a", "vecino d4 a", anchor ^ 0xF));
        // Duplicados exactos del ancla (distancia 0): fuerzan empate a 0 con
        // ids insertados en orden conocido (orden natural = rowid).
        evs.push(ev("dup1", "duplicado exacto uno", anchor));
        evs.push(ev("dup2", "duplicado exacto dos", anchor));
        // Varios vecinos a la MISMA distancia 1 para empates no triviales.
        evs.push(ev("t1", "empate uno", anchor ^ (1 << 3)));
        evs.push(ev("t2", "empate dos", anchor ^ (1 << 7)));
        evs.push(ev("t3", "empate tres", anchor ^ (1 << 11)));

        {
            let mut w = store.writer().unwrap();
            for e in &evs {
                w.add_event(e).unwrap();
            }
            w.commit().unwrap();
        }
        (db, store)
    }

    /// Todos los ids del índice (para usar cada uno como ancla).
    fn todos_los_ids(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT id FROM events ORDER BY rowid").unwrap();
        let ids: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        ids
    }

    /// Paridad exhaustiva: para CADA evento como ancla y cada `max_dist` del
    /// camino de bandas (0..=3) y del escaneo (4,5), la salida de `similar`
    /// debe ser IDÉNTICA (ids, distancias y orden) a la referencia histórica.
    #[test]
    fn paridad_bandas_vs_escaneo_para_todas_las_anclas() {
        let (_db, store) = fixture();
        let conn = store.conn();
        let ids = todos_los_ids(conn);
        for limit in [1usize, 5, 15, 1000] {
            for &max_dist in &[0u32, 1, 2, 3, 4, 5] {
                for id in &ids {
                    let got = similar(conn, id, max_dist, limit).unwrap();
                    let want = referencia(conn, id, max_dist, limit);
                    assert_eq!(
                        got, want,
                        "divergencia ancla={id} max_dist={max_dist} limit={limit}"
                    );
                }
            }
        }
    }

    /// El camino de bandas recupera TODOS los vecinos a distancia <= 3 y
    /// EXCLUYE el de distancia 4 (verifica la garantía pigeonhole y el límite).
    #[test]
    fn bandas_recall_exacto_hasta_3_y_excluye_4() {
        let (_db, store) = fixture();
        let conn = store.conn();

        let sims = similar(conn, "anchor", 3, 1000).unwrap();
        let ids: Vec<&str> = sims.iter().map(|s| s.id.as_str()).collect();
        for v in ["d1a", "d1b", "d2a", "d2b", "d3a", "d3b", "dup1", "dup2", "t1", "t2", "t3"] {
            assert!(ids.contains(&v), "falta vecino {v} a distancia <=3");
        }
        assert!(!ids.contains(&"d4a"), "d4a (distancia 4) no debe salir con max_dist=3");

        // Distancia 0 para los duplicados exactos; el ancla nunca se incluye.
        for s in &sims {
            if s.id == "dup1" || s.id == "dup2" {
                assert_eq!(s.distance, 0);
            }
        }
        assert!(!ids.contains(&"anchor"));

        // Orden ascendente por distancia.
        for w in sims.windows(2) {
            assert!(w[0].distance <= w[1].distance);
        }
    }

    /// Con `max_dist = 4` (el valor por defecto de la CLI/MCP) el resultado
    /// incluye el vecino a distancia 4 y sigue siendo idéntico a la referencia.
    #[test]
    fn default_max_dist_4_usa_escaneo_e_incluye_distancia_4() {
        let (_db, store) = fixture();
        let conn = store.conn();
        let got = similar(conn, "anchor", 4, 1000).unwrap();
        assert!(got.iter().any(|s| s.id == "d4a" && s.distance == 4));
        assert_eq!(got, referencia(conn, "anchor", 4, 1000));
    }
}
