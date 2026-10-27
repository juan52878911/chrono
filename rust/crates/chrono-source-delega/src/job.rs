//! Una carpeta de trabajo de delega → eventos `delega.*`.
//!
//! Lee `meta`, `summary.json`, `result.json`, `state.json`, `rc`, `progress.log` y
//! `notices.jsonl` (o sus nombres legados en español). Nunca lee `brief.md` ni copia el
//! texto completo del resultado: solo su primera línea, redactada con `forbidden.txt`.

use crate::simhash::{epoch_to_iso8601, simhash, tokenize};
use crate::util::{self, channel_file, first_line, load_terms, num, read_json, read_meta, text, ts_ms};
use chrono_core::{Actor, Event, Touch};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Códigos de salida de `delega run` que significan que el trabajo no llegó a arrancar
/// (2 rechazado, 3 config, 4 cuenta, 5 sin capacidad, 8 presupuesto, 9 términos prohibidos).
pub const LAUNCH_RCS: &[i64] = &[2, 3, 4, 5, 8, 9];

/// Largo máximo del texto que entra en un título.
pub const TEXT_MAX: usize = 160;

/// Subtipos legados de avisos y alertas → nombres nuevos (`delega.metrics.SUBTYPE_MAP`).
const SUBTYPES: &[(&str, &str)] = &[
    ("atasco", "stall"),
    ("bucle", "loop"),
    ("errores", "errors"),
    ("muerto", "dead"),
    ("no_entregado", "undelivered"),
    ("sin_respuesta", "no_answer"),
    ("tope", "cap"),
    ("tope_cortado", "cap_cut"),
    ("hito", "milestone"),
    ("bloqueo", "blocker"),
    ("pregunta", "question"),
];

/// Tipos legados de `progress.log` → nuevos.
const TYPES: &[(&str, &str)] = &[
    ("HERRAMIENTA", "TOOL"),
    ("RESULTADO", "RESULT"),
    ("FIN", "END"),
    ("TEXTO", "TEXT"),
    ("MENSAJE", "MESSAGE"),
    ("INICIO", "START"),
    ("AVISO", "NOTICE"),
    ("ALERTA", "ALERT"),
];

pub fn subtype(s: &str) -> String {
    SUBTYPES.iter().find(|(old, _)| *old == s).map_or(s, |(_, new)| *new).to_string()
}

fn norm_type(t: &str) -> (String, String) {
    let (head, tail) = t.split_once(':').unwrap_or((t, ""));
    let head = TYPES.iter().find(|(old, _)| *old == head).map_or(head, |(_, new)| *new);
    (head.to_string(), subtype(tail))
}

/// Contexto común de la fuente.
pub struct Ctx<'a> {
    pub source_id: &'a str,
    /// Sal de los ids de `audit.jsonl` (ver `audit_salt` en `lib.rs`).
    pub audit_salt: &'a str,
    /// Hora local de delega = UTC + `offset_s`.
    pub offset_s: i64,
    /// Términos del fichero global (`forbidden_terms` en las opciones de la fuente).
    pub terms: &'a [String],
    /// Nombres de las carpetas de trabajo, ordenados (para resolver el id corto de audit).
    pub jobs: &'a [String],
}

impl Ctx<'_> {
    /// audit guarda el id corto (`20261026-122251-88099`); la carpeta lleva además una
    /// etiqueta (`20261026-122251-88099-chrono`). Devuelve el nombre de la carpeta.
    fn resolve_job(&self, short: &str) -> Option<String> {
        let i = self.jobs.partition_point(|d| d.as_str() < short);
        self.jobs[i..]
            .iter()
            .take_while(|d| d.starts_with(short))
            .find(|d| d.len() == short.len() || d.as_bytes()[short.len()] == b'-')
            .cloned()
    }
}

/// `YYYYMMDD-HHMMSS…` del id del trabajo → ms (hora local leída como UTC).
fn id_naive_ms(job: &str) -> Option<i64> {
    let b = job.as_bytes();
    if b.len() < 15 || b[8] != b'-' {
        return None;
    }
    // `get` y no `[..]`: un nombre con un carácter de varios bytes al principio hacía panic.
    let iso = format!("{}-{}-{}T{}:{}:{}", job.get(0..4)?, job.get(4..6)?, job.get(6..8)?, job.get(9..11)?, job.get(11..13)?, job.get(13..15)?);
    util::parse_ts(&iso).map(|(ms, _)| ms)
}

