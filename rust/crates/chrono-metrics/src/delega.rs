//! `delega`: las métricas de la orquestación con cifras exactas, desde la fuente
//! `delega` (carpetas de trabajo y `audit.jsonl`) y, si están en el mismo índice,
//! los eventos de Nod (sesiones del orquestador y decisiones que emite delega).
//!
//! - **Gasto** por día, perfil, nodo, dueño y modelo. El coste de un trabajo es el
//!   mismo que usa `delega spend` (resumen → resultado → estado) y el día es el del id
//!   del trabajo (hora local de delega), así el total cuadra con `delega spend all`.
//!   Se compara trabajo a trabajo con los `end` de `audit.jsonl`.
//! - **Fallos de infraestructura**: trabajos rechazados (`refused` en audit), con un
//!   `rc` de arranque de `delega run` (2, 3, 4, 5, 8, 9), terminados sin salida, o que
//!   nunca cerraron y llevan más de `stale_ms` sin actividad. Los que siguen vivos
//!   (actividad reciente respecto al último evento de delega del índice) no cuentan.
//! - **Latencia de decisión**: aviso `question` (o `blocker`) → siguiente `MESSAGE`
//!   del orquestador en el progreso del mismo trabajo; y `decision.request` de Nod →
//!   primera respuesta del mismo trabajo (o del mismo `id`).
//! - **Turnos del orquestador**: en las sesiones de Nod del orquestador, los turnos del
//!   agente principal sin ninguna llamada a herramienta («sin novedades»), y cuántos
//!   de ellos los despertó una notificación (Monitor, tareas en segundo plano).
//! - **Shas medidos por rama** (los PR salen de una rama): `integrate` de audit.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::Connection;

use crate::Result;

/// Códigos de salida de `delega run` previos a arrancar el trabajador.
pub const LAUNCH_RCS: &[i64] = &[2, 3, 4, 5, 8, 9];

/// Orígenes de `prompt.submit` que no son el humano escribiendo (`composer`, `bridge`).
const AUTO_ORIGINS: &[&str] = &["task-notification", "sdk", "peer", "scheduled-trigger"];

/// Pregunta respondida en más de esto: cuenta en `over_30min` (el umbral del plan).
const SLOW_MS: i64 = 30 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct DelegaOpts {
    pub since_epoch: i64,
    pub until_epoch: i64,
    /// Sesiones (o trozos de `cwd`) del orquestador, además de las deducidas.
    pub orchestrators: Vec<String>,
    /// Sin actividad durante esto (respecto al último evento de delega) y sin cierre: caído.
    pub stale_ms: i64,
    /// Elementos por lista.
    pub limit: usize,
}

