//! Tool `delega`: las métricas de la orquestación (gasto cuadrado con audit, fallos de
//! infraestructura, latencia de decisiones, turnos vacíos del orquestador, shas por rama)
//! en texto corto o, con `format=json`, en JSON. El CLI (`chrono delega`) usa la misma.

use chrono_metrics::{DelegaOpts, DelegaReport, Latency, SpendRow};
use serde_json::{json, Value};

use crate::since::since_to_epoch;
use crate::Result;

fn spend_json(rows: &[SpendRow]) -> Value {
    json!(rows.iter().map(|r| json!({"key": r.key, "jobs": r.jobs, "cost_usd": round2(r.cost_usd)})).collect::<Vec<_>>())
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn lat_json(l: &Latency) -> Value {
    json!({
        "asked": l.asked, "answered": l.answered, "unanswered": l.unanswered,
        "median_ms": l.median_ms, "p90_ms": l.p90_ms, "max_ms": l.max_ms, "over_30min": l.over_30min,
        "slowest": l.slowest.iter().map(|(j, ms)| json!({"job": j, "ms": ms})).collect::<Vec<_>>(),
    })
}

pub fn to_json(r: &DelegaReport) -> Value {
    let a = &r.audit;
    json!({
        "jobs": r.jobs, "jobs_ended": r.jobs_ended,
        "spend": {
            "total_usd": round2(r.spend_total_usd), "jobs_without_cost": r.jobs_without_cost,
            "by_day": spend_json(&r.spend_by_day), "by_profile": spend_json(&r.spend_by_profile),
            "by_node": spend_json(&r.spend_by_node), "by_owner": spend_json(&r.spend_by_owner),
            "by_model": spend_json(&r.spend_by_model),
        },
        "audit_check": {
            "first_audit_ms": a.first_audit_at, "jobs_with_audit_end": a.jobs_with_audit_end, "matched": a.matched,
            "audit_cost_usd": round2(a.audit_cost_usd), "dir_cost_usd": round2(a.dir_cost_usd),
            "mismatched": a.mismatched.iter().map(|(j, d, au)| json!({"job": j, "dir_usd": d, "audit_usd": au})).collect::<Vec<_>>(),
            "audit_without_dir": a.audit_without_dir, "dir_without_audit": a.dir_without_audit,
        },
        "infra_failures": {
            "considered": r.infra.considered, "failed": r.infra.failed, "running": r.infra.running,
            "rate": r.infra.rate, "by_reason": r.infra.by_reason,
            "jobs": r.infra.jobs.iter().map(|(j, why)| json!({"job": j, "reason": why})).collect::<Vec<_>>(),
        },
        "decision_latency": {
            "question_to_message": lat_json(&r.latency_progress),
            "blocker_to_message": lat_json(&r.latency_blockers),
            "nod_decision_request": lat_json(&r.latency_nod),
        },
        "orchestrator_turns": {
            "sessions": r.turns.sessions.iter().map(|(s, why)| json!({"session": s, "matched_by": why})).collect::<Vec<_>>(),
            "turns": r.turns.turns, "empty": r.turns.empty, "empty_rate": r.turns.empty_rate,
            "wakeups": r.turns.wakeups, "empty_wakeups": r.turns.empty_wakeups,
            "by_origin": r.turns.by_origin.iter().map(|(o, n, e)| json!({"origin": o, "turns": n, "empty": e})).collect::<Vec<_>>(),
            "by_session": r.turns.by_session.iter().map(|(s, n, e)| json!({"session": s, "turns": n, "empty": e})).collect::<Vec<_>>(),
        },
        "shas_per_branch": r.shas_per_branch.iter().map(|s| json!({"branch": s.branch, "shas": s.shas, "verified": s.verified, "failed": s.failed})).collect::<Vec<_>>(),
        "horizon_ms": r.horizon_epoch,
        "notes": r.notes,
    })
}

fn pct(x: Option<f64>) -> String {
    x.map_or("—".into(), |v| format!("{:.1} %", v * 100.0))
}

fn mins(ms: Option<i64>) -> String {
    ms.map_or("—".into(), |m| format!("{:.1} min", m as f64 / 60_000.0))
}

fn spend_line(name: &str, rows: &[SpendRow], n: usize) -> String {
    let parts: Vec<String> = rows.iter().take(n).map(|r| format!("{} {:.2} $ ({})", r.key, r.cost_usd, r.jobs)).collect();
    let more = if rows.len() > n { format!(" · +{} más", rows.len() - n) } else { String::new() };
    format!("  por {name}: {}{more}", parts.join(" · "))
}

fn lat_line(name: &str, l: &Latency) -> String {
    if l.asked == 0 {
        return format!("  {name}: sin datos");
    }
    let slow: Vec<String> = l.slowest.iter().take(3).map(|(j, ms)| format!("{j} {}", mins(Some(*ms)))).collect();
    format!(
        "  {name}: {} preguntas, {} respondidas, {} sin respuesta · mediana {} · p90 {} · máx {} · > 30 min: {}{}",
        l.asked,
        l.answered,
        l.unanswered,
        mins(l.median_ms),
        mins(l.p90_ms),
        mins(l.max_ms),
        l.over_30min,
        if slow.is_empty() { String::new() } else { format!(" · más lentas: {}", slow.join(", ")) }
    )
}

pub fn to_text(r: &DelegaReport) -> String {
    let mut out = Vec::new();
    out.push(format!("delega · {} trabajos ({} cerrados) · último evento {}", r.jobs, r.jobs_ended, crate::contract::iso(r.horizon_epoch)));
    out.push(format!("Gasto: {:.2} $ ({} trabajos sin coste)", r.spend_total_usd, r.jobs_without_cost));
    out.push(spend_line("día", &r.spend_by_day, 31));
    out.push(spend_line("perfil", &r.spend_by_profile, 5));
    out.push(spend_line("nodo", &r.spend_by_node, 6));
    out.push(spend_line("dueño", &r.spend_by_owner, 6));
    out.push(spend_line("modelo", &r.spend_by_model, 5));
    let a = &r.audit;
    match a.first_audit_at {
        None => out.push("Cuadre con audit.jsonl: sin eventos de audit".into()),
        Some(first) => {
            out.push(format!(
                "Cuadre con audit.jsonl (desde {}): {} trabajos con end en audit, {} coinciden · audit {:.2} $ frente a carpetas {:.2} $",
                crate::contract::iso(first),
                a.jobs_with_audit_end,
                a.matched,
                a.audit_cost_usd,
                a.dir_cost_usd
            ));
            for (j, d, au) in a.mismatched.iter().take(5) {
                out.push(format!("  difiere {j}: carpeta {} · audit {au:.2} $", d.map_or("sin coste".into(), |d| format!("{d:.2} $"))));
            }
            if !a.dir_without_audit.is_empty() {
                out.push(format!("  cerrados después del primer audit sin end en audit: {} ({})", a.dir_without_audit.len(), a.dir_without_audit.iter().take(5).cloned().collect::<Vec<_>>().join(", ")));
            }
            if !a.audit_without_dir.is_empty() {
                out.push(format!("  end en audit sin carpeta: {}", a.audit_without_dir.join(", ")));
            }
        }
    }
    let i = &r.infra;
    let reasons: Vec<String> = i.by_reason.iter().map(|(k, v)| format!("{k} {v}")).collect();
    out.push(format!(
        "Fallos de infraestructura: {} de {} ({}) [objetivo < 3 %] · {} · en curso: {}",
        i.failed,
        i.considered,
        pct(i.rate),
        if reasons.is_empty() { "ninguno".into() } else { reasons.join(", ") },
        i.running
    ));
    if !i.jobs.is_empty() {
        out.push(format!("  {}", i.jobs.iter().map(|(j, w)| format!("{j} ({w})")).collect::<Vec<_>>().join(", ")));
    }
    out.push("Latencia de decisión [objetivo mediana < 30 min]:".into());
    out.push(lat_line("aviso question → mensaje del orquestador", &r.latency_progress));
    out.push(lat_line("aviso blocker → mensaje del orquestador", &r.latency_blockers));
    out.push(lat_line("decision.request → respuesta (Nod)", &r.latency_nod));
    let t = &r.turns;
    if t.sessions.is_empty() {
        out.push("Turnos del orquestador: sin sesiones de orquestador en los eventos de Nod".into());
    } else {
        out.push(format!(
            "Turnos del orquestador: {} en {} sesiones · sin herramientas {} ({}) [objetivo < 5 %] · abiertos sin el humano {}, de ellos vacíos {}",
            t.turns,
            t.sessions.len(),
            t.empty,
            pct(t.empty_rate),
            t.wakeups,
            t.empty_wakeups
        ));
        let origins: Vec<String> = t
            .by_origin
            .iter()
            .map(|(o, n, e)| format!("{} {e}/{n}", if o.is_empty() { "?" } else { o.as_str() }))
            .collect();
        out.push(format!("  vacíos por origen del prompt: {}", origins.join(" · ")));
        for (s, n, e) in t.by_session.iter().take(5) {
            out.push(format!("  {s}: {e} vacíos de {n}"));
        }
    }
    if r.shas_per_branch.is_empty() {
        out.push("Shas medidos por rama: sin eventos integrate".into());
    } else {
        let rows: Vec<String> = r.shas_per_branch.iter().map(|s| format!("{} {} shas ({} ok, {} fallos)", s.branch, s.shas, s.verified, s.failed)).collect();
        out.push(format!("Shas medidos por rama: {}", rows.join(" · ")));
    }
    for n in &r.notes {
        out.push(format!("nota: {n}"));
    }
    out.join("\n")
}

/// Opciones desde los argumentos del tool.
pub fn opts(args: &Value) -> DelegaOpts {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
    let n = |k: &str, d: i64| args.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|x| x.parse().ok()))).unwrap_or(d);
    DelegaOpts {
        since_epoch: since_to_epoch(s("since")),
        until_epoch: if s("until").is_empty() { 0 } else { since_to_epoch(s("until")) },
        orchestrators: s("orchestrators").split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect(),
        stale_ms: n("stale_hours", 6).max(0) * 3600 * 1000,
        limit: n("limit", 10).max(1) as usize,
    }
}

pub fn run(conn: &rusqlite::Connection, args: &Value) -> Result<String> {
    let rep = chrono_metrics::delega_report(conn, &opts(args))?;
    if args.get("format").and_then(Value::as_str) == Some("json") {
        Ok(serde_json::to_string_pretty(&to_json(&rep))?)
    } else {
        Ok(to_text(&rep))
    }
}