/// Offset local observado en un trabajo: el `start` del resumen (hora local sin zona)
/// frente al `start` del estado (epoch), redondeado a 15 min.
pub fn observed_offset(dir: &Path) -> Option<i64> {
    let summary = read_json(&channel_file(dir, "summary.json"));
    let state = read_json(&channel_file(dir, "state.json"));
    let naive = util::parse_ts(&text(&summary, &["start", "inicio"])?)?.0;
    let epoch_ms = util::epoch_s_to_ms(num(&state, &["start", "inicio"])?)?;
    let diff_s = (naive as f64 - epoch_ms as f64) / 1000.0;
    let o = ((diff_s / 900.0).round() as i64).saturating_mul(900);
    // Una diferencia de más de 18 h no es la zona: son datos inconsistentes.
    (o.abs() <= util::MAX_OFFSET_S).then_some(o)
}

fn fmt_num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

#[allow(clippy::too_many_arguments)]
fn event(ctx: &Ctx, id: String, kind: &str, at: i64, level: &str, title: String, actor: &Actor, job: &str, attrs: BTreeMap<String, String>) -> Event {
    Event {
        id,
        source_id: ctx.source_id.to_string(),
        kind: kind.to_string(),
        at: epoch_to_iso8601(at),
        at_epoch: at,
        actor: actor.clone(),
        simhash: simhash(&tokenize(&title)),
        title,
        body: String::new(),
        level: level.to_string(),
        attrs,
        touches: vec![Touch { entity: job.to_string(), entity_type: "job".into(), weight: 1, attrs: BTreeMap::new() }],
        links: Vec::new(),
        is_bulk: false,
    }
}

struct Progress {
    start: Option<i64>,
    last: Option<i64>,
    /// (línea, ms, tipo, subtipo)
    marks: Vec<(usize, i64, String, String)>,
}

fn read_progress(dir: &Path, offset_s: i64) -> Progress {
    let mut p = Progress { start: None, last: None, marks: Vec::new() };
    let Ok(t) = fs::read_to_string(channel_file(dir, "progress.log")) else { return p };
    for (i, line) in t.lines().enumerate() {
        let mut parts = line.splitn(3, '\t');
        let (Some(ts), Some(ty)) = (parts.next(), parts.next()) else { continue };
        let Some(at) = ts_ms(ts, offset_s) else { continue };
        let (head, sub) = norm_type(ty);
        p.last = Some(at);
        if head == "START" && p.start.is_none() {
            p.start = Some(at);
        }
        if head == "ALERT" || head == "MESSAGE" {
            p.marks.push((i + 1, at, head, sub));
        }
    }
    p
}

