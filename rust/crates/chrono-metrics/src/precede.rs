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
    /// En cuántos episodios aparece en la ventana previa (de TODOS, no solo
    /// de los devueltos).
    pub episodes: i64,
    /// Episodios cuya ventana previa fue activa para las fuentes de la
    /// plantilla (al menos un evento suyo): la población de `expected`.
    pub active_episodes: Option<i64>,
    /// Frecuencia base (P-MSR-3): de las ventanas del mismo tamaño en el periodo
    /// en que las fuentes de la plantilla tuvieron actividad, la fracción que
    /// contiene la plantilla (con cualquier nivel), desde los `rollups` de 60 s.
    /// `None` si la plantilla no está en los rollups o con `join` (la base de
    /// todo el índice no es la de dentro de una clave).
    pub base_rate: Option<f64>,
    /// `base_rate × active_episodes`: en cuántos estaría por azar.
    pub expected: Option<f64>,
    /// `(episodes / active_episodes) / base_rate`: >1, aparece antes más que por azar.
    pub lift: Option<f64>,
}

/// Lo que devuelve [`precede`]: los episodios (acotados a `max_episodes`) y
/// las cifras de TODOS (`episodes_total`, precursores comunes).
#[derive(Debug, Clone, PartialEq)]
pub struct Preceded {
    pub episodes: Vec<Episode>,
    pub episodes_total: usize,
    pub common: Vec<Common>,
    /// Con `join`: anclas sin ningún valor de la clave (no forman episodio).
    pub anchors_without_key: usize,
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
    /// Clave de relación (P-REL-2): los episodios se separan por su valor y
    /// solo cuentan los precursores con ese mismo valor (la misma sesión, la
    /// misma traza), además de la ventana.
    pub join: Option<&'a str>,
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
            join: None,
            max_episodes: 50,
            per_episode: 8,
            by_origin: false,
        }
    }
}

