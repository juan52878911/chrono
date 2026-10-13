//! `session_patterns`: cruza el PROCESO (eventos Nod v1 de sesiones con
//! agentes, fuente `nod`) con el RESULTADO (commits del índice git), por tarea
//! y por sesión. "Nod mide el proceso y chrono el resultado."
//!
//! Definición de cada métrica y de cuándo no se puede calcular en
//! `docs/NOD.md`. En corto:
//! - tokens = Σ `turn.step.usage` (principal + subagentes; `turn.end.usage` y
//!   `agent.end.usage` son agregados y no se suman);
//! - costo = por (sesión, proceso) el último `usage.cost_usd` (acumulado del
//!   proceso). Se reparte por tarea como incrementos entre lecturas;
//! - tiempo del agente = Σ `turn.end.duration_ms` del principal (si falta,
//!   `ts(turn.end) − ts(turn.start)`); tiempo esperando al humano = Σ
//!   `permission.decision.wait_ms` con `by` = user|remote;
//! - permisos negados (y por tool), interrupciones (`turn.end` abortado del
//!   principal), bucles (≥3 `tool.result` seguidos con `ok:false` de la misma
//!   tool en la misma sesión y agente; una negativa del humano corta la racha);
//! - commits entregados = shas distintos de `vcs.commit`, resueltos por
//!   prefijo contra los commits del índice (la misma resolución que usa
//!   `trace`); cuántos tocan un hotspot (la métrica `hotspots` restringida a
//!   ficheros);
//! - retrabajo = ficheros de esos commits que otro commit (no entregado por el
//!   mismo grupo) vuelve a cambiar en los N días siguientes, y sus líneas;
//! - cambios incompletos = ficheros que la sesión escribió (Write/Edit con
//!   resultado ok, privacidad `completo`) y que no están en ningún commit suyo;
//! - merge = rama local mergeada en la base (git en vivo, lo pasa el
//!   llamador) o PR del forge que cita la tarea;
//! - costo por commit = costo / commits entregados;
//! - producido = lo que dejaron las tools (`tool.result.links`: PRs, issues,
//!   artifacts y documentos).
//!
//! Tiempos en MILISEGUNDOS (`at_epoch`, como todo el dominio).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};

use crate::Result;

/// Parámetros de la consulta.
#[derive(Debug, Clone)]
pub struct SessionPatternsOpts {
    /// Solo eventos Nod con `at_epoch >= since_epoch` (ms; 0 = todo).
    pub since_epoch: i64,
    /// Hasta cuándo se observó el repo (ms; para saber si la ventana de
    /// retrabajo de un commit ya se cerró). El CLI pasa el `last_sync_at` de la
    /// fuente git.
    pub horizon_epoch: i64,
    /// Ventana de retrabajo en días (30).
    pub rework_days: i64,
    /// Tamaño del top de hotspots (de ficheros) que cuenta como "hotspot".
    pub hotspot_limit: usize,
    /// Máximo de tareas y de sesiones devueltas.
    pub limit: usize,
    /// Filtros opcionales.
    pub task: Option<String>,
    pub session: Option<String>,
    /// Rama local → ¿mergeada en la base? (git en vivo; vacío sin git).
    pub branch_merged: BTreeMap<String, bool>,
    /// Nombre de la rama base usada para `branch_merged` (informativo).
    pub base_branch: String,
}