/// Los eventos de una carpeta de trabajo. `job` es el nombre de la carpeta.
pub fn job_events(dir: &Path, job: &str, ctx: &Ctx) -> Vec<Event> {
    let meta = read_meta(dir);
    let summary = read_json(&channel_file(dir, "summary.json"));
    let result = read_json(&channel_file(dir, "result.json"));
    let state = read_json(&channel_file(dir, "state.json"));
    let rc_file = fs::read_to_string(dir.join("rc")).ok().and_then(|s| s.trim().parse::<i64>().ok());
    let mut terms = load_terms(&dir.join("forbidden.txt"));
    terms.extend(ctx.terms.iter().cloned());
    terms.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()).then_with(|| a.cmp(b)));
    terms.dedup();

    let m = |k: &str| meta.get(k).filter(|v| !v.is_empty()).cloned();
    let owner = m("OWNER");
    let profile = m("PROFILE");
    let actor_name = owner.clone().or(profile.clone()).unwrap_or_else(|| "delega".into());
    let actor = Actor { key: format!("delega:{actor_name}"), name: actor_name };

    let mut common: BTreeMap<String, String> = BTreeMap::new();
    common.insert("job".into(), job.into());
    for (attr, key) in [
        ("profile", "PROFILE"),
        ("node", "NODE"),
        ("owner", "OWNER"),
        ("repo", "REPO"),
        ("branch", "BRANCH"),
        ("project", "PROJECT"),
        ("account", "ACCOUNT"),
        ("base", "BASE"),
    ] {
        if let Some(v) = m(key) {
            common.insert(attr.into(), v);
        }
    }
    if let Some(model) = text(&summary, &["model", "modelo"]).or_else(|| text(&state, &["model", "modelo"])) {
        common.insert("model".into(), model);
    }

    let off = ctx.offset_s;
    let progress = read_progress(dir, off);
    let start = progress
        .start
        .or_else(|| text(&summary, &["start", "inicio"]).and_then(|s| ts_ms(&s, off)))
        .or_else(|| num(&state, &["start", "inicio"]).and_then(util::epoch_s_to_ms))
        .or_else(|| id_naive_ms(job).and_then(|ms| util::valid_ms(ms - off * 1000)))
        .unwrap_or(0);

    let mut out = Vec::new();
    let node = common.get("node").cloned().unwrap_or_default();
    let who = match (profile.as_deref(), node.as_str()) {
        (Some(p), "") => format!(" {p}"),
        (Some(p), n) => format!(" {p}@{n}"),
        (None, "") => String::new(),
        (None, n) => format!(" @{n}"),
    };
    out.push(event(ctx, format!("delega:{job}:start"), "delega.job.start", start, "INFO", format!("job.start {job}{who}"), &actor, job, common.clone()));

    // Alertas y mensajes del orquestador (sin texto: las alertas citan la última acción).
    for (line, at, head, sub) in &progress.marks {
        let mut a = common.clone();
        let (kind, level, title) = if head == "ALERT" {
            a.insert("alert".into(), sub.clone());
            let level = if matches!(sub.as_str(), "dead" | "cap_cut") { "ERROR" } else { "WARN" };
            ("delega.alert", level, format!("alert.{sub} {job}"))
        } else {
            ("delega.message", "INFO", format!("message {job}"))
        };
        out.push(event(ctx, format!("delega:{job}:p{line}"), kind, *at, level, title, &actor, job, a));
    }

    // Avisos del trabajador: primera línea, redactada.
    if let Ok(t) = fs::read_to_string(channel_file(dir, "notices.jsonl")) {
        for (i, line) in t.lines().enumerate() {
            let Ok(Value::Object(n)) = serde_json::from_str::<Value>(line) else { continue };
            let Some(at) = text(&n, &["ts"]).and_then(|s| ts_ms(&s, off)) else { continue };
            let sub = subtype(&text(&n, &["kind", "tipo"]).unwrap_or_default());
            let body = first_line(&text(&n, &["text", "texto"]).unwrap_or_default(), TEXT_MAX, &terms);
            let level = match sub.as_str() {
                "question" => "WARN",
                "blocker" => "ERROR",
                _ => "INFO",
            };
            let mut a = common.clone();
            a.insert("notice".into(), sub.clone());
            let title = if body.is_empty() { format!("notice.{sub} {job}") } else { format!("notice.{sub} {job}: {body}") };
            out.push(event(ctx, format!("delega:{job}:n{}", i + 1), "delega.notice", at, level, title, &actor, job, a));
        }
    }

    let results_last = fs::read_to_string(channel_file(dir, "results.jsonl"))
        .ok()
        .and_then(|t| t.lines().rev().find_map(|l| serde_json::from_str::<Value>(l).ok()))
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let has_output = !result.is_empty() || !results_last.is_empty();
    let terminal = !summary.is_empty() || has_output || rc_file.is_some() || m("END").is_some();
    if !terminal {
        return out;
    }

    let end = text(&summary, &["end", "fin"])
        .and_then(|s| ts_ms(&s, off))
        .or_else(|| m("END").and_then(|s| s.parse::<f64>().ok()).and_then(util::epoch_s_to_ms))
        .or_else(|| num(&state, &["last_event", "ultimo_evento"]).and_then(util::epoch_s_to_ms))
        .or(progress.last)
        .unwrap_or(start)
        .max(start);

    let mut a = common.clone();
    let cost = [
        ("summary", &summary, &["cost_usd", "coste_usd"][..]),
        ("result", &result, &["total_cost_usd"][..]),
        ("state", &state, &["cost_usd", "coste_usd"][..]),
    ]
    .into_iter()
    .find_map(|(from, obj, keys)| num(obj, keys).map(|c| (from, c)));
    if let Some((from, c)) = cost {
        a.insert("cost_usd".into(), fmt_num(c));
        a.insert("cost_from".into(), from.into());
    }
    let duration_min = num(&summary, &["duration_min", "duracion_min"]).unwrap_or((end.saturating_sub(start) as f64 / 6000.0).round() / 10.0);
    a.insert("duration_min".into(), fmt_num(duration_min));
    a.insert("duration_ms".into(), end.saturating_sub(start).to_string());
    for (attr, s_keys, st_keys) in [
        ("messages", &["messages", "mensajes"][..], &["turns", "turnos"][..]),
        ("tools", &["tools", "herramientas"][..], &["tools", "herramientas"][..]),
        ("alerts", &["alerts", "alertas"][..], &["alerts", "alertas"][..]),
    ] {
        if let Some(v) = num(&summary, s_keys).or_else(|| num(&state, st_keys)) {
            a.insert(attr.into(), fmt_num(v));
        }
    }
    let rc = rc_file.or_else(|| num(&summary, &["rc"]).map(|x| x as i64)).or_else(|| num(&state, &["rc"]).map(|x| x as i64));
    if let Some(rc) = rc {
        a.insert("rc".into(), rc.to_string());
    }
    let cap = text(&summary, &["cap", "tope"]).or_else(|| text(&state, &["cap", "tope"]));
    if let Some(cap) = &cap {
        a.insert("cap".into(), cap.clone());
    }
    a.insert("output".into(), if has_output { "yes" } else { "no" }.into());
    if let Some(sid) = text(&summary, &["session_id"]).or_else(|| text(&state, &["session_id"])).or_else(|| text(&result, &["session_id"])) {
        a.insert("session_id".into(), sid);
    }
    let infra = !has_output || rc.is_some_and(|r| LAUNCH_RCS.contains(&r));
    let level = if infra {
        "ERROR"
    } else if rc.is_some_and(|r| r != 0) || cap.is_some() {
        "WARN"
    } else {
        "INFO"
    };
    let mut title = format!("job.end {job} rc={}", rc.map_or("?".into(), |r| r.to_string()));
    if let Some((_, c)) = cost {
        title.push_str(&format!(" ${c:.2}"));
    }
    if cap.is_some() {
        title.push_str(" cap");
    }
    if !has_output {
        title.push_str(" no-output");
    }
    out.push(event(ctx, format!("delega:{job}:end"), "delega.job.end", end, level, title, &actor, job, a));

    let res_text = result.get("result").and_then(Value::as_str).or_else(|| results_last.get("result").and_then(Value::as_str));
    if let Some(line) = res_text.map(|t| first_line(t, TEXT_MAX, &terms)).filter(|l| !l.is_empty()) {
        out.push(event(ctx, format!("delega:{job}:result"), "delega.job.result", end, "INFO", format!("job.result {job}: {line}"), &actor, job, common));
    }
    out
}

