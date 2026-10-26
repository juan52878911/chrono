//! `precede`: qué pasa JUSTO ANTES de algo que se repite. Agrupa en episodios
//! los eventos ancla (los que casan con un `EventScope`: un mensaje de apagado,
//! un despliegue fallido, un 500…) y, para cada episodio, cuenta lo que ocurrió
//! en la ventana previa (por defecto errores y avisos, con el nivel deducido
//! del texto si la fuente no lo trae). Además dice qué precursores se repiten en
//! varios episodios («catalog version mismatch: antes de 6 de 6»).
//!
//! Responde de una vez a «¿cuántas veces se reinició y qué lo precede?», que un
//! agente resuelve mal leyendo líneas: mezcla episodios, se le escapan los que
//! no tienen nivel y niega lo que no ve. Determinista: mismo índice, misma salida.

use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;

use crate::{EventScope, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Precursor {
    pub level: String,
    pub template: String,
    pub entity: String,
    pub count: i64,
    /// Segundos entre el último de estos eventos y el inicio del episodio.
    pub last_before_secs: i64,
    pub example_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub start_at: String,
    pub start_epoch: i64,
    pub end_epoch: i64,
    /// Eventos ancla en el episodio.
    pub anchors: i64,
    pub entity: String,
    /// Orígenes distintos de las anclas (contenedor/servicio), en orden de aparición:
    /// si hay más de uno, el episodio mezcla procesos y conviene separar con `attr`.
    pub origins: Vec<String>,
    pub first_id: String,
    pub preceding: Vec<Precursor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Common {
    pub level: String,
    pub template: String,
    /// En cuántos episodios aparece en la ventana previa.
    pub episodes: i64,
}

#[derive(Debug, Clone)]
pub struct PrecedeOpts<'a> {
    pub since_epoch: i64,
    /// Ancla: qué eventos marcan el episodio (grep, entity, attr, level…).
    pub anchor: EventScope<'a>,
    /// Hueco máximo (ms) entre anclas del mismo episodio.
    pub gap_ms: i64,
    /// Ventana previa (ms) que se mira antes del inicio de cada episodio.
    pub window_ms: i64,
    /// Niveles de lo que se cuenta como precursor (lista con comas); `*` =
    /// cualquiera, también sin nivel (fuentes cuyos pasos no llevan nivel, como
    /// las tools de una sesión).
    pub levels: &'a str,
    /// Solo precursores de esta(s) entidad(es) (por defecto, todas).
    pub entity: Option<&'a str>,
    pub max_episodes: usize,
    pub per_episode: usize,
    /// Episodios por ORIGEN (contenedor/servicio) en vez de mezclar orígenes que
    /// coinciden en el tiempo: el apagado de un preflight y el del servicio
    /// principal son episodios distintos, y lo del uno precede al otro.
    pub by_origin: bool,
}

impl Default for PrecedeOpts<'_> {
    fn default() -> Self {
        PrecedeOpts {
            since_epoch: 0,
            anchor: EventScope::default(),
            gap_ms: 120_000,
            window_ms: 120_000,
            levels: "FATAL,CRITICAL,ERROR,WARNING",
            entity: None,
            max_episodes: 50,
            per_episode: 8,
            by_origin: false,
        }
    }
}

/// Dónde pasó: el contenedor/host/pod si el evento lo trae (separa un preflight
/// del servicio principal), si no la entidad (el servicio, el fichero).
const ENTITY_SQL: &str = "COALESCE(json_extract(e.attrs, '$.container'), json_extract(e.attrs, '$.pod'),
                                   json_extract(e.attrs, '$.host'),
                                   (SELECT en.key FROM touches t JOIN entities en ON en.id = t.entity_id
                                    WHERE t.event_rid = e.rid ORDER BY en.key LIMIT 1), '')";

fn severity(level: &str) -> u8 {
    match level.to_ascii_uppercase().as_str() {
        "FATAL" | "PANIC" => 0,
        "CRITICAL" => 1,
        "ERROR" => 2,
        "WARNING" | "WARN" => 3,
        _ => 4,
    }
}

