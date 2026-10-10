//! `trace <id>`: sigue la CADENA de un evento por la tabla `links` (relaciones
//! deterministas: `reverts`, `ticket`, `pr`…), a diferencia de `correlate` que
//! va por cercanía temporal (±Δt). Responde "qué está conectado con esto y
//! cómo": el commit que revierte a otro, todos los commits de un mismo ticket o
//! PR, etc. Recorrido en anchura (BFS) acotado por profundidad.

use std::collections::{BTreeMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};

use crate::events::EventRow;
use crate::Result;

/// Un evento alcanzado al trazar, con cómo y a qué distancia se llegó.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceNode {
    pub event: EventRow,
    /// Profundidad BFS desde el ancla (1 = conectado directamente).
    pub depth: i64,
    /// Cómo se conectó al grafo: `"reverts"`, `"ticket:PROJ-1"`, `"pr:42"`…
    /// (la `rel` del link, con el `target` si la conexión es por target compartido).
    pub rel: String,
}

/// Resultado de `trace`: el ancla y los eventos conectados por `links`.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub anchor: EventRow,
    pub nodes: Vec<TraceNode>,
}

/// Traza la cadena de `links` desde `event_id` (BFS, profundidad ≤ `max_depth`,
/// a lo sumo `limit` nodos). Aristas entre eventos A y B:
/// 1. A y B comparten un link `(rel, target)` idéntico (co-ticket / co-pr /
///    revierten lo mismo) → `rel = "<rel>:<target>"`.
/// 2. A tiene un link `(rel, target)` con `target == B.id` (A referencia a B,
///    p.ej. A revierte el commit B), y su inverso → `rel = "<rel>"`.
///
/// Orden determinista de `nodes`: por `depth` ASC, luego `at_epoch` ASC, luego
/// `id` ASC. Error si `event_id` no existe. `max_depth`/`limit` deben ser > 0.
///
/// Recorrido por niveles: de la frontera actual se calculan todos los vecinos
/// no visitados; si un vecino es alcanzable por varias aristas en el mismo
/// nivel se queda con la `rel` menor lexicográficamente. Si el nivel no cabe
/// en `limit`, se admiten primero los de menor `(at_epoch, id)`.
pub fn trace(conn: &Connection, event_id: &str, max_depth: i64, limit: usize) -> Result<Trace> {
    if max_depth <= 0 {
        return Err("trace: max_depth debe ser > 0".into());
    }
    if limit == 0 {
        return Err("trace: limit debe ser > 0".into());
    }

    let anchor = fetch_event(conn, event_id)?
        .ok_or_else(|| format!("trace: evento no encontrado: {event_id}"))?;

    let mut visitados: HashSet<String> = HashSet::new();
    visitados.insert(anchor.id.clone());
    let mut frontera: Vec<String> = vec![anchor.id.clone()];
    let mut nodes: Vec<TraceNode> = Vec::new();

    let mut depth = 1;
    while depth <= max_depth && nodes.len() < limit && !frontera.is_empty() {
        // id vecino -> rel elegida (la menor) para este nivel.
        let mut candidatos: BTreeMap<String, String> = BTreeMap::new();
        for nodo in &frontera {
            for (vecino, rel) in vecinos(conn, nodo)? {
                if visitados.contains(&vecino) {
                    continue;
                }
                candidatos
                    .entry(vecino)
                    .and_modify(|actual| {
                        if rel < *actual {
                            *actual = rel.clone();
                        }
                    })
                    .or_insert(rel);
            }
        }

        // Resolver a EventRow (un id sin evento no es nodo) y ordenar el nivel.
        let mut nivel: Vec<TraceNode> = Vec::new();
        for (id, rel) in candidatos {
            if let Some(event) = fetch_event(conn, &id)? {
                nivel.push(TraceNode { event, depth, rel });
            }
        }
        nivel.sort_by(|a, b| (a.event.at_epoch, &a.event.id).cmp(&(b.event.at_epoch, &b.event.id)));
        nivel.truncate(limit - nodes.len());

        frontera = nivel.iter().map(|n| n.event.id.clone()).collect();
        for n in &nivel {
            visitados.insert(n.event.id.clone());
        }
        nodes.extend(nivel);
        depth += 1;
    }

    nodes.sort_by(|a, b| {
        (a.depth, a.event.at_epoch, &a.event.id).cmp(&(b.depth, b.event.at_epoch, &b.event.id))
    });
    Ok(Trace { anchor, nodes })
}