/// Dónde pasó: el contenedor/host/pod si el evento lo trae (separa un preflight
/// del servicio principal), si no la entidad (el servicio, el fichero).
fn entity_sql() -> String {
    let a = |k: &str| crate::timeline::attr_sql("e", k, &format!("'$.{k}'"));
    format!(
        "COALESCE({}, {}, {}, (SELECT en.key FROM touches t JOIN entities en ON en.id = t.entity_id
                                WHERE t.event_rid = e.rid ORDER BY en.key LIMIT 1), '')",
        a("container"),
        a("pod"),
        a("host")
    )
}

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
/// Las cifras (`episodes_total`, «antes de x/N», lift) salen de TODOS los
/// episodios; `max_episodes` solo acota los que se devuelven.
pub fn precede(conn: &Connection, o: &PrecedeOpts) -> Result<Preceded> {
    let join = o.join.map(str::trim).filter(|k| !k.is_empty());
    crate::relate::check_join(conn, join).map_err(|e| format!("precede: {e}"))?;
    crate::relate::check_join(conn, o.anchor.join)?;
    // 1. Anclas en orden temporal (con sus valores de la clave, si se pide,
    // separados por \x1f: un ancla con varios cuenta en cada uno).
    let (filters, extra) = o.anchor.sql(2);
    let key_col = match join {
        Some(k) => format!(
            "(SELECT group_concat(value, char(31)) FROM (SELECT DISTINCT kv.value AS value FROM ({}) kv WHERE kv.rid = e.rid ORDER BY 1))",
            crate::relate::key_values_sql(k)
        ),
        None => "''".to_string(),
    };
    let sql = format!(
        "SELECT e.id, e.at, e.at_epoch, {entity}, {key_col} FROM events e
         WHERE e.at_epoch > 0 AND (?1 = 0 OR e.at_epoch >= ?1) {filters}
         ORDER BY e.at_epoch ASC, e.id ASC",
        entity = entity_sql()
    );
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&o.since_epoch];
    for p in &extra {
        bound.push(p);
    }
    let mut stmt = conn.prepare(&sql)?;
    let anchors: Vec<(String, String, i64, String, Option<String>)> = stmt
        .query_map(rusqlite::params_from_iter(bound), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<std::result::Result<_, _>>()?;

    // 2. Episodios por hueco (por valor de la clave o por origen si se pide;
    // si no, todos juntos). Con clave, un ancla sin ella no forma episodio (se
    // cuenta en `anchors_without_key`) y una con varios valores entra en el
    // episodio de cada uno.
    let mut episodes: Vec<Episode> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut open: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut anchors_without_key = 0;
    let anchors: Vec<(String, String, i64, String, String)> = anchors
        .into_iter()
        .flat_map(|(id, at, epoch, entity, key)| {
            let values: Vec<String> = match key {
                Some(k) if join.is_some() => k.split('\x1f').filter(|v| !v.is_empty()).map(str::to_string).collect(),
                _ if join.is_some() => Vec::new(),
                _ => vec![String::new()],
            };
            if values.is_empty() {
                anchors_without_key += 1;
            }
            values.into_iter().map(move |v| (id.clone(), at.clone(), epoch, entity.clone(), v))
        })
        .collect();
    for (id, at, epoch, entity, key) in anchors {
        let group = if join.is_some() { &key } else { &entity };
        let slot = if join.is_some() || o.by_origin { open.get(group).copied() } else { episodes.len().checked_sub(1) };
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
                open.insert(group.clone(), episodes.len());
                keys.push(key.clone());
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
    let mut order: Vec<usize> = (0..episodes.len()).collect();
    order.sort_by(|&a, &b| episodes[a].start_epoch.cmp(&episodes[b].start_epoch).then(episodes[a].entity.cmp(&episodes[b].entity)));
    let mut episodes: Vec<(Episode, String)> = order.into_iter().map(|i| (episodes[i].clone(), keys[i].clone())).collect();
    let episodes_total = episodes.len();

    // 3. Lo que precede a cada uno: niveles pedidos (con el deducido), ventana previa.
    let level = (o.levels.trim() != "*").then_some(o.levels);
    let pre_scope = EventScope { level, entity: o.entity, infer_level: true, ..Default::default() };
    let (pf, pextra) = pre_scope.sql(3);
    let level_sql = crate::timeline::level_expr("e", true);
    let same_key = match join {
        Some(k) => format!("AND e.rid IN ({})", crate::relate::key_rids_sql(k, &format!("?{}", 3 + pextra.len()), false)),
        None => String::new(),
    };
    let pre_sql = format!(
        "SELECT e.id, e.at_epoch, e.title, {level_sql}, {entity} FROM events e
         WHERE e.at_epoch >= ?1 AND e.at_epoch < ?2 {pf} {same_key}
         ORDER BY e.at_epoch ASC, e.id ASC",
        entity = entity_sql()
    );
    let mut pre = conn.prepare(&pre_sql)?;
    let mut common: BTreeMap<(String, String), i64> = BTreeMap::new();
    for (ep, key) in &mut episodes {
        let from = ep.start_epoch - o.window_ms;
        let to = ep.start_epoch;
        let mut b: Vec<&dyn rusqlite::ToSql> = vec![&from, &to];
        for p in &pextra {
            b.push(p);
        }
        if join.is_some() {
            b.push(&*key);
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
    let candidates: Vec<((String, String), i64)> = common.into_iter().filter(|(_, n)| *n >= 2 || episodes_total == 1).collect();
    let templates: Vec<&str> = candidates.iter().map(|((_, t), _)| t.as_str()).collect();
    // Con clave, la base temporal de todo el índice no es la de dentro de una
    // sesión o traza: sin frecuencia base por clave, ni esperado ni lift.
    let rates = if join.is_some() { HashMap::new() } else { base_rates(conn, &templates, o.window_ms, o.since_epoch, o.anchor.until_epoch)? };
    let mut commons: Vec<Common> = candidates
        .into_iter()
        .map(|((level, template), n)| {
            let base = rates.get(&template).filter(|b| b.rate > 0.0);
            // Lo esperado se compara con los episodios cuya ventana previa fue
            // activa para la fuente de la plantilla (en los demás no podía estar).
            let active = base.map(|b| episodes.iter().filter(|(ep, _)| b.is_active(ep.start_epoch)).count().max(n as usize) as i64);
            let base_rate = base.map(|b| b.rate);
            let expected = base_rate.zip(active).map(|(r, a)| r * a as f64);
            let lift = base_rate.zip(active).map(|(r, a)| (n as f64 / a as f64) / r);
            Common { level, template, episodes: n, active_episodes: active, base_rate, expected, lift }
        })
        .collect();
    // Primero lo que más episodios explica POR ENCIMA del azar: una plantilla
    // que sale cada minuto «precede» a todo y no aporta (exceso ≈ 0). Sin
    // frecuencia base, el exceso es el número de episodios.
    let excess = |c: &Common| c.episodes as f64 - c.expected.unwrap_or(0.0);
    commons.sort_by(|a, b| {
        excess(b)
            .partial_cmp(&excess(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.episodes.cmp(&a.episodes))
            .then(severity(&a.level).cmp(&severity(&b.level)))
            .then(a.template.cmp(&b.template))
    });
    commons.truncate(10);
    let episodes: Vec<Episode> = episodes.into_iter().take(o.max_episodes).map(|(e, _)| e).collect();
    Ok(Preceded { episodes, episodes_total, common: commons, anchors_without_key })
}

/// Frecuencia base de una plantilla (P-MSR-3) y dónde vale.
struct Base {
    /// Fracción de las ventanas ACTIVAS que contienen la plantilla.
    rate: f64,
    /// Finales de ventana activos (unión de intervalos `[s, e)`, ordenados):
    /// aquellos en que alguna fuente de la plantilla tuvo al menos un evento.
    active: std::rc::Rc<Vec<(i64, i64)>>,
}

impl Base {
    /// ¿La ventana que acaba en `end` es activa?
    fn is_active(&self, end: i64) -> bool {
        let i = self.active.partition_point(|&(_, e)| e <= end);
        self.active.get(i).is_some_and(|&(s, _)| s <= end)
    }
}

/// Une los intervalos de finales de ventana que cubre cada bucket ocupado
/// (`[b, b+bucket+window)`, recortados a `[lo, hi)`). `bs` ordenados.
fn cover(bs: &[i64], bucket: i64, window_ms: i64, lo: i64, hi: i64) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for &b in bs {
        let (s, e) = (b.max(lo), (b + bucket + window_ms).min(hi));
        if e <= s {
            continue;
        }
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

fn span(iv: &[(i64, i64)]) -> i64 {
    iv.iter().map(|(s, e)| e - s).sum()
}

/// Frecuencia base de cada plantilla: la fracción de las ventanas de
/// `window_ms` (su final recorriendo el periodo) que contienen al menos un
/// evento con ella, desde `rollups`, contada solo sobre las ventanas en que
/// las fuentes de la plantilla tuvieron actividad (al menos un evento). Sobre
/// todo el periodo, una fuente a ráfagas (sesiones de Claude: horas sin nada)
/// daba una base baja y casi todo salía con lift > 1. Un bucket ocupado
/// `[b, b+bucket)` cubre los finales de ventana en `[b, b+bucket+window)`; se
/// unen los intervalos. O(buckets de esas fuentes), sin tocar `events`.
fn base_rates(conn: &Connection, templates: &[&str], window_ms: i64, since: i64, until: i64) -> Result<HashMap<String, Base>> {
    let mut out = HashMap::new();
    if templates.is_empty() {
        return Ok(out);
    }
    // Rollups atrasados (un `tail`, eventos borrados) darían una base baja y un
    // lift inflado: sin ellos al día, no hay frecuencia base.
    let fresh: bool = conn.query_row(
        "SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'rollup_max_rowid'), -1) >= (SELECT COALESCE(MAX(rid), 0) FROM events)",
        [],
        |r| r.get(0),
    )?;
    if !fresh {
        return Ok(out);
    }
    let bucket: i64 = conn
        .query_row("SELECT value FROM meta WHERE key = 'rollup_bucket_ms'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60_000);
    let (lo, hi): (Option<i64>, Option<i64>) =
        conn.query_row("SELECT MIN(at_epoch), MAX(at_epoch) FROM events WHERE at_epoch > 0", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let (Some(mut lo), Some(mut hi)) = (lo, hi) else { return Ok(out) };
    if since > 0 {
        lo = lo.max(since);
    }
    if until > 0 {
        hi = hi.min(until);
    }
    if hi <= lo {
        return Ok(out);
    }
    let wanted: std::collections::HashSet<&str> = templates.iter().copied().collect();
    let mut ids: HashMap<i64, String> = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, template FROM templates")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, t) = row?;
        if wanted.contains(t.as_str()) {
            ids.insert(id, t);
        }
    }
    if ids.is_empty() {
        return Ok(out);
    }
    // Buckets de cada plantilla y las fuentes en que aparece.
    let mut buckets: HashMap<&str, (Vec<i64>, std::collections::BTreeSet<i64>)> = HashMap::new();
    let range = "bucket_epoch > 0 AND bucket_epoch < ?1 AND bucket_epoch + ?2 + ?3 > ?4";
    let mut stmt = conn.prepare(&format!("SELECT template_id, bucket_epoch, source_ref FROM rollups WHERE {range}"))?;
    for row in stmt.query_map(rusqlite::params![hi, bucket, window_ms, lo], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
        let (tid, b, src) = row?;
        if let Some(t) = ids.get(&tid) {
            let e = buckets.entry(t.as_str()).or_default();
            e.0.push(b);
            e.1.insert(src);
        }
    }
    // Ventanas activas por conjunto de fuentes (casi siempre una, compartida).
    let mut active_of: HashMap<Vec<i64>, std::rc::Rc<Vec<(i64, i64)>>> = HashMap::new();
    let mut src_stmt = conn.prepare(&format!("SELECT DISTINCT bucket_epoch FROM rollups WHERE source_ref = ?5 AND {range}"))?;
    for (t, (mut bs, srcs)) in buckets {
        let srcs: Vec<i64> = srcs.into_iter().collect();
        let active = match active_of.get(&srcs) {
            Some(a) => a.clone(),
            None => {
                let mut all: Vec<i64> = Vec::new();
                for s in &srcs {
                    for b in src_stmt.query_map(rusqlite::params![hi, bucket, window_ms, lo, s], |r| r.get::<_, i64>(0))? {
                        all.push(b?);
                    }
                }
                all.sort_unstable();
                all.dedup();
                let a = std::rc::Rc::new(cover(&all, bucket, window_ms, lo, hi));
                active_of.insert(srcs, a.clone());
                a
            }
        };
        bs.sort_unstable();
        bs.dedup();
        let (covered, total) = (span(&cover(&bs, bucket, window_ms, lo, hi)), span(&active));
        if total > 0 {
            out.insert(t.to_string(), Base { rate: (covered as f64 / total as f64).min(1.0), active });
        }
    }
    Ok(out)
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
        let r = precede(store.conn(), &o).unwrap();
        let (eps, common) = (r.episodes, r.common);
        assert_eq!(eps.len(), 2, "{eps:?}");
        assert_eq!(eps[0].anchors, 2);
        assert_eq!(eps[0].preceding[0].template, "ERROR: catalog version mismatch");
        assert_eq!(eps[0].preceding[0].last_before_secs, 40);
        assert_eq!(eps[1].preceding.len(), 2);
        assert_eq!(common.len(), 1, "solo el mismatch está en los dos: {common:?}");
        assert_eq!(common[0].episodes, 2);
        let _ = std::fs::remove_file(&path);
    }

    fn temp_store(tag: &str) -> (PathBuf, Store) {
        let path: PathBuf = std::env::temp_dir().join(format!("chrono-precede-{tag}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        (path, store)
    }

    #[test]
    fn las_cifras_salen_de_todos_los_episodios_y_el_ubicuo_baja() {
        let (path, mut store) = temp_store("lift");
        {
            let mut w = store.writer().unwrap();
            // 30 caídas cada hora; 20 s antes de cada una, un aviso `disk full` (raro
            // fuera de ahí); y un ERROR `heartbeat lost` cada 5 min todo el periodo
            // (sale antes de casi todo, por azar). El ubicuo es MÁS grave: por
            // severidad saldría primero; solo el exceso sobre el azar lo baja.
            for i in 0..30i64 {
                let t = 3600 * (i + 1);
                w.add_event(&ev(&format!("a{i}"), t, "FATAL", "db", "FATAL: shutting down")).unwrap();
                w.add_event(&ev(&format!("d{i}"), t - 20, "WARNING", "disk", "WARNING: disk full")).unwrap();
            }
            for k in 0..(31 * 12) {
                w.add_event(&ev(&format!("h{k}"), 300 * k + 7, "ERROR", "net", "ERROR: heartbeat lost")).unwrap();
            }
            w.commit().unwrap();
        }
        store.build_rollups(60_000).unwrap();
        store.set_meta("rollup_bucket_ms", "60000").unwrap();
        let o = PrecedeOpts {
            anchor: EventScope { level: Some("FATAL"), ..Default::default() },
            window_ms: 600_000,
            max_episodes: 5,
            ..Default::default()
        };
        let r = precede(store.conn(), &o).unwrap();
        assert_eq!(r.episodes.len(), 5);
        assert_eq!(r.episodes_total, 30, "el total no se recorta con max_episodes");
        let by = |t: &str| r.common.iter().find(|c| c.template.contains(t)).unwrap_or_else(|| panic!("{t}: {:?}", r.common));
        let (disk, hb) = (by("disk full"), by("heartbeat"));
        assert_eq!(disk.episodes, 30);
        assert_eq!(hb.episodes, 30);
        assert!(hb.base_rate.unwrap() > 0.9, "{hb:?}");
        assert!(disk.lift.unwrap() > 3.0 * hb.lift.unwrap(), "{disk:?} {hb:?}");
        assert_eq!(r.common[0].template, disk.template, "el ubicuo deja de salir primero: {:?}", r.common);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn la_base_sale_de_las_ventanas_activas_de_la_fuente() {
        let (path, mut store) = temp_store("lift-rafagas");
        {
            let mut w = store.writer().unwrap();
            // Una fuente a ráfagas (como las sesiones): 10 ráfagas de 10 min cada 10 h.
            // Dentro de cada una, un `ruido` cada minuto, una caída en el minuto 8 y
            // un `disco lento` 20 s antes. Fuera, nada: sobre todo el periodo el ruido
            // parecería raro (lift ≈ 50) aunque sale antes de todo mientras hay actividad.
            for r in 0..10i64 {
                let t0 = 36_000 * (r + 1);
                for m in 0..10 {
                    w.add_event(&ev(&format!("r{r}-{m}"), t0 + 60 * m + 5, "WARNING", "app", "WARNING: ruido")).unwrap();
                }
                w.add_event(&ev(&format!("d{r}"), t0 + 460, "WARNING", "app", "WARNING: disco lento")).unwrap();
                w.add_event(&ev(&format!("a{r}"), t0 + 480, "FATAL", "db", "FATAL: shutting down")).unwrap();
            }
            w.commit().unwrap();
        }
        store.build_rollups(60_000).unwrap();
        store.set_meta("rollup_bucket_ms", "60000").unwrap();
        let o = PrecedeOpts { anchor: EventScope { level: Some("FATAL"), ..Default::default() }, ..Default::default() };
        let r = precede(store.conn(), &o).unwrap();
        assert_eq!(r.episodes_total, 10);
        let by = |t: &str| r.common.iter().find(|c| c.template.contains(t)).unwrap_or_else(|| panic!("{t}: {:?}", r.common));
        let (ruido, disco) = (by("ruido"), by("disco"));
        assert_eq!((ruido.episodes, disco.episodes), (10, 10));
        assert_eq!(ruido.active_episodes, Some(10), "{ruido:?}");
        assert!(ruido.lift.unwrap() < 1.5, "el ruido de la ráfaga no precede más que por azar: {ruido:?}");
        assert!(disco.lift.unwrap() > 3.0, "{disco:?}");
        assert_eq!(r.common[0].template, disco.template, "{:?}", r.common);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn join_separa_episodios_y_precursores_por_clave() {
        let (path, mut store) = temp_store("join");
        {
            let mut w = store.writer().unwrap();
            w.set_dims(vec!["sesion".into()]);
            let mut e = |id: &str, secs, level: &str, title: &str, ses: &str| {
                let mut x = ev(id, secs, level, "tool", title);
                x.attrs.insert("sesion".into(), ses.into());
                w.add_event(&x).unwrap();
            };
            // Dos sesiones fallan a la vez; solo la A tuvo antes un `permiso denegado`.
            e("pa", 100, "WARNING", "WARNING: permiso denegado", "A");
            e("fa", 110, "ERROR", "Exit code 1", "A");
            e("fb", 111, "ERROR", "Exit code 1", "B");
            w.commit().unwrap();
        }
        let base = PrecedeOpts { anchor: EventScope { grep: Some("Exit code"), ..Default::default() }, ..Default::default() };
        let r = precede(store.conn(), &base).unwrap();
        assert_eq!(r.episodes_total, 1, "sin clave, las dos caídas son un episodio");
        let r = precede(store.conn(), &PrecedeOpts { join: Some("sesion"), ..base.clone() }).unwrap();
        assert_eq!(r.episodes_total, 2, "con clave, uno por sesión");
        let pre: Vec<usize> = r.episodes.iter().map(|e| e.preceding.len()).collect();
        assert_eq!(pre, [1, 0], "el aviso de A no precede a B: {:?}", r.episodes);
        assert!(precede(store.conn(), &PrecedeOpts { join: Some("a b"), ..base }).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn join_un_ancla_con_varios_valores_cuenta_en_cada_uno_y_las_sin_clave_se_cuentan() {
        let (path, mut store) = temp_store("join-multi");
        {
            let mut w = store.writer().unwrap();
            let mut e = |id: &str, secs, level: &str, title: &str, trazas: &[&str]| {
                let mut x = ev(id, secs, level, "tool", title);
                x.links = trazas.iter().map(|t| chrono_core::Link { rel: "traza".into(), target: t.to_string() }).collect();
                w.add_event(&x).unwrap();
            };
            // Un fallo de las trazas A y B a la vez: cada una tuvo su aviso antes. Y
            // un fallo sin traza.
            e("pa", 100, "WARNING", "WARNING: cola llena", &["A"]);
            e("pb", 101, "WARNING", "WARNING: disco lento", &["B"]);
            e("f", 110, "ERROR", "Exit code 1", &["A", "B"]);
            e("g", 500, "ERROR", "Exit code 1", &[]);
            w.commit().unwrap();
        }
        let o = PrecedeOpts { anchor: EventScope { grep: Some("Exit code"), ..Default::default() }, join: Some("traza"), ..Default::default() };
        let r = precede(store.conn(), &o).unwrap();
        assert_eq!(r.episodes_total, 2, "uno por traza, no solo por la menor: {:?}", r.episodes);
        let pre: Vec<&str> = r.episodes.iter().map(|e| e.preceding[0].template.as_str()).collect();
        assert_eq!(pre, ["WARNING: cola llena", "WARNING: disco lento"], "{:?}", r.episodes);
        assert_eq!(r.anchors_without_key, 1);
        let _ = std::fs::remove_file(&path);
    }
}