/// Episodios del ancla y lo que los precede, más los precursores comunes.
pub fn precede(conn: &Connection, o: &PrecedeOpts) -> Result<(Vec<Episode>, Vec<Common>)> {
    // 1. Anclas en orden temporal.
    let (filters, extra) = o.anchor.sql(2);
    let sql = format!(
        "SELECT e.id, e.at, e.at_epoch, {ENTITY_SQL} FROM events e
         WHERE e.at_epoch > 0 AND (?1 = 0 OR e.at_epoch >= ?1) {filters}
         ORDER BY e.at_epoch ASC, e.id ASC"
    );
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&o.since_epoch];
    for p in &extra {
        bound.push(p);
    }
    let mut stmt = conn.prepare(&sql)?;
    let anchors: Vec<(String, String, i64, String)> = stmt
        .query_map(rusqlite::params_from_iter(bound), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<std::result::Result<_, _>>()?;

    // 2. Episodios por hueco (por origen si se pide; si no, todos juntos).
    let mut episodes: Vec<Episode> = Vec::new();
    let mut open: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (id, at, epoch, entity) in anchors {
        let slot = if o.by_origin { open.get(&entity).copied() } else { episodes.len().checked_sub(1) };
        let last = slot.and_then(|i| episodes.get_mut(i));
        match last {
            Some(ep) if epoch - ep.end_epoch <= o.gap_ms => {
                ep.end_epoch = epoch;
                ep.anchors += 1;
                if !ep.origins.contains(&entity) {
                    ep.origins.push(entity);
                }
            }
            _ => {
                open.insert(entity.clone(), episodes.len());
                episodes.push(Episode {
                    start_at: at,
                    start_epoch: epoch,
                    end_epoch: epoch,
                    anchors: 1,
                    origins: vec![entity.clone()],
                    entity,
                    first_id: id,
                    preceding: Vec::new(),
                });
            }
        }
    }
    episodes.sort_by(|a, b| a.start_epoch.cmp(&b.start_epoch).then(a.entity.cmp(&b.entity)));
    episodes.truncate(o.max_episodes);

    // 3. Lo que precede a cada uno: niveles pedidos (con el deducido), ventana previa.
    let level = (o.levels.trim() != "*").then_some(o.levels);
    let pre_scope = EventScope { level, entity: o.entity, infer_level: true, ..Default::default() };
    let (pf, pextra) = pre_scope.sql(3);
    let level_sql = crate::timeline::level_expr("e", true);
    let pre_sql = format!(
        "SELECT e.id, e.at_epoch, e.title, {level_sql}, {ENTITY_SQL} FROM events e
         WHERE e.at_epoch >= ?1 AND e.at_epoch < ?2 {pf}
         ORDER BY e.at_epoch ASC, e.id ASC"
    );
    let mut pre = conn.prepare(&pre_sql)?;
    let mut common: BTreeMap<(String, String), i64> = BTreeMap::new();
    for ep in &mut episodes {
        let from = ep.start_epoch - o.window_ms;
        let to = ep.start_epoch;
        let mut b: Vec<&dyn rusqlite::ToSql> = vec![&from, &to];
        for p in &pextra {
            b.push(p);
        }
        let rows: Vec<(String, i64, String, String, String)> = pre
            .query_map(rusqlite::params_from_iter(b), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<std::result::Result<_, _>>()?;
        // (nivel, plantilla, entidad) -> (cuenta, último epoch, ejemplo)
        let mut agg: HashMap<(String, String, String), (i64, i64, String)> = HashMap::new();
        for (id, epoch, title, level, entity) in rows {
            // Lo del MISMO origen en el último segundo es el propio arranque del
            // episodio, no algo que lo preceda.
            if entity == ep.entity && to - epoch < 1000 {
                continue;
            }
            let key = (level.to_ascii_uppercase(), chrono_core::templatize(&title), entity);
            let e = agg.entry(key).or_insert((0, 0, String::new()));
            e.0 += 1;
            if epoch >= e.1 {
                e.1 = epoch;
                e.2 = id;
            }
        }
        let mut seen_here: std::collections::BTreeSet<(String, String)> = Default::default();
        let mut list: Vec<Precursor> = agg
            .into_iter()
            .map(|((level, template, entity), (count, last, example_id))| {
                seen_here.insert((level.clone(), template.clone()));
                Precursor { level, template, entity, count, last_before_secs: (to - last) / 1000, example_id }
            })
            .collect();
        for k in seen_here {
            *common.entry(k).or_insert(0) += 1;
        }
        list.sort_by(|a, b| {
            severity(&a.level)
                .cmp(&severity(&b.level))
                .then(b.count.cmp(&a.count))
                .then(a.last_before_secs.cmp(&b.last_before_secs))
                .then(a.template.cmp(&b.template))
        });
        list.truncate(o.per_episode);
        ep.preceding = list;
    }
    let mut commons: Vec<Common> = common
        .into_iter()
        .filter(|(_, n)| *n >= 2 || episodes.len() == 1)
        .map(|((level, template), episodes)| Common { level, template, episodes })
        .collect();
    commons.sort_by(|a, b| b.episodes.cmp(&a.episodes).then(severity(&a.level).cmp(&severity(&b.level))).then(a.template.cmp(&b.template)));
    commons.truncate(10);
    Ok((episodes, commons))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono_core::{Actor, Event, Touch};
    use chrono_store::Store;

    use super::*;

    fn ev(id: &str, secs: i64, level: &str, svc: &str, title: &str) -> Event {
        Event {
            id: id.into(),
            source_id: "jsonl:/x".into(),
            kind: "log".into(),
            at: format!("t{secs}"),
            at_epoch: secs * 1000,
            level: level.into(),
            title: title.into(),
            actor: Actor::default(),
            touches: vec![Touch { entity: svc.into(), entity_type: "service".into(), weight: 1, ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn episodios_y_precursores_comunes_con_nivel_deducido() {
        let path: PathBuf = std::env::temp_dir().join(format!("chrono-precede-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path).unwrap();
        {
            let mut w = store.writer().unwrap();
            // Dos reinicios (dos anclas cada uno, juntas) con un error 40 s antes; el
            // primero SIN nivel de la fuente (lo dice el texto).
            w.add_event(&ev("p1", 960, "", "pre", "ERROR:  catalog version mismatch")).unwrap();
            w.add_event(&ev("a1", 1000, "", "db", "LOG: received fast shutdown request")).unwrap();
            w.add_event(&ev("a2", 1010, "", "db", "LOG: received fast shutdown request")).unwrap();
            w.add_event(&ev("x1", 2000, "INFO", "db", "ok")).unwrap();
            w.add_event(&ev("p2", 4960, "ERROR", "pre", "ERROR:  catalog version mismatch")).unwrap();
            w.add_event(&ev("w2", 4970, "WARNING", "api", "WARNING: slow")).unwrap();
            w.add_event(&ev("a3", 5000, "", "db", "LOG: received fast shutdown request")).unwrap();
            w.commit().unwrap();
        }
        let o = PrecedeOpts { anchor: EventScope { grep: Some("fast shutdown"), ..Default::default() }, ..Default::default() };
        let (eps, common) = precede(store.conn(), &o).unwrap();
        assert_eq!(eps.len(), 2, "{eps:?}");
        assert_eq!(eps[0].anchors, 2);
        assert_eq!(eps[0].preceding[0].template, "ERROR: catalog version mismatch");
        assert_eq!(eps[0].preceding[0].last_before_secs, 40);
        assert_eq!(eps[1].preceding.len(), 2);
        assert_eq!(common.len(), 1, "solo el mismatch está en los dos: {common:?}");
        assert_eq!(common[0].episodes, 2);
        let _ = std::fs::remove_file(&path);
    }
}