/// Un registro de `audit.jsonl` → evento `delega.audit.<event>`. `off` (byte de inicio de la línea) da el id.
pub fn audit_event(rec: &Map<String, Value>, off: u64, ctx: &Ctx) -> Option<Event> {
    let kind_name = text(rec, &["event"])?;
    let at = text(rec, &["ts"]).and_then(|s| ts_ms(&s, ctx.offset_s))?;
    let mut attrs = BTreeMap::new();
    for (k, v) in rec {
        if matches!(k.as_str(), "ts" | "event" | "reason") {
            continue;
        }
        let s = match v {
            Value::String(s) if !s.is_empty() => s.clone(),
            Value::Number(n) => n.as_f64().map(fmt_num).unwrap_or_else(|| n.to_string()),
            Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        attrs.insert(k.clone(), s);
    }
    if let Some(dir) = attrs.get("job").and_then(|j| ctx.resolve_job(j)).filter(|d| Some(d) != attrs.get("job")) {
        let short = attrs.insert("job".into(), dir).unwrap_or_default();
        attrs.insert("job_id".into(), short);
    }
    let job = attrs.get("job").cloned();
    let rc = attrs.get("rc").and_then(|r| r.parse::<i64>().ok());
    let level = match kind_name.as_str() {
        "refused" => "ERROR",
        "end" if rc.is_some_and(|r| r != 0) => "WARN",
        "integrate" if matches!(attrs.get("result").map(String::as_str), Some("conflicts" | "verify_failed")) => "WARN",
        _ => "INFO",
    };
    let who = job.clone().or_else(|| attrs.get("node").cloned()).unwrap_or_default();
    let mut title = format!("audit.{kind_name} {who}").trim_end().to_string();
    if let Some(reason) = text(rec, &["reason"]) {
        title.push_str(&format!(": {}", first_line(&reason, TEXT_MAX, ctx.terms)));
    }
    let name = attrs.get("owner").or(attrs.get("profile")).cloned().unwrap_or_else(|| "delega".into());
    let actor = Actor { key: format!("delega:{name}"), name };
    let mut ev = event(ctx, format!("delega:audit:{}:{off}", ctx.audit_salt), &format!("delega.audit.{kind_name}"), at, level, title, &actor, "", attrs);
    ev.touches = job.map(|j| vec![Touch { entity: j, entity_type: "job".into(), weight: 1, attrs: BTreeMap::new() }]).unwrap_or_default();
    Some(ev)
}
