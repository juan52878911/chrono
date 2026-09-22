//! `correlate <id>`: eventos de OTRAS fuentes cercanos en el tiempo a un evento
//! ancla (±Δt). El valor de la multi-fuente: ligar un deploy con los logs de
//! error que lo siguen, un incidente con los commits de esa ventana, etc.
//! Consulta log-native (R3 oleada 2). Ver `docs/DESIGN-GENERAL-CORE.md §3`.

use rusqlite::{params, Connection, OptionalExtension};

use crate::Result;

/// Referencia compacta a un evento (el ancla o un correlacionado).
#[derive(Debug, Clone, PartialEq)]
pub struct EventRef {
    pub id: String,
    pub source_id: String,
    pub kind: String,
    pub at: String,
    pub at_epoch: i64,
    pub level: String,
    pub title: String,
    /// `at_epoch - ancla.at_epoch`: negativo = antes del ancla, positivo =
    /// después. En el ancla vale 0.
    pub delta_secs: i64,
}

/// Resultado de `correlate`: el ancla y los eventos correlacionados.
#[derive(Debug, Clone, PartialEq)]
pub struct Correlation {
    pub anchor: EventRef,
    pub related: Vec<EventRef>,
}

/// Eventos de fuentes DISTINTAS a la del ancla dentro de `±delta_secs` de su
/// `at_epoch` (excluye el propio ancla y los eventos de su misma `source_id`:
/// correlacionar es cruzar fuentes).
///
/// `delta_secs` debe ser > 0. Orden determinista de `related`: por
/// `abs(delta_secs)` ASC, luego `at_epoch` ASC, luego `id` ASC. `limit` acota
/// el nº de correlacionados. Devuelve error si `event_id` no existe.
pub fn correlate(
    conn: &Connection,
    event_id: &str,
    delta_secs: i64,
    limit: usize,
) -> Result<Correlation> {
    if delta_secs <= 0 {
        return Err("correlate: delta_secs debe ser > 0".into());
    }

    let anchor_row = conn
        .query_row(
            "SELECT id, source_id, kind, at, at_epoch, level, title FROM events WHERE id = ?1",
            params![event_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;

    let (id, source_id, kind, at, at_epoch, level, title) = anchor_row
        .ok_or_else(|| format!("correlate: evento no encontrado: {event_id}"))?;

    let anchor = EventRef { id, source_id, kind, at, at_epoch, level, title, delta_secs: 0 };

    let lo = anchor.at_epoch - delta_secs;
    let hi = anchor.at_epoch + delta_secs;

    // La ventana es cerrada en ambos extremos ([lo, hi]) y `source_id != ?2`
    // ya excluye por sí solo al propio ancla (comparte source_id consigo
    // misma), además de a cualquier evento de su misma fuente.
    let mut stmt = conn.prepare(
        "SELECT id, source_id, kind, at, at_epoch, level, title, (at_epoch - ?1) AS delta
         FROM events
         WHERE source_id != ?2
           AND at_epoch >= ?3
           AND at_epoch <= ?4
         ORDER BY ABS(at_epoch - ?1) ASC, at_epoch ASC, id ASC
         LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        params![anchor.at_epoch, anchor.source_id, lo, hi, limit as i64],
        |r| {
            Ok(EventRef {
                id: r.get(0)?,
                source_id: r.get(1)?,
                kind: r.get(2)?,
                at: r.get(3)?,
                at_epoch: r.get(4)?,
                level: r.get(5)?,
                title: r.get(6)?,
                delta_secs: r.get(7)?,
            })
        },
    )?;

    let mut related = Vec::new();
    for row in rows {
        related.push(row?);
    }
    Ok(Correlation { anchor, related })
}

#[cfg(test)]
mod tests {
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
                .join(format!("chrono-metrics-test-correlate-{name}-{}-{}.db", std::process::id(), n));
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

    fn mk_event(id: &str, source_id: &str, at_epoch: i64) -> Event {
        Event {
            id: id.to_string(),
            source_id: source_id.to_string(),
            kind: "event".to_string(),
            at: format!("1970-01-01T00:00:{at_epoch:02}Z"),
            at_epoch,
            actor: Actor::default(),
            title: format!("evento {id}"),
            ..Default::default()
        }
    }

    fn build_fixture() -> (TempDb, Store) {
        let db = TempDb::new("fixture");
        let mut store = Store::open(db.path()).unwrap();
        let evs = [
            mk_event("anchor1", "git:/repo", 1000),
            // Misma fuente que el ancla, dentro de la ventana: debe excluirse.
            mk_event("git2", "git:/repo", 1005),
            // Otras fuentes, dentro de ±60: delta -5, -5 (empate) y +5.
            mk_event("j2", "jsonl:/log", 995),
            mk_event("j1", "jsonl:/log", 995),
            mk_event("h1", "http:/svc", 1005),
            // Otra fuente, fuera de la ventana (delta 1000): debe excluirse.
            mk_event("far1", "jsonl:/log", 2000),
        ];
        let mut w = store.writer().unwrap();
        for e in &evs {
            w.add_event(e).unwrap();
        }
        w.commit().unwrap();
        (db, store)
    }

    #[test]
    fn correlate_cruza_fuentes_respeta_ventana_y_ordena_por_delta_y_desempates() {
        let (_db, store) = build_fixture();
        let corr = correlate(store.conn(), "anchor1", 60, 10).unwrap();

        assert_eq!(corr.anchor.id, "anchor1");
        assert_eq!(corr.anchor.delta_secs, 0);

        let ids: Vec<&str> = corr.related.iter().map(|r| r.id.as_str()).collect();
        // j1/j2 (delta -5, empate por at_epoch -> desempate por id) antes que
        // h1 (delta +5, mismo |delta| pero at_epoch mayor). git2 (misma fuente)
        // y far1 (fuera de ventana) no aparecen.
        assert_eq!(ids, vec!["j1", "j2", "h1"]);
        assert_eq!(corr.related[0].delta_secs, -5);
        assert_eq!(corr.related[1].delta_secs, -5);
        assert_eq!(corr.related[2].delta_secs, 5);
    }

    #[test]
    fn correlate_respeta_limit() {
        let (_db, store) = build_fixture();
        let corr = correlate(store.conn(), "anchor1", 60, 2).unwrap();
        assert_eq!(corr.related.len(), 2);
        assert_eq!(corr.related[0].id, "j1");
        assert_eq!(corr.related[1].id, "j2");
    }

    #[test]
    fn correlate_ventana_es_cerrada_en_ambos_extremos() {
        let db = TempDb::new("bordes");
        let mut store = Store::open(db.path()).unwrap();
        let evs = [
            mk_event("anchor", "git:/repo", 1000),
            mk_event("edge_lo", "jsonl:/log", 940),  // delta exacto -60: dentro.
            mk_event("edge_hi", "jsonl:/log", 1060), // delta exacto +60: dentro.
            mk_event("out_lo", "jsonl:/log", 939),   // fuera por 1s.
            mk_event("out_hi", "jsonl:/log", 1061),  // fuera por 1s.
        ];
        let mut w = store.writer().unwrap();
        for e in &evs {
            w.add_event(e).unwrap();
        }
        w.commit().unwrap();

        let corr = correlate(store.conn(), "anchor", 60, 10).unwrap();
        let ids: Vec<&str> = corr.related.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["edge_lo", "edge_hi"]);
    }

    #[test]
    fn correlate_falla_si_delta_no_es_positivo_o_el_evento_no_existe() {
        let (_db, store) = build_fixture();
        let conn = store.conn();
        assert!(correlate(conn, "anchor1", 0, 10).is_err());
        assert!(correlate(conn, "anchor1", -1, 10).is_err());
        assert!(correlate(conn, "no-existe", 60, 10).is_err());
    }
}
