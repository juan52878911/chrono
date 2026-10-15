//! `informe`: la consulta compuesta. Lo que un agente pide en 6–10 llamadas
//! encadenadas (fuentes → niveles → servicios por nivel → patrones → picos →
//! episodios) en UNA, acotada y determinista, con las consultas siguientes ya
//! escritas para profundizar. Reutiliza las mismas funciones que las tools
//! sueltas: las cifras coinciden byte a byte con las de `top`/`patterns`/`precede`.

use serde_json::{json, Value};

use chrono_metrics::{EventScope, PrecedeOpts, TopFilter};

use crate::Result;

const LEVELS: &[&str] = &["FATAL", "CRITICAL", "ERROR", "WARNING"];

pub fn informe(store: &chrono_store::Store, args: &Value, since_epoch: i64) -> Result<Value> {
    match args.get("tipo").and_then(Value::as_str).unwrap_or("logs") {
        "codigo" | "código" | "code" => codigo(store, args, since_epoch),
        _ => logs(store.conn(), args, since_epoch),
    }
}

fn logs(conn: &rusqlite::Connection, args: &Value, since_epoch: i64) -> Result<Value> {
    let sc = crate::tools::scope_from(args);
    let base = |level: Option<&'static str>| TopFilter {
        source: sc.source,
        grep: sc.grep,
        level,
        kind: sc.kind,
        entity: sc.entity,
        attr: sc.attr,
        infer_level: false,
        until_epoch: sc.until_epoch,
    };
    let pairs = |v: Vec<chrono_metrics::TopValue>| -> String {
        v.iter().map(|t| format!("{}={}", t.value, t.count)).collect::<Vec<_>>().join(" ")
    };
    // Cobertura y líneas sin nivel, por fuente.
    let fuentes: Vec<Value> = chrono_metrics::sources(conn)?
        .into_iter()
        .filter(|s| s.kind != "git" && sc.source.map_or(true, |f| s.source.contains(f) || s.alias.as_deref() == Some(f)))
        .map(|s| {
            let p = |v: &[(String, i64)]| v.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(" ");
            let name = s.alias.clone().unwrap_or_else(|| {
                let (kind, path) = s.source.split_once(':').unwrap_or(("", &s.source));
                format!("{kind}:{}", path.rsplit('/').next().unwrap_or(path))
            });
            json!({"source": name, "events": s.events, "first": s.first, "last": s.last,
                   "unlabeled_inferred": p(&s.inferred_levels)})
        })
        .collect();
    let niveles = pairs(chrono_metrics::top_filtered(conn, "level", since_epoch, &base(None), 12)?);
    // Por nivel: servicios, contenedores y mensajes más repetidos.
    let mut por_nivel = Vec::new();
    let mut graves: Vec<String> = Vec::new();
    for lv in LEVELS {
        let svc = chrono_metrics::top_filtered(conn, "entity", since_epoch, &base(Some(lv)), 10)?;
        if svc.is_empty() {
            continue;
        }
        let total: i64 = chrono_metrics::top_filtered(conn, "level", since_epoch, &base(Some(lv)), 1)?.first().map_or(0, |t| t.count);
        let cont = chrono_metrics::top_filtered(conn, "attr:container", since_epoch, &base(Some(lv)), 8)?;
        let scope = EventScope { level: Some(lv), ..sc.clone() };
        let pats = chrono_metrics::patterns_scoped(conn, since_epoch, &scope, 5)?;
        if *lv != "WARNING" {
            graves.extend(pats.iter().take(2).map(|p| p.template.clone()));
        }
        por_nivel.push(json!({
            "level": lv, "count": total, "servicios": pairs(svc), "contenedores": pairs(cont),
            "mensajes": pats.iter().map(|p| json!({"count": p.count, "template": p.template, "example_id": p.example_id})).collect::<Vec<_>>(),
        }));
    }
    // Picos de lo grave (10 min).
    let grave_scope = EventScope { level: Some("FATAL,CRITICAL,ERROR"), ..sc.clone() };
    let mut buckets = chrono_metrics::timeline_scoped(conn, since_epoch, sc.until_epoch, 600_000, None, &grave_scope, 0)?;
    buckets.sort_by(|a, b| b.count.cmp(&a.count).then(a.bucket_epoch.cmp(&b.bucket_epoch)));
    let picos: Vec<Value> = buckets.iter().take(5).map(|b| json!({"at": crate::contract::iso(b.bucket_epoch), "count": b.count})).collect();
    // Episodios de FATAL/CRITICAL (nivel de la fuente) y lo que los precede.
    let o = PrecedeOpts {
        since_epoch,
        // Para ENCONTRAR episodios vale el nivel deducido (hay apagados sin nivel en la fuente).
        anchor: EventScope { level: Some("FATAL,CRITICAL"), infer_level: true, ..sc.clone() },
        max_episodes: 24,
        per_episode: 10,
        by_origin: true,
        ..Default::default()
    };
    let (eps, common) = chrono_metrics::precede(conn, &o)?;
    let n_eps = eps.len();
    let episodios: Vec<Value> = eps
        .iter()
        .map(|e| {
            // Lo que precede va en `precursores_comunes`; aquí, el precursor más
            // repetido entre episodios (a igualdad, el más grave).
            let rank = |t: &str, l: &str| common.iter().position(|c| c.template == t && c.level == l).unwrap_or(usize::MAX);
            let mut pre: Vec<&chrono_metrics::Precursor> = e.preceding.iter().collect();
            pre.sort_by_key(|p| (rank(&p.template, &p.level), p.last_before_secs));
            let antes: Vec<String> = pre
                .iter()
                .take(2)
                .map(|p| format!("{}s {}: {}", p.last_before_secs, p.entity, crate::contract::short(&p.template).chars().take(70).collect::<String>()))
                .collect();
            json!({"start": e.start_at, "anchors": e.anchors, "origins": e.origins,
                   "antes": if antes.is_empty() { Value::Null } else { json!(antes.join(" | ")) }})
        })
        .collect();
    let comunes: Vec<Value> = common
        .iter()
        .map(|c| json!({"before_episodes": format!("{}/{}", c.episodes, n_eps), "level": c.level, "template": c.template}))
        .collect();
    // Consultas siguientes, ya escritas.
    let mut siguientes: Vec<String> = Vec::new();
    for t in graves.iter().take(2) {
        siguientes.push(format!("precede grep=\"{}\" by_origin=true → cuántas veces, cuándo y qué lo precede", literal(t)));
    }
    siguientes.push("events entity=<servicio> level=ERROR since=<hora> limit=80 → leer una traza completa (la excepción y la petición que falló)".into());
    siguientes.push("Las cifras por nivel son las de la fuente; unlabeled_inferred son líneas SIN nivel cuyo texto lo dice: mencionarlas aparte, no sumarlas.".into());
    Ok(json!({
        "fuentes": fuentes,
        "niveles": niveles,
        "por_nivel": por_nivel,
        "picos_graves_10m": picos,
        "episodios_fatal_critical": n_eps,
        "precursores_comunes": comunes,
        "episodios": episodios,
        "siguientes": siguientes,
    }))
}