impl Default for DelegaOpts {
    fn default() -> Self {
        Self { since_epoch: 0, until_epoch: 0, orchestrators: Vec::new(), stale_ms: 6 * 3600 * 1000, limit: 10 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpendRow {
    pub key: String,
    pub jobs: usize,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditCheck {
    /// Trabajos con `end` en audit.
    pub jobs_with_audit_end: usize,
    /// … cuyo coste coincide (±0,005 $) con el de la carpeta.
    pub matched: usize,
    /// (trabajo, coste de la carpeta, coste de audit)
    pub mismatched: Vec<(String, Option<f64>, f64)>,
    pub audit_cost_usd: f64,
    /// Coste de las carpetas de esos mismos trabajos.
    pub dir_cost_usd: f64,
    /// `end` de audit sin carpeta de trabajo indexada.
    pub audit_without_dir: Vec<String>,
    /// Trabajos cerrados después del primer evento de audit que no tienen `end` en audit.
    pub dir_without_audit: Vec<String>,
    pub first_audit_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Infra {
    /// Trabajos que cuentan (todos menos los que siguen vivos).
    pub considered: usize,
    pub failed: usize,
    pub running: usize,
    pub rate: Option<f64>,
    pub by_reason: BTreeMap<String, usize>,
    /// (trabajo, motivo)
    pub jobs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Latency {
    pub asked: usize,
    pub answered: usize,
    /// El trabajo terminó (o el índice acaba) sin respuesta.
    pub unanswered: usize,
    pub median_ms: Option<i64>,
    pub p90_ms: Option<i64>,
    pub max_ms: Option<i64>,
    pub over_30min: usize,
    /// (trabajo, ms) de las más lentas.
    pub slowest: Vec<(String, i64)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Turns {
    /// Sesiones de Nod tomadas como orquestador y por qué (`owner-cwd`, `command`, `option`).
    pub sessions: Vec<(String, String)>,
    pub turns: usize,
    pub empty: usize,
    pub empty_rate: Option<f64>,
    /// Turnos que no abrió el humano: `prompt.submit` con origen automático
    /// (`task-notification`, `sdk`, `peer`, `scheduled-trigger`: avisos de tareas, de
    /// sesiones enlazadas y disparadores), y cuántos de ellos quedaron vacíos.
    pub wakeups: usize,
    pub empty_wakeups: usize,
    /// (origen del prompt, turnos, vacíos); `""` = sin `prompt.submit` antes del turno.
    pub by_origin: Vec<(String, usize, usize)>,
    /// (sesión, turnos, vacíos)
    pub by_session: Vec<(String, usize, usize)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShaRow {
    pub branch: String,
    pub shas: usize,
    pub verified: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DelegaReport {
    pub jobs: usize,
    pub jobs_ended: usize,
    pub spend_total_usd: f64,
    pub jobs_without_cost: usize,
    pub spend_by_day: Vec<SpendRow>,
    pub spend_by_profile: Vec<SpendRow>,
    pub spend_by_node: Vec<SpendRow>,
    pub spend_by_owner: Vec<SpendRow>,
    pub spend_by_model: Vec<SpendRow>,
    pub audit: AuditCheck,
    pub infra: Infra,
    pub latency_progress: Latency,
    pub latency_blockers: Latency,
    pub latency_nod: Latency,
    pub turns: Turns,
    pub shas_per_branch: Vec<ShaRow>,
    pub notes: Vec<String>,
    /// Último evento de delega del índice (el «ahora» de `running`).
    pub horizon_epoch: i64,
}

#[derive(Debug, Default, Clone)]
struct Job {
    start: i64,
    profile: String,
    node: String,
    owner: String,
    model: String,
    ended: bool,
    cost: Option<f64>,
    rc: Option<i64>,
    output: Option<bool>,
    last: i64,
    refused: bool,
}

fn day_of(job: &str, start: i64) -> String {
    let b = job.as_bytes();
    if b.len() >= 8 && b[..8].iter().all(u8::is_ascii_digit) {
        format!("{}-{}-{}", &job[0..4], &job[4..6], &job[6..8])
    } else {
        crate::explain::utc_date(start.div_euclid(1000))
    }
}

fn in_window(at: i64, o: &DelegaOpts) -> bool {
    at >= o.since_epoch && (o.until_epoch <= 0 || at < o.until_epoch)
}

fn group(jobs: &[(&String, &Job)], key: impl Fn(&String, &Job) -> String, by_key: bool) -> Vec<SpendRow> {
    let mut m: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for (id, j) in jobs {
        let k = key(id, j);
        let e = m.entry(if k.is_empty() { "?".into() } else { k }).or_default();
        e.0 += 1;
        e.1 += j.cost.unwrap_or(0.0);
    }
    let mut v: Vec<SpendRow> = m.into_iter().map(|(key, (jobs, cost_usd))| SpendRow { key, jobs, cost_usd }).collect();
    if !by_key {
        v.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd).then_with(|| a.key.cmp(&b.key)));
    }
    v
}

fn latency(mut samples: Vec<(String, i64)>, asked: usize, limit: usize) -> Latency {
    let answered = samples.len();
    let mut ms: Vec<i64> = samples.iter().map(|s| s.1).collect();
    ms.sort_unstable();
    let pct = |p: f64| -> Option<i64> {
        if ms.is_empty() {
            None
        } else {
            let i = ((p * ms.len() as f64).ceil() as usize).clamp(1, ms.len()) - 1;
            Some(ms[i])
        }
    };
    samples.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    samples.truncate(limit);
    Latency {
        asked,
        answered,
        unanswered: asked - answered,
        median_ms: pct(0.5),
        p90_ms: pct(0.9),
        max_ms: ms.last().copied(),
        over_30min: ms.iter().filter(|m| **m > SLOW_MS).count(),
        slowest: samples,
    }
}

type Row = (String, i64, Option<String>, Vec<Option<String>>);

/// `kind`, `at`, `job` y los atributos `fields` de los eventos cuyo `kind` cumple `cond`
/// (SQL sobre `e.kind`, escrito aquí: nunca viene del usuario).
fn load(conn: &Connection, cond: &str, fields: &[&str]) -> Result<Vec<Row>> {
    let cols: String = fields.iter().map(|f| format!(", CAST(json_extract(e.attrs, '$.\"{f}\"') AS TEXT)")).collect();
    let sql = format!(
        "SELECT e.kind, e.at_epoch, json_extract(e.attrs, '$.job'){cols} FROM events e
         WHERE {cond} ORDER BY e.at_epoch, e.rowid"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        let mut extra = Vec::with_capacity(fields.len());
        for i in 0..fields.len() {
            extra.push(r.get::<_, Option<String>>(3 + i)?);
        }
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, extra))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn s(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

pub fn delega_report(conn: &Connection, o: &DelegaOpts) -> Result<DelegaReport> {
    let mut rep = DelegaReport::default();

    // ── trabajos ────────────────────────────────────────────────────────────
    let rows = load(conn, "e.kind >= 'delega.' AND e.kind < 'delega/'", &["profile", "node", "owner", "model", "cost_usd", "rc", "output", "notice"])?;
    let mut jobs: BTreeMap<String, Job> = BTreeMap::new();
    let mut horizon = 0i64;
    for (kind, at, job, f) in &rows {
        horizon = horizon.max(*at);
        let Some(job) = job.clone().filter(|j| !j.is_empty()) else { continue };
        if kind.starts_with("delega.audit.") {
            if kind == "delega.audit.refused" {
                let j = jobs.entry(job).or_insert_with(|| Job { start: *at, ..Default::default() });
                j.refused = true;
                j.last = j.last.max(*at);
            }
            continue;
        }
        let j = jobs.entry(job).or_default();
        j.last = j.last.max(*at);
        match kind.as_str() {
            "delega.job.start" => {
                j.start = *at;
                j.profile = s(&f[0]);
                j.node = s(&f[1]);
                j.owner = s(&f[2]);
                if j.model.is_empty() {
                    j.model = s(&f[3]);
                }
            }
            "delega.job.end" => {
                j.ended = true;
                if !s(&f[3]).is_empty() {
                    j.model = s(&f[3]);
                }
                j.cost = f[4].as_deref().and_then(|c| c.parse().ok());
                j.rc = f[5].as_deref().and_then(|c| c.parse().ok());
                j.output = f[6].as_deref().map(|v| v == "yes");
            }
            _ => {}
        }
    }
    rep.horizon_epoch = horizon;
    let selected: Vec<(&String, &Job)> = jobs.iter().filter(|(_, j)| in_window(j.start, o)).collect();
    rep.jobs = selected.len();
    rep.jobs_ended = selected.iter().filter(|(_, j)| j.ended).count();
    rep.spend_total_usd = selected.iter().map(|(_, j)| j.cost.unwrap_or(0.0)).sum();
    rep.jobs_without_cost = selected.iter().filter(|(_, j)| j.cost.is_none()).count();
    rep.spend_by_day = group(&selected, |id, j| day_of(id, j.start), true);
    rep.spend_by_profile = group(&selected, |_, j| j.profile.clone(), false);
    rep.spend_by_node = group(&selected, |_, j| j.node.clone(), false);
    rep.spend_by_owner = group(&selected, |_, j| j.owner.clone(), false);
    rep.spend_by_model = group(&selected, |_, j| j.model.clone(), false);

    // ── audit frente a carpetas ─────────────────────────────────────────────
    let audit = load(conn, "e.kind >= 'delega.audit.' AND e.kind < 'delega.audit/'", &["cost_usd", "branch", "sha", "result"])?;
    let mut a = AuditCheck { first_audit_at: audit.first().map(|r| r.1), ..Default::default() };
    let mut audit_end: BTreeMap<String, f64> = BTreeMap::new();
    for (kind, _, job, f) in &audit {
        if kind == "delega.audit.end" {
            if let (Some(job), Some(c)) = (job, f[0].as_deref().and_then(|c| c.parse::<f64>().ok())) {
                audit_end.insert(job.clone(), c);
            }
        }
    }
    for (job, ac) in &audit_end {
        match jobs.get(job) {
            Some(j) if in_window(j.start, o) => {
                a.jobs_with_audit_end += 1;
                a.audit_cost_usd += ac;
                a.dir_cost_usd += j.cost.unwrap_or(0.0);
                if j.cost.is_some_and(|c| (c - ac).abs() <= 0.005) {
                    a.matched += 1;
                } else {
                    a.mismatched.push((job.clone(), j.cost, *ac));
                }
            }
            Some(_) => {}
            None => a.audit_without_dir.push(job.clone()),
        }
    }
    if let Some(first) = a.first_audit_at {
        a.dir_without_audit = selected
            .iter()
            .filter(|(id, j)| j.ended && j.start >= first && !audit_end.contains_key(*id))
            .map(|(id, _)| (*id).clone())
            .collect();
    }
    rep.audit = a;

    // ── fallos de infraestructura ───────────────────────────────────────────
    let mut infra = Infra::default();
    for (id, j) in &selected {
        let reason = if j.refused {
            Some("refused".to_string())
        } else if let Some(rc) = j.rc.filter(|rc| LAUNCH_RCS.contains(rc)) {
            Some(format!("rc={rc}"))
        } else if j.ended {
            (j.output == Some(false)).then(|| "no_output".to_string())
        } else if horizon.saturating_sub(j.last) > o.stale_ms {
            Some("never_ended".to_string())
        } else {
            infra.running += 1;
            continue;
        };
        infra.considered += 1;
        if let Some(r) = reason {
            infra.failed += 1;
            *infra.by_reason.entry(r.clone()).or_default() += 1;
            infra.jobs.push(((*id).clone(), r));
        }
    }
    infra.rate = (infra.considered > 0).then(|| infra.failed as f64 / infra.considered as f64);
    infra.jobs.truncate(o.limit);
    rep.infra = infra;

    // ── latencia pregunta → respuesta (progreso) ────────────────────────────
    let mut by_job: HashMap<&str, Vec<(i64, &str)>> = HashMap::new();
    for (kind, at, job, f) in &rows {
        let Some(job) = job.as_deref() else { continue };
        match kind.as_str() {
            "delega.notice" => by_job.entry(job).or_default().push((*at, f[7].as_deref().unwrap_or(""))),
            "delega.message" => by_job.entry(job).or_default().push((*at, "MESSAGE")),
            _ => {}
        }
    }
    for (kind_name, slot) in [("question", 0usize), ("blocker", 1)] {
        let (mut asked, mut samples) = (0usize, Vec::new());
        for (job, evs) in &by_job {
            if !jobs.get(*job).is_some_and(|j| in_window(j.start, o)) {
                continue;
            }
            let mut evs = evs.clone();
            evs.sort();
            for (i, (at, k)) in evs.iter().enumerate() {
                if *k != kind_name {
                    continue;
                }
                asked += 1;
                if let Some((ans, _)) = evs[i + 1..].iter().find(|(t, k)| *k == "MESSAGE" && t >= at) {
                    samples.push((job.to_string(), ans.saturating_sub(*at)));
                }
            }
        }
        let l = latency(samples, asked, o.limit);
        if slot == 0 {
            rep.latency_progress = l;
        } else {
            rep.latency_blockers = l;
        }
    }

    // ── latencia decision.request → respuesta (Nod) ─────────────────────────
    let nod = load(
        conn,
        "e.kind IN ('nod.decision.request', 'nod.decision.answer', 'nod.job.say', 'nod.job.message')",
        &["id"],
    )?;
    let (mut asked, mut samples) = (0usize, Vec::new());
    for (i, (kind, at, job, f)) in nod.iter().enumerate() {
        if kind != "nod.decision.request" || !in_window(*at, o) {
            continue;
        }
        asked += 1;
        let id = f[0].as_deref().filter(|x| !x.is_empty());
        let ans = nod[i + 1..].iter().find(|(k, t, j, g)| {
            k != "nod.decision.request" && t >= at && match (id, g[0].as_deref()) {
                (Some(a), Some(b)) => a == b,
                _ => job.is_some() && j == job,
            }
        });
        if let Some((_, t, _, _)) = ans {
            samples.push((job.clone().unwrap_or_default(), t.saturating_sub(*at)));
        }
    }
    rep.latency_nod = latency(samples, asked, o.limit);

    // ── turnos del orquestador ──────────────────────────────────────────────
    rep.turns = orchestrator_turns(conn, o, &jobs)?;

    // ── shas medidos por rama (integrate) ───────────────────────────────────
    let mut shas: BTreeMap<String, (BTreeSet<String>, usize, usize)> = BTreeMap::new();
    for (kind, at, _, f) in &audit {
        if kind != "delega.audit.integrate" || !in_window(*at, o) {
            continue;
        }
        let e = shas.entry(s(&f[1])).or_default();
        if let Some(sha) = f[2].clone().filter(|x| !x.is_empty()) {
            e.0.insert(sha);
        }
        match f[3].as_deref() {
            Some("verified") => e.1 += 1,
            Some("verify_failed" | "conflicts") => e.2 += 1,
            _ => {}
        }
    }
    let mut sv: Vec<ShaRow> =
        shas.into_iter().map(|(branch, (set, verified, failed))| ShaRow { branch, shas: set.len(), verified, failed }).collect();
    sv.sort_by(|a, b| b.shas.cmp(&a.shas).then_with(|| a.branch.cmp(&b.branch)));
    sv.truncate(o.limit);
    rep.shas_per_branch = sv;

    // ── notas ───────────────────────────────────────────────────────────────
    if rows.is_empty() {
        rep.notes.push("no delega events: add the state dir with `chrono add <state_dir> --as delega`".into());
    }
    if let Some(r) = rep.spend_by_profile.iter().find(|r| r.key == "?") {
        rep.notes.push(format!(
            "{} jobs have no PROFILE in meta (legacy scripts): `delega spend --by profile` counts them under the default profile",
            r.jobs
        ));
    }
    if rep.audit.first_audit_at.is_none() {
        rep.notes.push("no audit.jsonl events: spend is not cross-checked".into());
    }
    if rep.shas_per_branch.is_empty() {
        rep.notes.push("no `integrate` events in audit: shas per branch/PR cannot be inferred yet".into());
    }
    if rep.turns.sessions.is_empty() {
        rep.notes.push(
            "no orchestrator sessions found in Nod events (none whose cwd folder is a job owner; pass orchestrators=…)".into(),
        );
    }
    if rep.latency_nod.asked == 0 {
        rep.notes.push("no nod decision.request events: decision latency comes only from progress logs".into());
    }
    Ok(rep)
}

/// Sesiones de Nod del orquestador y sus turnos.
fn orchestrator_turns(conn: &Connection, o: &DelegaOpts, jobs: &BTreeMap<String, Job>) -> Result<Turns> {
    let owners: BTreeSet<&str> = jobs.values().map(|j| j.owner.as_str()).filter(|x| !x.is_empty()).collect();
    let mut sessions: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT json_extract(attrs, '$.session'), COALESCE(json_extract(attrs, '$.cwd'), '')
             FROM events WHERE kind = 'nod.session.start'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (Some(sid), cwd) = row? else { continue };
            let base = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            if owners.contains(base) {
                sessions.entry(sid.clone()).or_insert_with(|| "owner-cwd".into());
            }
            if o.orchestrators.iter().any(|x| !x.is_empty() && (*x == sid || cwd.contains(x.as_str()))) {
                sessions.insert(sid, "option".into());
            }
        }
    }
    for sid in &o.orchestrators {
        if sid.len() >= 8 && !sid.contains('/') {
            sessions.entry(sid.clone()).or_insert_with(|| "option".into());
        }
    }
    {
        // Con `include_command` en la fuente de Nod, el título lleva el comando.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT json_extract(attrs, '$.session') FROM events
             WHERE kind = 'nod.tool.call' AND json_extract(attrs, '$.agent') IS NULL
               AND (title LIKE '%delega %' OR title LIKE '%delegar.sh%' OR title LIKE '%ws exec claude%')",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
        for sid in rows.flatten().flatten() {
            sessions.entry(sid).or_insert_with(|| "command".into());
        }
    }
    let mut t = Turns { sessions: sessions.iter().map(|(a, b)| (a.clone(), b.clone())).collect(), ..Default::default() };
    if sessions.is_empty() {
        return Ok(t);
    }

    // Eventos de esas sesiones: prompt.submit (origen), turn.start, turn.end y tool.call del principal.
    let mut stmt = conn.prepare(
        "SELECT kind, at_epoch, json_extract(attrs, '$.session'), json_extract(attrs, '$.turn'),
                COALESCE(json_extract(attrs, '$.origin'), '')
         FROM events
         WHERE kind IN ('nod.prompt.submit', 'nod.turn.start', 'nod.turn.end', 'nod.tool.call')
           AND json_extract(attrs, '$.agent') IS NULL
         ORDER BY at_epoch, CAST(json_extract(attrs, '$.seq') AS INTEGER)",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, String>(4)?))
    })?;
    let mut last_origin: HashMap<String, String> = HashMap::new();
    let mut turn_origin: HashMap<String, String> = HashMap::new();
    let mut tools: HashMap<String, usize> = HashMap::new();
    let mut ends: Vec<(String, String, i64)> = Vec::new();
    for row in rows {
        let (kind, at, sid, turn, origin) = row?;
        let Some(sid) = sid.filter(|x| sessions.contains_key(x)) else { continue };
        match kind.as_str() {
            "nod.prompt.submit" => {
                last_origin.insert(sid, origin);
            }
            "nod.turn.start" => {
                if let Some(turn) = turn {
                    turn_origin.insert(turn, last_origin.remove(&sid).unwrap_or_default());
                }
            }
            "nod.tool.call" => {
                if let Some(turn) = turn {
                    *tools.entry(turn).or_default() += 1;
                }
            }
            "nod.turn.end" => {
                if let Some(turn) = turn {
                    if in_window(at, o) {
                        ends.push((sid, turn, at));
                    }
                }
            }
            _ => {}
        }
    }
    let mut per: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut origins: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (sid, turn, _) in &ends {
        let empty = tools.get(turn).copied().unwrap_or(0) == 0;
        let origin = turn_origin.get(turn).cloned().unwrap_or_default();
        let wake = AUTO_ORIGINS.contains(&origin.as_str());
        let oe = origins.entry(origin).or_default();
        oe.0 += 1;
        oe.1 += usize::from(empty);
        t.turns += 1;
        t.empty += usize::from(empty);
        t.wakeups += usize::from(wake);
        t.empty_wakeups += usize::from(empty && wake);
        let e = per.entry(sid.clone()).or_default();
        e.0 += 1;
        e.1 += usize::from(empty);
    }
    t.empty_rate = (t.turns > 0).then(|| t.empty as f64 / t.turns as f64);
    let mut bo: Vec<(String, usize, usize)> = origins.into_iter().map(|(o, (n, e))| (o, n, e)).collect();
    bo.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    t.by_origin = bo;
    let mut by: Vec<(String, usize, usize)> = per.into_iter().map(|(s, (n, e))| (s, n, e)).collect();
    by.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then_with(|| a.0.cmp(&b.0)));
    by.truncate(o.limit);
    t.by_session = by;
    Ok(t)
}