impl Default for SessionPatternsOpts {
    fn default() -> Self {
        Self {
            since_epoch: 0,
            horizon_epoch: 0,
            rework_days: 30,
            hotspot_limit: 25,
            limit: 25,
            task: None,
            session: None,
            branch_merged: BTreeMap::new(),
            base_branch: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// Parte de los anteriores que viene de pasos de subagentes.
    pub subagents: i64,
}

impl Tokens {
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCount {
    pub tool: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopInfo {
    pub session: String,
    pub agent: String,
    pub tool: String,
    pub failures: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Commits {
    /// Shas tal como los dio `vcs.commit` (distintos, en orden de aparición).
    pub delivered: Vec<String>,
    /// Ids (sha completo) de los commits del índice a los que resuelven.
    pub resolved: Vec<String>,
    /// Shas que no casan con ningún commit del índice (o casan con varios).
    pub unresolved: Vec<String>,
    /// De los resueltos, cuántos tocan al menos un hotspot.
    pub touching_hotspots: i64,
    /// Hotspots tocados (orden alfabético).
    pub hotspots_touched: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rework {
    pub window_days: i64,
    pub files_delivered: i64,
    pub files_reworked: i64,
    pub lines_delivered: i64,
    pub lines_reworked: i64,
    pub file_ratio: f64,
    /// Commits cuya ventana aún no se cerró en el horizonte observado.
    pub open_windows: i64,
    /// Ficheros retrabajados (orden alfabético).
    pub reworked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incomplete {
    /// Ficheros del repo escritos con éxito por la sesión.
    pub edited_files: i64,
    /// De ellos, los que no están en ningún commit entregado.
    pub uncommitted: Vec<String>,
    /// Ficheros escritos fuera de cualquier raíz de repo conocida (no cuentan).
    pub outside_repo: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    /// "merged" | "not_merged" | "unknown".
    pub status: String,
    /// "branch" | "pr" | "" (unknown).
    pub via: String,
    pub branches: Vec<String>,
    /// (número, estado, mergeado) de PRs que citan la tarea.
    pub prs: Vec<(i64, String, bool)>,
    /// Por qué es "unknown" (vacío si no).
    pub reason: String,
}

/// Métricas de un grupo (una tarea, una sesión, o el total).
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Tarea (None = sin tarea) o id de sesión.
    pub key: Option<String>,
    /// Para una tarea: sus sesiones. Para una sesión: sus tareas.
    pub related: Vec<String>,
    /// Perfiles (actor de Nod) que participaron.
    pub profiles: Vec<String>,
    /// Agentes (`source.host`: `claude`, `opencode`…).
    pub hosts: Vec<String>,
    /// Sesión de Remote Control (`remote_session`, la última vista). Solo en
    /// grupos de sesión; `None` si no hubo o la privacidad la quitó.
    pub remote_session: Option<String>,
    /// Lo que produjeron sus tools (`tool.result.links`): URLs de PRs,
    /// issues y artifacts, y rutas de documentos. Orden alfabético.
    pub produced: Vec<String>,
    /// ms.
    pub first_epoch: i64,
    pub last_epoch: i64,
    pub tokens: Tokens,
    pub cost_usd: Option<f64>,
    pub agent_ms: i64,
    pub waiting_ms: i64,
    pub waiting_decisions: i64,
    pub waiting_upper_bounds: i64,
    pub permissions_asked: i64,
    pub denied: i64,
    pub denied_by_user: i64,
    pub denied_by_tool: Vec<ToolCount>,
    pub interruptions: i64,
    pub loops: Vec<LoopInfo>,
    pub commits: Commits,
    pub rework: Option<Rework>,
    pub incomplete: Option<Incomplete>,
    pub merge: Merge,
    pub cost_per_commit: Option<f64>,
    pub tokens_per_commit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub sessions: i64,
    pub tasks: i64,
    pub all: Group,
    /// Tareas mergeadas / tareas con estado conocido.
    pub merge_rate: Option<f64>,
    pub tasks_merged: i64,
    pub tasks_merge_known: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPatterns {
    pub tasks: Vec<Group>,
    pub tasks_total: usize,
    pub sessions: Vec<Group>,
    pub sessions_total: usize,
    pub summary: Summary,
    /// Hotspots (ficheros) usados como referencia.
    pub hotspots: Vec<String>,
    pub events_scanned: i64,
    /// Qué no se pudo calcular y por qué.
    pub notes: Vec<String>,
}

/// Un evento Nod ya leído del índice (solo lo que usa la consulta).
#[derive(Debug, Default, Clone)]
struct Ev {
    id: String,
    ty: String,
    at_epoch: i64,
    instance: String,
    run: String,
    session: String,
    agent: String,
    task: String,
    turn: String,
    profile: String,
    host: String,
    remote_session: String,
    privacy: String,
    tool: String,
    tool_use_id: String,
    ok: String,
    rejected: String,
    decision: String,
    by: String,
    wait_ms: i64,
    exact: String,
    outcome: String,
    duration_ms: Option<i64>,
    usage: [i64; 4],
    cost_usd: Option<f64>,
    sha: String,
    branch: String,
    repo_branch: String,
    repo_root: String,
    cwd: String,
}

const ATTR_KEYS: &[&str] = &[
    "instance",
    "run",
    "session",
    "agent",
    "task",
    "turn",
    "profile",
    "host",
    "remote_session",
    "privacy",
    "tool",
    "tool_use_id",
    "ok",
    "rejected",
    "decision",
    "by",
    "wait_ms",
    "exact",
    "outcome",
    "duration_ms",
    "usage.input",
    "usage.output",
    "usage.cache_read",
    "usage.cache_write",
    "cost_usd",
    "sha",
    "branch",
    "repo.branch",
    "repo.root",
    "cwd",
];

fn load_events(conn: &Connection, since_epoch: i64) -> Result<Vec<Ev>> {
    let cols: Vec<String> = ATTR_KEYS
        .iter()
        .map(|k| format!("COALESCE(json_extract(attrs, '$.\"{k}\"'), '')"))
        .collect();
    let sql = format!(
        "SELECT id, kind, at_epoch, {} FROM events
         WHERE kind LIKE 'nod.%' AND at_epoch >= ?1",
        cols.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![since_epoch], |r| {
        let mut vals: Vec<String> = Vec::with_capacity(ATTR_KEYS.len());
        for i in 0..ATTR_KEYS.len() {
            let v: rusqlite::types::Value = r.get(3 + i)?;
            vals.push(match v {
                rusqlite::types::Value::Text(s) => s,
                rusqlite::types::Value::Integer(n) => n.to_string(),
                rusqlite::types::Value::Real(f) => f.to_string(),
                _ => String::new(),
            });
        }
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, vals))
    })?;
    let int = |s: &str| s.parse::<f64>().map(|f| f as i64).unwrap_or(0);
    let mut out = Vec::new();
    for row in rows {
        let (id, kind, at_epoch, v) = row?;
        let g = |k: &str| v[ATTR_KEYS.iter().position(|x| *x == k).unwrap()].clone();
        out.push(Ev {
            id,
            ty: kind.trim_start_matches("nod.").to_string(),
            at_epoch,
            instance: g("instance"),
            run: g("run"),
            session: g("session"),
            agent: g("agent"),
            task: g("task"),
            turn: g("turn"),
            profile: g("profile"),
            host: g("host"),
            remote_session: g("remote_session"),
            privacy: g("privacy"),
            tool: g("tool"),
            tool_use_id: g("tool_use_id"),
            ok: g("ok"),
            rejected: g("rejected"),
            decision: g("decision"),
            by: g("by"),
            wait_ms: int(&g("wait_ms")),
            exact: g("exact"),
            outcome: g("outcome"),
            duration_ms: g("duration_ms").parse::<f64>().ok().map(|f| f as i64),
            usage: [int(&g("usage.input")), int(&g("usage.output")), int(&g("usage.cache_read")), int(&g("usage.cache_write"))],
            cost_usd: g("cost_usd").parse::<f64>().ok(),
            sha: g("sha"),
            branch: g("branch"),
            repo_branch: g("repo.branch"),
            repo_root: g("repo.root"),
            cwd: g("cwd"),
        });
    }
    // Orden del contrato: por instante y, a igualdad, por (instance, run, seq).
    out.sort_by(|a, b| {
        (a.at_epoch, &a.instance, &a.run, seq_of(&a.id)).cmp(&(b.at_epoch, &b.instance, &b.run, seq_of(&b.id)))
    });
    Ok(out)
}

fn seq_of(id: &str) -> i64 {
    id.rsplit(':').next().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Rutas de ficheros editados por cada `tool.call` (touch `edited-file`).
fn load_edits(conn: &Connection, since_epoch: i64) -> Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT t.event_id, COALESCE(json_extract(t.attrs, '$.path'), '')
         FROM touches t
         JOIN entities e ON e.id = t.entity_id
         JOIN events ev ON ev.id = t.event_id
         WHERE e.type = 'edited-file' AND ev.kind = 'nod.tool.call' AND ev.at_epoch >= ?1
         ORDER BY t.event_id, e.key",
    )?;
    let rows = stmt.query_map(params![since_epoch], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (id, path) = row?;
        if !path.is_empty() {
            out.entry(id).or_default().push(path);
        }
    }
    Ok(out)
}

/// Lo que produjo cada `tool.result` (`links` `url`/`file` de la fuente nod).
fn load_produced(conn: &Connection, since_epoch: i64) -> Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT l.event_id, l.target FROM links l
         JOIN events ev ON ev.id = l.event_id
         WHERE ev.kind = 'nod.tool.result' AND l.rel IN ('url', 'file') AND ev.at_epoch >= ?1
         ORDER BY l.event_id, l.target",
    )?;
    let rows = stmt.query_map(params![since_epoch], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (id, target) = row?;
        out.entry(id).or_default().push(target);
    }
    Ok(out)
}

/// Acumulador de un grupo.
#[derive(Default)]
struct Acc {
    related: BTreeSet<String>,
    profiles: BTreeSet<String>,
    hosts: BTreeSet<String>,
    remote_session: Option<String>,
    produced: BTreeSet<String>,
    first: i64,
    last: i64,
    tokens: Tokens,
    cost: f64,
    has_cost: bool,
    agent_ms: i64,
    waiting_ms: i64,
    waiting_decisions: i64,
    waiting_upper_bounds: i64,
    asked: i64,
    denied: i64,
    denied_by_user: i64,
    denied_tools: BTreeMap<String, i64>,
    interruptions: i64,
    streaks: BTreeMap<(String, String), (String, i64)>,
    loops: Vec<LoopInfo>,
    shas: Vec<String>,
    branches: BTreeSet<String>,
    sessions_seen: BTreeSet<String>,
    edited: BTreeSet<String>,
    has_complete_privacy: bool,
}

impl Acc {
    fn touch(&mut self, e: &Ev) {
        if self.first == 0 || e.at_epoch < self.first {
            self.first = e.at_epoch;
        }
        self.last = self.last.max(e.at_epoch);
        if !e.profile.is_empty() {
            self.profiles.insert(e.profile.clone());
        }
        if !e.host.is_empty() {
            self.hosts.insert(e.host.clone());
        }
        if !e.remote_session.is_empty() {
            self.remote_session = Some(e.remote_session.clone());
        }
        self.sessions_seen.insert(e.session.clone());
    }

    fn close_streak(&mut self, key: (String, String), s: (String, i64)) {
        if s.1 >= 3 {
            self.loops.push(LoopInfo { session: key.0, agent: key.1, tool: s.0, failures: s.1 });
        }
    }

    fn finish_streaks(&mut self) {
        let rest = std::mem::take(&mut self.streaks);
        for (k, s) in rest {
            self.close_streak(k, s);
        }
    }

    /// Aplica un evento al acumulador. `agent_ms` y `cost_delta` vienen
    /// precalculados (necesitan estado por turno / por proceso).
    fn add(&mut self, e: &Ev, agent_ms: i64, cost_delta: Option<f64>, edits: Option<&Vec<String>>, tool_ok: bool, produced: Option<&Vec<String>>) {
        self.touch(e);
        if e.privacy == "completo" {
            self.has_complete_privacy = true;
        }
        match e.ty.as_str() {
            "turn.step" => {
                let t = &mut self.tokens;
                t.input += e.usage[0];
                t.output += e.usage[1];
                t.cache_read += e.usage[2];
                t.cache_write += e.usage[3];
                if !e.agent.is_empty() {
                    t.subagents += e.usage.iter().sum::<i64>();
                }
            }
            "turn.end" if e.agent.is_empty() => {
                self.agent_ms += agent_ms;
                if e.outcome == "aborted" {
                    self.interruptions += 1;
                }
            }
            "permission.request" => self.asked += 1,
            "permission.decision" => {
                let human = e.by == "user" || e.by == "remote";
                if human {
                    self.waiting_ms += e.wait_ms;
                    self.waiting_decisions += 1;
                    if e.exact == "false" {
                        self.waiting_upper_bounds += 1;
                    }
                }
                if e.decision == "deny" {
                    self.denied += 1;
                    if human {
                        self.denied_by_user += 1;
                    }
                    let tool = if e.tool.is_empty() { "?".to_string() } else { e.tool.clone() };
                    *self.denied_tools.entry(tool).or_insert(0) += 1;
                }
            }
            "tool.result" => {
                self.produced.extend(produced.into_iter().flatten().cloned());
                let key = (e.session.clone(), e.agent.clone());
                let failed = e.ok == "false" && e.rejected != "true";
                let cur = self.streaks.remove(&key);
                match (failed, cur) {
                    (true, Some((t, n))) if t == e.tool => {
                        self.streaks.insert(key, (t, n + 1));
                    }
                    (true, prev) => {
                        if let Some(p) = prev {
                            self.close_streak(key.clone(), p);
                        }
                        self.streaks.insert(key, (e.tool.clone(), 1));
                    }
                    (false, prev) => {
                        if let Some(p) = prev {
                            self.close_streak(key, p);
                        }
                    }
                }
            }
            "tool.call" => {
                if let (Some(paths), true) = (edits, tool_ok) {
                    for p in paths {
                        self.edited.insert(p.clone());
                    }
                }
            }
            "usage" => {
                if let Some(d) = cost_delta {
                    self.cost += d;
                    self.has_cost = true;
                }
            }
            "vcs.commit" => {
                if !e.sha.is_empty() && !self.shas.contains(&e.sha) {
                    self.shas.push(e.sha.clone());
                }
                if !e.branch.is_empty() {
                    self.branches.insert(e.branch.clone());
                }
            }
            _ => {}
        }
    }
}

/// ¿Aporta este tipo de evento a alguna métrica? (Para no crear grupos de
/// tarea vacíos con eventos como `session.start`, que no llevan tarea.)
fn sacc_agent(by_session: &mut BTreeMap<String, Acc>, session: &str, ms: i64) {
    by_session.entry(session.to_string()).or_default().agent_ms += ms;
}

fn contributes(ty: &str) -> bool {
    matches!(
        ty,
        "turn.step" | "turn.end" | "permission.request" | "permission.decision" | "tool.result" | "tool.call" | "usage" | "vcs.commit"
    )
}

/// Datos de un commit del índice (cacheados).
struct CommitInfo {
    at_epoch: i64,
    files: Vec<(String, i64)>,
}

struct Ctx<'a> {
    conn: &'a Connection,
    opts: &'a SessionPatternsOpts,
    hotspots: BTreeSet<String>,
    git_roots: Vec<String>,
    session_roots: BTreeMap<String, Vec<String>>,
    resolved: HashMap<String, Option<String>>,
    commits: HashMap<String, CommitInfo>,
    has_git: bool,
    has_prs: bool,
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl Ctx<'_> {
    /// Resuelve un sha (completo o abreviado) contra los commits del índice.
    /// `None` si no casa o si el prefijo es ambiguo.
    fn resolve(&mut self, sha: &str) -> Result<Option<String>> {
        if let Some(r) = self.resolved.get(sha) {
            return Ok(r.clone());
        }
        let r = crate::trace::resolve_commit_prefix(self.conn, sha)?;
        self.resolved.insert(sha.to_string(), r.clone());
        Ok(r)
    }

    fn commit(&mut self, id: &str) -> Result<&CommitInfo> {
        if !self.commits.contains_key(id) {
            let at_epoch: i64 = self
                .conn
                .query_row("SELECT at_epoch FROM events WHERE id = ?1", params![id], |r| r.get(0))
                .optional()?
                .unwrap_or(0);
            let mut stmt = self.conn.prepare_cached(
                "SELECT e.key, MAX(t.added, 0) + MAX(t.deleted, 0) FROM touches t JOIN entities e ON e.id = t.entity_id
                 WHERE t.event_id = ?1 AND e.type = 'file' ORDER BY e.key",
            )?;
            let files: Vec<(String, i64)> = stmt
                .query_map(params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            self.commits.insert(id.to_string(), CommitInfo { at_epoch, files });
        }
        Ok(&self.commits[id])
    }

    /// Ruta relativa al repo de un fichero editado, o `None` si está fuera de
    /// toda raíz conocida (raíces de las fuentes git y `repo.root`/`cwd` de las
    /// sesiones). Gana la raíz más larga.
    fn relativize(&self, path: &str, sessions: &BTreeSet<String>) -> Option<String> {
        let mut roots: Vec<&String> = self.git_roots.iter().collect();
        for s in sessions {
            if let Some(rs) = self.session_roots.get(s) {
                roots.extend(rs.iter());
            }
        }
        roots
            .into_iter()
            .filter(|r| !r.is_empty())
            .filter_map(|r| {
                let r = r.trim_end_matches('/');
                path.strip_prefix(r).and_then(|rest| rest.strip_prefix('/')).map(|rel| (r.len(), rel.to_string()))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, rel)| rel)
    }

    fn prs_for(&self, task: &str) -> Result<Vec<(i64, String, bool)>> {
        if task.len() < 3 || is_hashed(task) {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT number, state, merged FROM issues
             WHERE kind = 'pr' AND (title LIKE '%' || ?1 || '%' OR labels LIKE '%' || ?1 || '%')
             ORDER BY number",
        )?;
        let rows = stmt.query_map(params![task], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn finish(&mut self, key: Option<String>, mut acc: Acc, tasks_for_prs: &[String], extra_branches: &BTreeSet<String>) -> Result<Group> {
        acc.finish_streaks();
        acc.loops.sort_by(|a, b| (b.failures, &a.session, &a.tool).cmp(&(a.failures, &b.session, &b.tool)));

        // --- commits ---
        let mut commits = Commits { delivered: acc.shas.clone(), ..Default::default() };
        let mut delivered_ids: Vec<String> = Vec::new();
        for sha in &acc.shas {
            match self.resolve(sha)? {
                Some(id) if !delivered_ids.contains(&id) => delivered_ids.push(id),
                Some(_) => {}
                None => commits.unresolved.push(sha.clone()),
            }
        }
        let delivered_set: BTreeSet<String> = delivered_ids.iter().cloned().collect();
        let mut touched_hot: BTreeSet<String> = BTreeSet::new();
        let mut committed_files: BTreeSet<String> = BTreeSet::new();
        for id in &delivered_ids {
            let files: Vec<String> = self.commit(id)?.files.iter().map(|(f, _)| f.clone()).collect();
            let hot: Vec<&String> = files.iter().filter(|f| self.hotspots.contains(*f)).collect();
            if !hot.is_empty() {
                commits.touching_hotspots += 1;
                touched_hot.extend(hot.into_iter().cloned());
            }
            committed_files.extend(files);
        }
        commits.hotspots_touched = touched_hot.into_iter().collect();
        commits.resolved = delivered_ids.clone();

        // --- retrabajo ---
        let rework = if delivered_ids.is_empty() {
            None
        } else {
            let window = self.opts.rework_days * 86_400_000;
            let horizon = self.opts.horizon_epoch;
            let mut files_delivered: BTreeSet<String> = BTreeSet::new();
            let mut lines_delivered = 0i64;
            let mut reworked: BTreeSet<String> = BTreeSet::new();
            let mut seen_pairs: BTreeSet<(String, String)> = BTreeSet::new();
            let mut lines_reworked = 0i64;
            let mut open_windows = 0i64;
            for id in &delivered_ids {
                let (at, files) = {
                    let c = self.commit(id)?;
                    (c.at_epoch, c.files.clone())
                };
                if horizon > 0 && at + window > horizon {
                    open_windows += 1;
                }
                for (f, w) in &files {
                    files_delivered.insert(f.clone());
                    lines_delivered += w;
                    let mut stmt = self.conn.prepare_cached(
                        "SELECT ev.id, MAX(t.added, 0) + MAX(t.deleted, 0) FROM touches t
                         JOIN entities e ON e.id = t.entity_id
                         JOIN events ev ON ev.id = t.event_id
                         WHERE e.key = ?1 AND e.type = 'file' AND ev.kind = 'commit'
                           AND ev.at_epoch > ?2 AND ev.at_epoch <= ?3
                         ORDER BY ev.id",
                    )?;
                    let later: Vec<(String, i64)> = stmt
                        .query_map(params![f, at, at + window], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                        .collect::<std::result::Result<_, _>>()?;
                    for (lid, lw) in later {
                        if delivered_set.contains(&lid) {
                            continue;
                        }
                        reworked.insert(f.clone());
                        if seen_pairs.insert((lid, f.clone())) {
                            lines_reworked += lw;
                        }
                    }
                }
            }
            let fd = files_delivered.len() as i64;
            Some(Rework {
                window_days: self.opts.rework_days,
                files_delivered: fd,
                files_reworked: reworked.len() as i64,
                lines_delivered,
                lines_reworked,
                file_ratio: if fd > 0 { reworked.len() as f64 / fd as f64 } else { 0.0 },
                open_windows,
                reworked: reworked.into_iter().collect(),
            })
        };

        // --- cambios incompletos ---
        let incomplete = if acc.has_complete_privacy {
            let mut in_repo: BTreeSet<String> = BTreeSet::new();
            let mut outside = 0i64;
            for p in &acc.edited {
                match self.relativize(p, &acc.sessions_seen) {
                    Some(rel) => {
                        in_repo.insert(rel);
                    }
                    None => outside += 1,
                }
            }
            let uncommitted: Vec<String> = in_repo.iter().filter(|f| !committed_files.contains(*f)).cloned().collect();
            Some(Incomplete { edited_files: in_repo.len() as i64, uncommitted, outside_repo: outside })
        } else {
            None
        };

        // --- merge ---
        let mut branches = acc.branches.clone();
        branches.extend(extra_branches.iter().cloned());
        let mut prs: Vec<(i64, String, bool)> = Vec::new();
        for t in tasks_for_prs {
            for pr in self.prs_for(t)? {
                if !prs.contains(&pr) {
                    prs.push(pr);
                }
            }
        }
        let merge = self.merge_status(&branches, prs);

        let cost_usd = if acc.has_cost { Some(acc.cost) } else { None };
        let n_commits = commits.delivered.len() as i64;
        let cost_per_commit = match (cost_usd, n_commits) {
            (Some(c), n) if n > 0 => Some(c / n as f64),
            _ => None,
        };
        let tokens_per_commit = if n_commits > 0 { Some(acc.tokens.total() / n_commits) } else { None };
        let mut denied_by_tool: Vec<ToolCount> =
            acc.denied_tools.into_iter().map(|(tool, count)| ToolCount { tool, count }).collect();
        denied_by_tool.sort_by(|a, b| (b.count, &a.tool).cmp(&(a.count, &b.tool)));

        Ok(Group {
            key,
            related: acc.related.into_iter().collect(),
            profiles: acc.profiles.into_iter().collect(),
            hosts: acc.hosts.into_iter().collect(),
            remote_session: acc.remote_session,
            produced: acc.produced.into_iter().collect(),
            first_epoch: acc.first,
            last_epoch: acc.last,
            tokens: acc.tokens,
            cost_usd,
            agent_ms: acc.agent_ms,
            waiting_ms: acc.waiting_ms,
            waiting_decisions: acc.waiting_decisions,
            waiting_upper_bounds: acc.waiting_upper_bounds,
            permissions_asked: acc.asked,
            denied: acc.denied,
            denied_by_user: acc.denied_by_user,
            denied_by_tool,
            interruptions: acc.interruptions,
            loops: acc.loops,
            commits,
            rework,
            incomplete,
            merge,
            cost_per_commit,
            tokens_per_commit,
        })
    }

    fn merge_status(&self, branches: &BTreeSet<String>, prs: Vec<(i64, String, bool)>) -> Merge {
        let branches_v: Vec<String> = branches.iter().cloned().collect();
        let mk = |status: &str, via: &str, reason: &str, prs: Vec<(i64, String, bool)>| Merge {
            status: status.into(),
            via: via.into(),
            branches: branches_v.clone(),
            prs,
            reason: reason.into(),
        };
        if prs.iter().any(|p| p.2) {
            return mk("merged", "pr", "", prs);
        }
        // La base cuenta como mergeada consigo misma. Si el grupo tiene otra rama conocida (la sesión arrancó en
        // la base y luego trabajó en una rama), decide esa rama; la base solo cuenta cuando es la única.
        let base = &self.opts.base_branch;
        let other: Vec<bool> = branches
            .iter()
            .filter(|b| base.is_empty() || *b != base)
            .filter_map(|b| self.opts.branch_merged.get(b).copied())
            .collect();
        let known: Vec<bool> = if other.is_empty() {
            branches.iter().filter_map(|b| self.opts.branch_merged.get(b).copied()).collect()
        } else {
            other
        };
        if known.iter().any(|m| *m) {
            return mk("merged", "branch", "", prs);
        }
        if !known.is_empty() {
            return mk("not_merged", "branch", "", prs);
        }
        if !prs.is_empty() {
            return mk("not_merged", "pr", "", prs);
        }
        let reason = if branches.is_empty() {
            "no branch in the Nod events and no PR citing the task"
        } else if branches.iter().all(|b| is_hashed(b)) {
            "branch is hashed (privacy redactado)"
        } else if !self.has_git {
            "no git source in the index"
        } else if !self.has_prs {
            "branch is not a local branch of the repo and there is no PR data (forge)"
        } else {
            "branch is not a local branch of the repo and no PR cites the task"
        };
        mk("unknown", "", reason, prs)
    }
}

/// Identificador que Nod ocultó con hash (`h-xxxxxxxx`, privacidad redactado).
fn is_hashed(s: &str) -> bool {
    s.len() == 10 && s.starts_with("h-") && is_hex(&s[2..])
}

/// Calcula `session_patterns` sobre el índice.
pub fn session_patterns(conn: &Connection, opts: &SessionPatternsOpts) -> Result<SessionPatterns> {
    let mut events = load_events(conn, opts.since_epoch)?;
    if let Some(s) = &opts.session {
        events.retain(|e| &e.session == s);
    }
    let edits = load_edits(conn, opts.since_epoch)?;
    let produced = load_produced(conn, opts.since_epoch)?;

    let git_roots: Vec<String> = {
        let mut stmt = conn.prepare("SELECT COALESCE(path, '') FROM sources WHERE kind = 'git' ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let n_git_commits: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE kind = 'commit'", [], |r| r.get(0))?;
    let has_git = n_git_commits > 0;
    let has_prs: bool = conn.query_row("SELECT COUNT(*) FROM issues WHERE kind = 'pr'", [], |r| r.get::<_, i64>(0))? > 0;
    let hot: Vec<String> =
        crate::hotspots_of_type(conn, 0, opts.hotspot_limit, "file")?.into_iter().map(|h| h.entity).collect();

    // Pre-pasos: resultado de cada tool (para editar = escribió de verdad),
    // inicio de turnos (respaldo de duration_ms) y raíces/ramas por sesión.
    let mut tool_ok: HashMap<(String, String), bool> = HashMap::new();
    let mut turn_start: HashMap<(String, String, String), i64> = HashMap::new();
    let mut turn_closed: HashSet<(String, String, String)> = HashSet::new();
    let mut session_roots: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut session_branches: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for e in &events {
        match e.ty.as_str() {
            "tool.result" => {
                tool_ok.insert((e.session.clone(), e.tool_use_id.clone()), e.ok == "true" && e.rejected != "true");
            }
            "turn.start" => {
                turn_start.insert((e.instance.clone(), e.session.clone(), e.turn.clone()), e.at_epoch);
            }
            "turn.end" => {
                turn_closed.insert((e.instance.clone(), e.session.clone(), e.turn.clone()));
            }
            "session.start" => {
                let roots = session_roots.entry(e.session.clone()).or_default();
                for r in [&e.repo_root, &e.cwd] {
                    if !r.is_empty() && !roots.contains(r) {
                        roots.push(r.clone());
                    }
                }
                if !e.repo_branch.is_empty() {
                    session_branches.entry(e.session.clone()).or_default().insert(e.repo_branch.clone());
                }
            }
            _ => {}
        }
    }

    // Turnos todavía abiertos (turn.start sin turn.end): una sesión autónoma que se analiza a sí
    // misma desde dentro de su único turno —la reflexión de un `claude -p`— daba 0 s de agente.
    // Se cuenta hasta su último evento, y ese tiempo va a la tarea de ese evento.
    let mut open_last: HashMap<usize, i64> = HashMap::new();
    {
        let mut last_idx: HashMap<(String, String, String), usize> = HashMap::new();
        for (i, e) in events.iter().enumerate() {
            let k = (e.instance.clone(), e.session.clone(), e.turn.clone());
            if turn_start.contains_key(&k) && !turn_closed.contains(&k) {
                last_idx.insert(k, i);
            }
        }
        for (k, i) in last_idx {
            open_last.insert(i, turn_start[&k]);
        }
    }

    let mut all = Acc::default();
    let mut by_session: BTreeMap<String, Acc> = BTreeMap::new();
    let mut by_task: BTreeMap<Option<String>, Acc> = BTreeMap::new();
    let mut last_cost: HashMap<(String, String, String), f64> = HashMap::new();

    for (i, e) in events.iter().enumerate() {
        let task_key = if e.task.is_empty() { None } else { Some(e.task.clone()) };
        if let Some(t) = &opts.task {
            if task_key.as_deref() != Some(t.as_str()) {
                continue;
            }
        }
        let agent_ms = if e.ty == "turn.end" {
            match e.duration_ms {
                Some(d) => d,
                None => turn_start
                    .get(&(e.instance.clone(), e.session.clone(), e.turn.clone()))
                    .map(|s| (e.at_epoch - s).max(0))
                    .unwrap_or(0),
            }
        } else {
            0
        };
        let cost_delta = match (e.ty.as_str(), e.cost_usd) {
            ("usage", Some(c)) => {
                let k = (e.session.clone(), e.instance.clone(), e.run.clone());
                let prev = last_cost.insert(k, c).unwrap_or(0.0);
                // Acumulado del proceso: si baja, el proceso volvió a empezar.
                Some(if c >= prev { c - prev } else { c })
            }
            _ => None,
        };
        let ed = edits.get(&e.id);
        let ok = tool_ok.get(&(e.session.clone(), e.tool_use_id.clone())).copied().unwrap_or(false);
        let pr = produced.get(&e.id);

        all.add(e, agent_ms, cost_delta, ed, ok, pr);
        let sacc = by_session.entry(e.session.clone()).or_default();
        sacc.add(e, agent_ms, cost_delta, ed, ok, pr);
        if let Some(t) = &task_key {
            sacc.related.insert(t.clone());
        }
        if contributes(&e.ty) {
            let tacc = by_task.entry(task_key.clone()).or_default();
            tacc.add(e, agent_ms, cost_delta, ed, ok, pr);
            tacc.related.insert(e.session.clone());
        }
        if let Some(start) = open_last.get(&i) {
            let open_ms = (e.at_epoch - start).max(0);
            all.agent_ms += open_ms;
            sacc_agent(&mut by_session, &e.session, open_ms);
            by_task.entry(task_key.clone()).or_default().agent_ms += open_ms;
        }
    }

    let mut ctx = Ctx {
        conn,
        opts,
        hotspots: hot.iter().cloned().collect(),
        git_roots,
        session_roots,
        resolved: HashMap::new(),
        commits: HashMap::new(),
        has_git,
        has_prs,
    };

    let mut sessions = Vec::new();
    for (sid, acc) in by_session {
        let tasks: Vec<String> = acc.related.iter().cloned().collect();
        let extra = session_branches.get(&sid).cloned().unwrap_or_default();
        sessions.push(ctx.finish(Some(sid), acc, &tasks, &extra)?);
    }
    let mut tasks = Vec::new();
    let (mut tasks_merged, mut tasks_known) = (0i64, 0i64);
    for (tkey, acc) in by_task {
        let mut extra = BTreeSet::new();
        for s in &acc.related {
            if let Some(b) = session_branches.get(s) {
                extra.extend(b.iter().cloned());
            }
        }
        let prs_tasks: Vec<String> = tkey.iter().cloned().collect();
        let g = ctx.finish(tkey, acc, &prs_tasks, &extra)?;
        if g.key.is_some() && g.merge.status != "unknown" {
            tasks_known += 1;
            if g.merge.status == "merged" {
                tasks_merged += 1;
            }
        }
        tasks.push(g);
    }
    let n_sessions = sessions.len() as i64;
    let n_tasks = tasks.iter().filter(|t| t.key.is_some()).count() as i64;
    let all_tasks: Vec<String> = tasks.iter().filter_map(|t| t.key.clone()).collect();
    let all_branches: BTreeSet<String> = session_branches.values().flatten().cloned().collect();
    let all_g = ctx.finish(None, all, &all_tasks, &all_branches)?;

    let rank = |a: &Group, b: &Group| {
        let ca = a.cost_usd.unwrap_or(-1.0);
        let cb = b.cost_usd.unwrap_or(-1.0);
        cb.partial_cmp(&ca)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.tokens.total().cmp(&a.tokens.total()))
            .then(a.key.cmp(&b.key))
    };
    sessions.sort_by(rank);
    tasks.sort_by(rank);
    let (sessions_total, tasks_total) = (sessions.len(), tasks.len());
    sessions.truncate(opts.limit);
    tasks.truncate(opts.limit);

    let mut notes = Vec::new();
    if events.is_empty() {
        notes.push("no Nod events in the index (add them with: chrono add <events.jsonl> --as nod)".to_string());
    }
    if !has_git {
        notes.push("no git commits in the index: commits can't be resolved, so hotspots, rework and incomplete changes are unavailable".to_string());
    }
    if !has_prs {
        notes.push("no PR data (forge): merge status comes only from local branches".to_string());
    }
    if opts.branch_merged.is_empty() {
        notes.push("no local branch data (git not reachable): merge status comes only from PRs".to_string());
    }
    if !events.iter().any(|e| e.privacy == "completo") && !events.is_empty() {
        notes.push("no events with privacy 'completo': edited files (incomplete changes) are unavailable".to_string());
    }

    Ok(SessionPatterns {
        tasks,
        tasks_total,
        sessions,
        sessions_total,
        summary: Summary {
            sessions: n_sessions,
            tasks: n_tasks,
            all: all_g,
            merge_rate: if tasks_known > 0 { Some(tasks_merged as f64 / tasks_known as f64) } else { None },
            tasks_merged,
            tasks_merge_known: tasks_known,
        },
        hotspots: hot,
        events_scanned: events.len() as i64,
        notes,
    })
}