fn codigo(store: &chrono_store::Store, args: &Value, since_epoch: i64) -> Result<Value> {
    let target = args.get("target").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("informe tipo=codigo necesita target=fichero[:A-B]")?;
    let file = target.split(':').next().unwrap_or(target);
    let conn = store.conn();
    let explain = crate::tools::explain(store, target, 10).unwrap_or_else(|e| json!({"error": e.to_string()}));
    let owners = crate::tools::owners(conn, file, since_epoch)?;
    let coupling = crate::tools::coupling(conn, file, since_epoch)?;
    let hot = crate::tools::hotspots(conn, since_epoch)?;
    let rank = hot["hotspots"].as_array().and_then(|a| a.iter().position(|h| h["entity"] == file)).map(|p| p + 1);
    Ok(json!({
        "target": target,
        "hotspot_rank": rank.map_or(Value::Null, |r| json!(r)),
        "owners": owners["owners"], "bus_factor": owners["bus_factor"],
        "cambia_con": coupling["coupled"],
        "explain": explain,
    }))
}

/// El trozo literal más largo de una plantilla (sin `<TS>`, `<NUM>`…), para
/// usarlo de `grep`: casa con todas las líneas de esa plantilla.
fn literal(template: &str) -> String {
    let mut best = String::new();
    let mut cur = String::new();
    let mut in_ph = false;
    for ch in template.chars() {
        match ch {
            '<' => {
                in_ph = true;
                if cur.trim().len() > best.trim().len() {
                    best = cur.clone();
                }
                cur.clear();
            }
            '>' if in_ph => in_ph = false,
            _ if !in_ph => cur.push(ch),
            _ => {}
        }
    }
    if cur.trim().len() > best.trim().len() {
        best = cur;
    }
    best.trim_matches(|c: char| c.is_whitespace() || "[]():,\"".contains(c)).chars().take(70).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn literal_de_plantilla() {
        assert_eq!(super::literal("<TS> <TS> UTC [<NUM>] FATAL: terminating connection due to administrator command"), "FATAL: terminating connection due to administrator command");
        assert_eq!(super::literal("ERROR: catalog version mismatch, expected \"<NUM>.<NUM>\""), "ERROR: catalog version mismatch, expected");
    }
}