/// Lee un evento por id (`None` si no existe).
fn fetch_event(conn: &Connection, id: &str) -> Result<Option<EventRow>> {
    Ok(conn
        .query_row(
            "SELECT e.id, s.id, e.kind, e.at, e.at_epoch, e.level, e.title
             FROM events e JOIN sources s ON s.ref = e.source_ref WHERE e.id = ?1",
            params![id],
            |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    source_id: r.get(1)?,
                    kind: r.get(2)?,
                    at: r.get(3)?,
                    at_epoch: r.get(4)?,
                    level: r.get(5)?,
                    title: r.get(6)?,
                })
            },
        )
        .optional()?)
}

/// Vecinos de `nodo` como pares `(id_vecino, rel)`, sin filtrar visitados ni
/// comprobar que el id exista en `events` (lo resuelve el llamador).
fn vecinos(conn: &Connection, nodo: &str) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();

    // Links propios del nodo: (rel, target).
    let propios: Vec<(String, String)> = {
        let mut stmt = conn.prepare("SELECT rel, target FROM links WHERE event_id = ?1")?;
        let rows = stmt.query_map(params![nodo], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    let mut mismo_target = conn
        .prepare("SELECT event_id FROM links WHERE rel = ?1 AND target = ?2 AND event_id != ?3")?;
    for (rel, target) in &propios {
        // 1. Target compartido (co-ticket, co-pr, revierten lo mismo).
        let rows = mismo_target.query_map(params![rel, target, nodo], |r| r.get::<_, String>(0))?;
        for id in rows {
            out.push((id?, format!("{rel}:{target}")));
        }
        // 2a. Referencia directa: el target es el id de otro evento.
        out.push((target.clone(), rel.clone()));
    }

    // 2b. Referencia inversa: eventos cuyo link apunta a este nodo.
    let mut apuntan = conn.prepare("SELECT event_id, rel FROM links WHERE target = ?1")?;
    let rows = apuntan.query_map(params![nodo], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for fila in rows {
        out.push(fila?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono_core::{Actor, Event, Link};
    use chrono_store::Store;

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "chrono-metrics-test-trace-{name}-{}-{}.db",
                std::process::id(),
                n
            ));
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

    fn lk(rel: &str, target: &str) -> Link {
        Link {
            rel: rel.to_string(),
            target: target.to_string(),
        }
    }

    fn mk_event(id: &str, at_epoch: i64, links: Vec<Link>) -> Event {
        Event {
            id: id.to_string(),
            source_id: "git:/repo".to_string(),
            kind: "commit".to_string(),
            at: format!("1970-01-01T00:00:{at_epoch:02}Z"),
            at_epoch,
            actor: Actor::default(),
            title: format!("evento {id}"),
            links,
            ..Default::default()
        }
    }

    fn build(name: &str, evs: &[Event]) -> (TempDb, Store) {
        let db = TempDb::new(name);
        let mut store = Store::open(db.path()).unwrap();
        let mut w = store.writer().unwrap();
        for e in evs {
            w.add_event(e).unwrap();
        }
        w.commit().unwrap();
        (db, store)
    }

    fn resumen(t: &Trace) -> Vec<(&str, i64, &str)> {
        t.nodes
            .iter()
            .map(|n| (n.event.id.as_str(), n.depth, n.rel.as_str()))
            .collect()
    }

    #[test]
    fn reverts_se_sigue_en_ambos_sentidos() {
        let (_db, store) = build(
            "reverts",
            &[
                mk_event("B", 10, vec![]),
                mk_event("A", 20, vec![lk("reverts", "B")]),
            ],
        );
        let t = trace(store.conn(), "A", 3, 10).unwrap();
        assert_eq!(t.anchor.id, "A");
        assert_eq!(resumen(&t), vec![("B", 1, "reverts")]);

        let t = trace(store.conn(), "B", 3, 10).unwrap();
        assert_eq!(resumen(&t), vec![("A", 1, "reverts")]);
    }

    #[test]
    fn ticket_compartido_conecta_con_target_en_la_rel() {
        let (_db, store) = build(
            "ticket",
            &[
                mk_event("c1", 10, vec![lk("ticket", "PROJ-1")]),
                mk_event("c2", 20, vec![lk("ticket", "PROJ-1")]),
                mk_event("c3", 30, vec![lk("ticket", "PROJ-1")]),
                mk_event("otro", 40, vec![lk("ticket", "PROJ-2")]),
            ],
        );
        let t = trace(store.conn(), "c1", 3, 10).unwrap();
        assert_eq!(
            resumen(&t),
            vec![("c2", 1, "ticket:PROJ-1"), ("c3", 1, "ticket:PROJ-1")]
        );
    }

    #[test]
    fn max_depth_corta_la_cadena() {
        // A revierte B, B revierte C: A -> B (1) -> C (2).
        let (_db, store) = build(
            "profundidad",
            &[
                mk_event("C", 10, vec![]),
                mk_event("B", 20, vec![lk("reverts", "C")]),
                mk_event("A", 30, vec![lk("reverts", "B")]),
            ],
        );
        let t = trace(store.conn(), "A", 1, 10).unwrap();
        assert_eq!(resumen(&t), vec![("B", 1, "reverts")]);

        let t = trace(store.conn(), "A", 2, 10).unwrap();
        assert_eq!(resumen(&t), vec![("B", 1, "reverts"), ("C", 2, "reverts")]);
    }

    #[test]
    fn limit_acota_y_admite_primero_los_mas_antiguos() {
        let (_db, store) = build(
            "limit",
            &[
                mk_event("c1", 10, vec![lk("ticket", "T-1")]),
                mk_event("c2", 20, vec![lk("ticket", "T-1")]),
                mk_event("c3", 30, vec![lk("ticket", "T-1")]),
                mk_event("c4", 40, vec![lk("ticket", "T-1")]),
            ],
        );
        let t = trace(store.conn(), "c1", 3, 2).unwrap();
        assert_eq!(
            resumen(&t),
            vec![("c2", 1, "ticket:T-1"), ("c3", 1, "ticket:T-1")]
        );
    }

    #[test]
    fn parametros_invalidos_y_ancla_inexistente_fallan() {
        let (_db, store) = build("errores", &[mk_event("A", 10, vec![])]);
        let conn = store.conn();
        assert!(trace(conn, "A", 0, 10).is_err());
        assert!(trace(conn, "A", 1, 0).is_err());
        let e = trace(conn, "nope", 1, 10).unwrap_err().to_string();
        assert_eq!(e, "trace: evento no encontrado: nope");
        // Ancla sin links: traza vacía, no error.
        assert!(trace(conn, "A", 1, 10).unwrap().nodes.is_empty());
    }

    #[test]
    fn orden_por_depth_epoch_id_y_rel_minima_en_empate() {
        // A revierte X (ref) y comparte ticket con X (id "X" también está en el
        // ticket): X alcanzable por "reverts" y "ticket:T-1" -> gana "reverts".
        // b1/b2 con mismo at_epoch -> desempate por id. Z a profundidad 2 vía X.
        let (_db, store) = build(
            "orden",
            &[
                mk_event("A", 50, vec![lk("reverts", "X"), lk("ticket", "T-1")]),
                mk_event("X", 40, vec![lk("ticket", "T-1"), lk("pr", "7")]),
                mk_event("b2", 5, vec![lk("ticket", "T-1")]),
                mk_event("b1", 5, vec![lk("ticket", "T-1")]),
                mk_event("Z", 1, vec![lk("pr", "7")]),
            ],
        );
        let t = trace(store.conn(), "A", 3, 10).unwrap();
        assert_eq!(
            resumen(&t),
            vec![
                ("b1", 1, "ticket:T-1"),
                ("b2", 1, "ticket:T-1"),
                ("X", 1, "reverts"),
                ("Z", 2, "pr:7"),
            ]
        );
        // Reproducible.
        assert_eq!(t, trace(store.conn(), "A", 3, 10).unwrap());
    }
}
