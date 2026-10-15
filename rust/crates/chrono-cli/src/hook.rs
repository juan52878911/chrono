//! `chrono hook <modo>`: chrono como intermediario entre las herramientas y el
//! modelo, para los hooks de Claude Code. Sustituye al prototipo en Python.
//!
//! Solo actúa donde `.chrono/intermediario.json` lo pide (lo escribe la skill
//! `chrono-intermediario`) y por encima de sus límites; en cualquier otro caso
//! no imprime nada y la herramienta sigue igual. Nunca rompe la herramienta: un
//! error se apunta en `CHRONO_INTERMEDIARIO_LOG` (si está) y se sale con 0.
//!
//! Modos:
//! - `post`  (PostToolUse de tools MCP): con la regla `loki`, indexa EN SEGUNDO
//!   PLANO la ventana completa del selector consultado (el resto de la sesión ya
//!   tiene los datos en chrono) y, si la salida del MCP venía truncada, la
//!   sustituye (`updatedMCPToolOutput`) por cifras exactas de TODA la consulta +
//!   una muestra. Con `records`, agrega la salida grande que ya trae los datos.
//! - `read`  (PreToolUse de Read): leer ENTERO un fichero de datos muy grande
//!   (≥ `read.deny_bytes`, 5 MB) se cambia por su resumen exacto.
//! - `after-read` (PostToolUse de Read): un fichero de datos mediano (entre
//!   `read.min_bytes` y `deny_bytes`) se lee normal, se indexa y se le dice al
//!   modelo que las cifras salen de chrono.
//! - `loki-index` (interno): el trabajo en segundo plano de `post`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

type R<T> = std::result::Result<T, String>;

const DEFAULT_LABELS: &[&str] = &["service", "level", "container"];

fn log(msg: &str) {
    if let Ok(p) = std::env::var("CHRONO_INTERMEDIARIO_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{} {msg}", crate::timeutil::epoch_to_iso8601(crate::timeutil::now_epoch()));
        }
    }
}

/// Punto de entrada: lee el evento del hook por stdin e imprime la respuesta.
pub fn run(mode: &str) -> i32 {
    if std::env::var("CHRONO_INTERMEDIARIO").as_deref() == Ok("0") {
        return 0;
    }
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return 0;
    }
    let Ok(ev) = serde_json::from_str::<Value>(&buf) else { return 0 };
    match std::panic::catch_unwind(|| handle(mode, &ev)) {
        Ok(Ok(Some(out))) => println!("{out}"),
        Ok(Ok(None)) => {}
        Ok(Err(e)) => log(&format!("ERROR {mode}: {e}")),
        Err(_) => log(&format!("PANIC {mode}")),
    }
    0
}

fn handle(mode: &str, ev: &Value) -> R<Option<String>> {
    if mode == "loki-index" {
        loki_index(ev)?;
        return Ok(None);
    }
    if mode == "stop" {
        return verify_numbers(ev);
    }
    let cwd = ev.get("cwd").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let Some(root) = find_root(&cwd) else { return Ok(None) };
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".chrono/intermediario.json")).map_err(|e| e.to_string())?)
        .map_err(|e| format!("intermediario.json: {e}"))?;
    if !cfg.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
        return Ok(None);
    }
    let tool = ev.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let empty = json!({});
    let input = ev.get("tool_input").unwrap_or(&empty);
    let t0 = Instant::now();
    let out = match mode {
        "read" => read_deny(&cfg, input, &root)?.map(|reason| {
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}})
        }),
        "after-read" => after_read(&cfg, input, &root, ev.get("session_id").and_then(Value::as_str).unwrap_or(""))?
            .map(|ctx| json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": ctx}})),
        "post" => {
            let mut out = None;
            for rule in cfg.get("rules").and_then(Value::as_array).cloned().unwrap_or_default() {
                let pat = rule.get("tool").and_then(Value::as_str).unwrap_or("");
                if !tool_matches(pat, tool) {
                    continue;
                }
                let text = match rule.get("adapter").and_then(Value::as_str) {
                    Some("loki") => adapter_loki(&rule, input, ev.get("tool_response"), &root)?,
                    Some("records") => adapter_records(&rule, tool, ev.get("tool_response"), &root)?,
                    _ => None,
                };
                out = text.map(|t| json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedMCPToolOutput": t}}));
                break;
            }
            out
        }
        _ => None,
    };
    if let Some(o) = &out {
        log(&format!("{mode} {tool} → {} c en {} ms", o.to_string().len(), t0.elapsed().as_millis()));
    }
    Ok(out.map(|o| o.to_string()))
}

/// `mcp__grafana__query_loki_logs`, `mcp__*__query_loki*` o varias con `|`.
fn tool_matches(pattern: &str, tool: &str) -> bool {
    pattern.split('|').map(str::trim).any(|p| wild(p.as_bytes(), tool.as_bytes()))
}

fn wild(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => wild(&p[1..], s) || (!s.is_empty() && wild(p, &s[1..])),
        (Some(b'?'), Some(_)) => wild(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => wild(&p[1..], &s[1..]),
        _ => false,
    }
}

fn find_root(start: &Path) -> Option<PathBuf> {
    let mut d = start.to_path_buf();
    loop {
        if d.join(".chrono/intermediario.json").is_file() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

fn num(v: &Value, k: &str, default: i64) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(default)
}

fn self_bin() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("chrono"))
}

/// `chrono <args> --compact --db <db>` (este mismo binario).
fn chrono_q(db: &Path, args: &[&str]) -> String {
    Command::new(self_bin())
        .args(args)
        .args(["--compact", "--db"])
        .arg(db)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

// ── Tiempo ────────────────────────────────────────────────────────────────────

/// RFC3339 (con fracción y zona) o relativo (`now`, `now-24h`, `now-90m`) → ns.
fn to_ns(v: Option<&str>, default: &str) -> R<i64> {
    let v = v.filter(|s| !s.trim().is_empty()).unwrap_or(default).trim();
    let now_ns = crate::timeutil::now_epoch() * 1_000_000;
    if let Some(rest) = v.strip_prefix("now") {
        if rest.is_empty() {
            return Ok(now_ns);
        }
        let d = rest.strip_prefix('-').and_then(crate::timeutil::parse_duration).ok_or(format!("tiempo no válido: {v}"))?;
        return Ok(now_ns - d * 1_000_000);
    }
    let (main, offset_secs) = split_offset(v);
    let (base, frac) = match main.split_once('.') {
        Some((b, f)) => (b, f),
        None => (main, ""),
    };
    let ms = crate::timeutil::parse_since(base).ok_or(format!("tiempo no válido: {v}"))?;
    let frac_ns: i64 = format!("{:0<9}", &frac[..frac.len().min(9)]).parse().unwrap_or(0);
    Ok((ms - offset_secs * 1000) * 1_000_000 + frac_ns)
}

fn split_offset(v: &str) -> (&str, i64) {
    if let Some(s) = v.strip_suffix('Z') {
        return (s, 0);
    }
    if let Some(t) = v.find('T') {
        if let Some(pos) = v[t..].rfind(['+', '-']).map(|p| p + t) {
            let off = &v[pos + 1..];
            let (h, m) = off.split_once(':').unwrap_or((off.get(..2).unwrap_or("0"), off.get(2..).unwrap_or("0")));
            let secs = h.parse::<i64>().unwrap_or(0) * 3600 + m.parse::<i64>().unwrap_or(0) * 60;
            return (&v[..pos], if &v[pos..pos + 1] == "+" { secs } else { -secs });
        }
    }
    (v, 0)
}

// ── Loki ──────────────────────────────────────────────────────────────────────

/// `{…}` de una consulta LogQL (de logs o de métricas): lo que se indexa.
fn stream_selector(logql: &str) -> Option<String> {
    let start = logql.find('{')?;
    let mut depth = 0;
    let mut quote = false;
    let b = logql.as_bytes();
    for i in start..b.len() {
        match b[i] {
            b'"' if i == 0 || b[i - 1] != b'\\' => quote = !quote,
            b'{' if !quote => depth += 1,
            b'}' if !quote => {
                depth -= 1;
                if depth == 0 {
                    return Some(logql[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// El selector que se INDEXA: sin las condiciones sobre etiquetas que solo
/// filtran (por defecto `level`), para que `{service="x", level="ERROR"}` y
/// `{service="x"}` sean la misma fuente y no se vuelva a pedir lo mismo.
fn index_selector(sel: &str, drop: &[String]) -> String {
    let inner = sel.trim().trim_start_matches('{').trim_end_matches('}');
    let mut parts: Vec<String> = Vec::new();
    let (mut cur, mut quote, mut prev) = (String::new(), false, ' ');
    for ch in inner.chars() {
        if ch == '"' && prev != '\\' {
            quote = !quote;
        }
        if ch == ',' && !quote {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
        prev = ch;
    }
    parts.push(cur);
    let kept: Vec<String> = parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let name: String = p.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            !drop.iter().any(|d| d == &name)
        })
        .collect();
    if kept.is_empty() {
        sel.to_string() // no se puede quitar todo: Loki exige al menos un matcher
    } else {
        format!("{{{}}}", kept.join(", "))
    }
}

/// ¿[start, end] está dentro de la unión de las ventanas ya cubiertas?
fn covered(windows: &mut Vec<(i64, i64)>, start: i64, end: i64) -> bool {
    windows.sort();
    let mut reach = start;
    for (a, b) in windows.iter() {
        if *a > reach {
            break;
        }
        if *b > reach {
            reach = *b;
        }
        if reach >= end {
            return true;
        }
    }
    reach >= end
}

fn is_metric(logql: &str) -> bool {
    !logql.trim_start().starts_with('{')
}

fn token(rule: &Value) -> R<String> {
    if let Ok(t) = std::env::var("LOKI_TOKEN") {
        return Ok(t);
    }
    let cmd = rule.get("token_cmd").and_then(Value::as_str).map(str::to_string).or_else(|| std::env::var("LOKI_TOKEN_CMD").ok());
    match cmd {
        Some(c) => {
            let o = Command::new("sh").arg("-c").arg(&c).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
            Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
        }
        None => Ok(String::new()),
    }
}

/// Una fila por línea de log, ordenada por tiempo; `(filas, ¿quedaban más?)`.
fn loki_fetch(rule: &Value, logql: &str, start_ns: i64, end_ns: i64, max_lines: usize, deadline: Instant) -> R<(Vec<Value>, bool)> {
    let url = rule
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| std::env::var("LOKI_URL").ok())
        .ok_or("loki: falta url en la regla o LOKI_URL")?;
    let url = format!("{}/loki/api/v1/query_range", url.trim_end_matches('/'));
    let tok = token(rule)?;
    let keep: Vec<String> = rule
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_else(|| DEFAULT_LABELS.iter().map(|s| s.to_string()).collect());
    let mut rows: Vec<(i64, Value)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur = start_ns;
    let mut more = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left < Duration::from_secs(2) || rows.len() >= max_lines {
            more = true;
            break;
        }
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "-G", "--fail", "--max-time", &left.as_secs().max(2).to_string(), "-H", "@-"])
            .arg(&url)
            .args(["--data-urlencode", &format!("query={logql}")])
            .args(["--data-urlencode", &format!("start={cur}")])
            .args(["--data-urlencode", &format!("end={end_ns}")])
            .args(["--data-urlencode", "limit=5000", "--data-urlencode", "direction=forward"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("curl: {e}"))?;
        // El token va por stdin (`-H @-`), no en la línea de órdenes: `ps` no lo ve.
        if let Some(mut si) = child.stdin.take() {
            if !tok.is_empty() {
                let _ = writeln!(si, "Authorization: Bearer {tok}");
            }
        }
        let o = child.wait_with_output().map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(format!("loki: {}", String::from_utf8_lossy(&o.stderr).trim()));
        }
        let data: Value = serde_json::from_slice(&o.stdout).map_err(|e| format!("loki: {e}"))?;
        if data["data"]["resultType"] != "streams" {
            return Ok((Vec::new(), false));
        }
        let mut total = 0;
        let mut batch: Vec<(i64, Value)> = Vec::new();
        for s in data["data"]["result"].as_array().cloned().unwrap_or_default() {
            let lab = &s["stream"];
            for pair in s["values"].as_array().cloned().unwrap_or_default() {
                total += 1;
                let ts: i64 = pair[0].as_str().and_then(|t| t.parse().ok()).unwrap_or(0);
                let line = pair[1].as_str().unwrap_or("").to_string();
                let key = format!("{ts}\u{1}{lab}\u{1}{line}");
                if !seen.insert(key) {
                    continue;
                }
                let mut r = serde_json::Map::new();
                r.insert("ts".into(), json!(ts / 1_000_000));
                r.insert("msg".into(), json!(line));
                for k in &keep {
                    if let Some(v) = lab.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()) {
                        r.insert(k.clone(), json!(v));
                    }
                }
                batch.push((ts, Value::Object(r)));
            }
        }
        batch.sort_by_key(|(t, _)| *t);
        let last = batch.last().map(|(t, _)| *t);
        rows.extend(batch);
        match last {
            Some(t) if total >= 5000 => cur = t,
            _ => break,
        }
    }
    rows.truncate(max_lines);
    Ok((rows.into_iter().map(|(_, v)| v).collect(), more))
}

/// ¿La salida del MCP venía truncada? Lee los metadatos de mcp-grafana
/// (`linesReturned`, `resultsTruncated`, `maxLinesAllowed`) o, sin ellos, el tamaño.
fn looked_truncated(resp: Option<&Value>, asked: i64) -> bool {
    let text = response_text(resp);
    let grab = |k: &str| -> Option<i64> {
        let i = text.find(&format!("\"{k}\""))? + k.len() + 2;
        let rest = text[i..].trim_start().trim_start_matches(':').trim_start();
        rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
    if text.contains("\"resultsTruncated\":true") || text.contains("\"resultsTruncated\": true") {
        return true;
    }
    match grab("linesReturned") {
        Some(n) => n >= asked || grab("maxLinesAllowed").is_some_and(|m| n >= m),
        None => text.len() > 30_000,
    }
}

fn adapter_loki(rule: &Value, input: &Value, resp: Option<&Value>, root: &Path) -> R<Option<String>> {
    let logql = input.get("logql").or_else(|| input.get("query")).and_then(Value::as_str).unwrap_or("");
    if logql.is_empty() {
        return Ok(None);
    }
    let start = to_ns(input.get("startRfc3339").or_else(|| input.get("start")).and_then(Value::as_str), "now-1h")?;
    let end = to_ns(input.get("endRfc3339").or_else(|| input.get("end")).and_then(Value::as_str), "now")?;
    // 1. Indexado en segundo plano de la ventana completa del selector (también
    //    para consultas de métricas: lo siguiente que pregunte ya estará en chrono).
    // Sin ventana explícita la consulta es «la última hora» y caduca enseguida:
    // no vale la pena indexarla.
    let has_window = ["startRfc3339", "start"].iter().any(|k| input.get(*k).and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty() && !s.starts_with("now")));
    if has_window && rule.get("auto_index").and_then(Value::as_bool).unwrap_or(true) {
        if let Some(sel) = stream_selector(logql) {
            let drop: Vec<String> = rule
                .get("index_drop_labels")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_else(|| vec!["level".to_string()]);
            spawn_index(rule, &index_selector(&sel, &drop), start, end, root);
        }
    }
    // 2. Sustituir solo si la salida venía truncada (las métricas ya son exactas).
    let asked = num(input, "limit", 10).max(1);
    if is_metric(logql) || input.get("queryType").and_then(Value::as_str) == Some("instant") || !looked_truncated(resp, asked) {
        return Ok(None);
    }
    let deadline = Instant::now() + Duration::from_secs(num(rule, "timeout_secs", 45).max(5) as u64);
    let (rows, more) = loki_fetch(rule, logql, start, end, num(rule, "max_lines", 50_000) as usize, deadline)?;
    if rows.len() as i64 <= asked {
        return Ok(None);
    }
    let name = rule.get("name").and_then(Value::as_str).unwrap_or("loki");
    let source = ingest(root, name, &rows)?;
    let backward = input.get("direction").and_then(Value::as_str).unwrap_or("backward") == "backward";
    let n = (asked as usize).min(num(rule, "sample_lines", 40) as usize).min(rows.len());
    let sample: Vec<&Value> = if backward { rows.iter().rev().take(n).collect() } else { rows.iter().take(n).collect() };
    let head = format!(
        "[chrono · intermediario] La consulta casa con {} líneas en la ventana pedida{}; el MCP solo devolvía {asked}. Cifras EXACTAS sobre las {}:",
        rows.len(),
        if more { " (tope del intermediario alcanzado: hay más)" } else { "" },
        rows.len()
    );
    let note = format!("muestra: {n} líneas ({}):", if backward { "las más recientes primero" } else { "las más antiguas primero" });
    Ok(Some(summary(&rows, &head, Some(&source), &sample.iter().map(|r| fmt_row(r)).collect::<Vec<_>>(), &note, &["level", "service", "container"])))
}

/// Lanza `chrono hook loki-index` desacoplado (su propio grupo de procesos) si
/// esa ventana del selector no está ya cubierta.
fn spawn_index(rule: &Value, selector: &str, start: i64, end: i64, root: &Path) {
    let job = json!({"rule": rule, "selector": selector, "start": start, "end": end, "root": root});
    let mut c = Command::new(self_bin());
    c.args(["hook", "loki-index"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let Ok(mut child) = c.spawn() else { return };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(job.to_string().as_bytes());
    }
}

/// Trabajo de fondo: trae TODO el selector en la ventana y lo indexa. Lleva un
/// registro de ventanas cubiertas por selector para no repetir.
fn loki_index(job: &Value) -> R<()> {
    let rule = &job["rule"];
    let selector = job["selector"].as_str().ok_or("sin selector")?;
    let (start, end) = (job["start"].as_i64().unwrap_or(0), job["end"].as_i64().unwrap_or(0));
    let root = PathBuf::from(job["root"].as_str().ok_or("sin root")?);
    let dir = root.join(".chrono/mcp");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let lock = dir.join("loki.index.lock");
    if std::fs::create_dir(&lock).is_err() {
        return Ok(()); // otro proceso indexando
    }
    let res = (|| -> R<()> {
        let cover_p = dir.join("loki.cover.json");
        let mut cover: Vec<Value> = std::fs::read_to_string(&cover_p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut wins: Vec<(i64, i64)> = cover
            .iter()
            .filter(|c| c["selector"] == selector)
            .filter_map(|c| Some((c["start"].as_i64()?, c["end"].as_i64()?)))
            .collect();
        if covered(&mut wins, start, end) {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(num(rule, "index_timeout_secs", 300) as u64);
        let (rows, more) = loki_fetch(rule, selector, start, end, num(rule, "index_max_lines", 200_000) as usize, deadline)?;
        let name = rule.get("name").and_then(Value::as_str).unwrap_or("loki");
        ingest(&root, name, &rows)?;
        if !more {
            cover.push(json!({"selector": selector, "start": start, "end": end, "lines": rows.len()}));
            std::fs::write(&cover_p, serde_json::to_string_pretty(&cover).unwrap_or_default()).map_err(|e| e.to_string())?;
        }
        log(&format!("loki-index {selector} → {} líneas{}", rows.len(), if more { " (incompleto)" } else { "" }));
        Ok(())
    })();
    let _ = std::fs::remove_dir(&lock);
    res
}

// ── Ingesta y resumen ─────────────────────────────────────────────────────────

/// Añade las filas a `.chrono/mcp/<name>.jsonl` sin duplicados y sincroniza el
/// índice del proyecto. Devuelve el id de la fuente (para `source=`).
fn ingest(root: &Path, name: &str, rows: &[Value]) -> R<String> {
    let dir = root.join(".chrono/mcp");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{name}.jsonl"));
    let keys_p = dir.join(format!("{name}.jsonl.keys"));
    let mut seen: std::collections::HashSet<String> =
        std::fs::read_to_string(&keys_p).unwrap_or_default().lines().map(str::to_string).collect();
    let mut out = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
    let mut keys = std::fs::OpenOptions::new().create(true).append(true).open(&keys_p).map_err(|e| e.to_string())?;
    for r in rows {
        let line = r.to_string();
        let k = format!("{:016x}", chrono_core_hash(&line));
        if seen.insert(k.clone()) {
            writeln!(out, "{line}").map_err(|e| e.to_string())?;
            writeln!(keys, "{k}").map_err(|e| e.to_string())?;
        }
    }
    let db = root.join(".chrono/index.db");
    let id = format!("jsonl:{}", path.display());
    let known = db.is_file() && chrono_q(&db, &["sources"]).contains(&format!("{name}.jsonl"));
    let mut c = Command::new(self_bin());
    if known {
        c.args(["sync", "--db"]).arg(&db);
    } else {
        c.arg("add").arg(&path).arg("--db").arg(&db);
    }
    c.current_dir(root).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(|e| e.to_string())?;
    Ok(id)
}

/// FNV-1a de 64 bits: clave estable de deduplicación (no criptográfica).
fn chrono_core_hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn iso(ms: i64) -> String {
    crate::timeutil::epoch_to_iso8601(ms)
}

fn sval(r: &Value, k: &str) -> String {
    match r.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

fn counts<'a>(rows: impl Iterator<Item = String>) -> Vec<(String, usize)> {
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    for k in rows.filter(|k| !k.is_empty()) {
        *m.entry(k).or_insert(0) += 1;
    }
    let mut v: Vec<(String, usize)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

fn template(msg: &str) -> String {
    let t = chrono_core::templatize(msg.lines().next().unwrap_or(""));
    // El prefijo de fecha/proceso de la línea (`<TS> <TS> UTC [<NUM>] `) no
    // distingue nada y gasta caracteres del resumen.
    let mut s = t.as_str();
    loop {
        let before = s;
        for p in ["<TS> ", "UTC ", "[<NUM>] "] {
            s = s.strip_prefix(p).unwrap_or(s);
        }
        if s == before {
            break;
        }
    }
    s.chars().take(160).collect()
}

fn summary(rows: &[Value], head: &str, source: Option<&str>, sample: &[String], sample_note: &str, dims: &[&str]) -> String {
    let mut lines = vec![head.to_string()];
    let ts: Vec<i64> = rows.iter().filter_map(|r| r.get("ts").and_then(Value::as_i64)).collect();
    if let (Some(a), Some(b)) = (ts.iter().min(), ts.iter().max()) {
        lines.push(format!("ventana real de los datos: {} → {}", iso(*a), iso(*b)));
    }
    for d in dims {
        let c = counts(rows.iter().map(|r| sval(r, d)));
        if !c.is_empty() && c.len() < rows.len() {
            let shown: Vec<String> = c.iter().take(15).map(|(k, n)| format!("{k}={n}")).collect();
            let rest = c.len().saturating_sub(15);
            lines.push(format!("{d}: {}{}", shown.join(", "), if rest > 0 { format!(" (+{rest} valores más)") } else { String::new() }));
        }
    }
    let levels = counts(rows.iter().map(|r| sval(r, "level")));
    if !levels.is_empty() {
        lines.push("mensajes más repetidos POR NIVEL (veces · plantilla):".into());
        for (lv, _) in &levels {
            if matches!(lv.to_ascii_uppercase().as_str(), "INFO" | "DEBUG" | "TRACE" | "LOG") {
                continue;
            }
            let c = counts(rows.iter().filter(|r| sval(r, "level") == *lv).map(|r| template(&sval(r, "msg"))));
            let top: Vec<String> = c.iter().take(5).map(|(t, n)| format!("{n}× {}", t.chars().take(110).collect::<String>())).collect();
            lines.push(format!("  {lv}: {}", top.join(" | ")));
        }
        let sin = rows.iter().filter(|r| sval(r, "level").is_empty()).count();
        if sin > 0 {
            let inferred = counts(rows.iter().filter(|r| sval(r, "level").is_empty()).filter_map(|r| chrono_core::infer_level(&sval(r, "msg")).map(str::to_string)));
            let inf: Vec<String> = inferred.iter().map(|(k, n)| format!("{k}={n}")).collect();
            lines.push(format!(
                "  ({sin} líneas sin nivel de la fuente{}; no cuentan en los niveles de arriba)",
                if inf.is_empty() { String::new() } else { format!(", aunque su texto dice {}", inf.join(" ")) }
            ));
        }
    } else {
        let c = counts(rows.iter().map(|r| template(&sval(r, "msg"))));
        if !c.is_empty() {
            lines.push("mensajes más repetidos (veces · plantilla):".into());
            lines.extend(c.iter().take(10).map(|(t, n)| format!("  {n:>6} · {t}")));
        }
    }
    if !sample.is_empty() {
        lines.push(sample_note.to_string());
        lines.extend(sample.iter().cloned());
    }
    if let Some(src) = source {
        lines.push(format!(
            "Las {} filas están en chrono (source={src}): para contar, rankear, ver qué precede a algo (precede) o leer en orden, usa sus tools top/patterns/precede/timeline/events con filtros level (infer_level para las sin nivel), entity, attr (container:<nombre>), grep, since/until. Las cifras de arriba son exactas: no las recuentes.",
            rows.len()
        ));
    }
    lines.join("\n")
}

fn fmt_row(r: &Value) -> String {
    let ts = r.get("ts").and_then(Value::as_i64).map(iso).unwrap_or_default();
    let who = Some(sval(r, "container")).filter(|s| !s.is_empty()).unwrap_or_else(|| sval(r, "service"));
    let lv = Some(sval(r, "level")).filter(|s| !s.is_empty()).unwrap_or_else(|| "-".into());
    let msg: String = sval(r, "msg").chars().take(400).collect();
    format!("{ts} {who} {lv} {msg}")
}

// ── records ───────────────────────────────────────────────────────────────────

fn response_text(resp: Option<&Value>) -> String {
    match resp {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(""),
        Some(Value::Object(o)) if o.contains_key("content") => response_text(o.get("content")),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn find_records(v: &Value) -> Vec<Value> {
    let mut best: Vec<Value> = Vec::new();
    let mut stack = vec![v];
    while let Some(x) = stack.pop() {
        match x {
            Value::Array(a) => {
                if a.len() > best.len() && !a.is_empty() && a.iter().take(50).all(Value::is_object) {
                    best = a.clone();
                }
                stack.extend(a.iter().filter(|i| i.is_array() || i.is_object()));
            }
            Value::Object(o) => stack.extend(o.values().filter(|i| i.is_array() || i.is_object())),
            _ => {}
        }
    }
    best
}

fn adapter_records(rule: &Value, tool: &str, resp: Option<&Value>, root: &Path) -> R<Option<String>> {
    let text = response_text(resp);
    if (text.len() as i64) < num(rule, "min_bytes", 30_000) {
        return Ok(None);
    }
    let mut recs = serde_json::from_str::<Value>(&text).map(|v| find_records(&v)).unwrap_or_default();
    if recs.is_empty() {
        recs = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(Value::is_object).collect();
    }
    if (recs.len() as i64) < num(rule, "min_records", 20) {
        return Ok(None);
    }
    let name = rule.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| tool.replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_"));
    let tf = rule.get("time_field").and_then(Value::as_str);
    let mf = rule.get("message_field").and_then(Value::as_str);
    let rows: Vec<Value> = recs
        .iter()
        .map(|r| {
            let mut o = r.as_object().cloned().unwrap_or_default();
            if let Some(v) = tf.and_then(|f| r.get(f)) {
                o.insert("ts".into(), v.clone());
            }
            if let Some(v) = mf.and_then(|f| r.get(f)) {
                o.insert("msg".into(), v.clone());
            }
            Value::Object(o)
        })
        .collect();
    let source = ingest(root, &name, &rows)?;
    let dims: Vec<String> = rule
        .get("dims")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_else(|| {
            ["level", "status", "state", "service", "conclusion"].iter().filter(|k| recs.iter().take(50).any(|r| r.get(**k).is_some())).map(|s| s.to_string()).collect()
        });
    let dims_ref: Vec<&str> = dims.iter().map(String::as_str).collect();
    let n = num(rule, "sample_lines", 15) as usize;
    let sample: Vec<String> = recs.iter().take(n).map(|r| r.to_string().chars().take(300).collect()).collect();
    let head = format!("[chrono · intermediario] {} registros ({} KB) en la salida de {tool}; resumen exacto:", recs.len(), text.len() / 1024);
    Ok(Some(summary(&rows, &head, Some(&source), &sample, &format!("primeros {} registros:", sample.len()), &dims_ref)))
}

// ── Read ──────────────────────────────────────────────────────────────────────

fn read_cfg(cfg: &Value) -> Option<&Value> {
    cfg.get("read").filter(|r| r.get("enabled").and_then(Value::as_bool).unwrap_or(true))
}

/// Fichero de datos (por extensión) con su tamaño, si la lectura es completa.
fn data_file(rc: &Value, input: &Value) -> Option<(PathBuf, u64)> {
    if input.get("offset").is_some_and(|v| !v.is_null()) || input.get("limit").is_some_and(|v| !v.is_null()) {
        return None; // lecturas parciales: nunca se interceptan
    }
    // Read trae `file_path`; Grep, `path` (que puede ser un directorio: entonces no aplica).
    let p = PathBuf::from(input.get("file_path").or_else(|| input.get("path"))?.as_str()?);
    let exts: Vec<String> = rc
        .get("extensions")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_else(|| [".log", ".jsonl", ".ndjson", ".csv", ".tsv"].iter().map(|s| s.to_string()).collect());
    let name = p.file_name()?.to_string_lossy().to_string();
    if !exts.iter().any(|e| name.ends_with(e.as_str())) {
        return None;
    }
    let size = std::fs::metadata(&p).ok()?.len();
    Some((p, size))
}

/// Indexa el fichero (si no lo estaba) y devuelve `(db, source)`.
fn index_file(root: &Path, path: &Path) -> (PathBuf, String) {
    let db = root.join(".chrono/index.db");
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let listed = if db.is_file() { chrono_q(&db, &["sources"]) } else { String::new() };
    if !listed.contains(&name) {
        let _ = Command::new(self_bin()).arg("add").arg(path).arg("--db").arg(&db).current_dir(root).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let src = chrono_q(&db, &["sources"]).lines().map(|l| l.split('\t').next().unwrap_or("").to_string()).find(|s| s.contains(&name)).unwrap_or_else(|| format!("jsonl:{}", path.display()));
    (db, src)
}

fn read_deny(cfg: &Value, input: &Value, root: &Path) -> R<Option<String>> {
    let Some(rc) = read_cfg(cfg) else { return Ok(None) };
    let Some((path, size)) = data_file(rc, input) else { return Ok(None) };
    if (size as i64) < num(rc, "deny_bytes", 5_000_000) {
        return Ok(None);
    }
    let (db, src) = index_file(root, &path);
    let lines = std::fs::read(&path).map(|b| b.iter().filter(|c| **c == b'\n').count()).unwrap_or(0);
    let levels = rc.get("levels").and_then(Value::as_str).unwrap_or("FATAL,CRITICAL,ERROR,WARNING");
    let parts = [
        format!(
            "[chrono · intermediario] {} pesa {} KB ({lines} líneas): leerlo entero cuesta ~{} tokens y no sirve para contar. Está indexado en chrono; resumen exacto:",
            path.display(),
            size / 1024,
            size / 4
        ),
        chrono_q(&db, &["sources"]),
        chrono_q(&db, &["top", "level", "--source", &src]),
        chrono_q(&db, &["top", "entity", "--source", &src, "--limit", "15"]),
        chrono_q(&db, &["patterns", "--source", &src, "--level", levels, "--limit", "10"]),
        format!("Para cifras, «qué precede a X» y leer en orden: chrono (top, patterns, precede, timeline, events) con source={src} y filtros level, entity, attr, grep, since/until. Para un trozo literal: Read con offset y limit (eso no se intercepta)."),
    ];
    Ok(Some(parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join("\n")))
}

fn after_read(cfg: &Value, input: &Value, root: &Path, session: &str) -> R<Option<String>> {
    let Some(rc) = read_cfg(cfg) else { return Ok(None) };
    let is_grep = input.get("pattern").is_some();
    let Some((path, size)) = data_file(rc, if is_grep { &Value::Null } else { input }).or_else(|| is_grep.then(|| data_file(rc, &json!({"file_path": input.get("path")}))).flatten()) else {
        return Ok(None);
    };
    // Tras un Grep el fichero puede ser de cualquier tamaño a partir de min_bytes
    // (nadie lo leyó entero); tras un Read, solo los medianos (los grandes se interceptan antes).
    if (size as i64) < num(rc, "min_bytes", 200_000) || (!is_grep && (size as i64) >= num(rc, "deny_bytes", 5_000_000)) {
        return Ok(None);
    }
    // Un aviso por fichero y sesión: repetirlo en cada Grep solo gastaría contexto.
    let mark = root.join(".chrono/mcp").join(format!("aviso-{:016x}", chrono_core_hash(&format!("{session}{}", path.display()))));
    if mark.exists() {
        return Ok(None);
    }
    let _ = std::fs::create_dir_all(root.join(".chrono/mcp"));
    let _ = std::fs::write(&mark, b"");
    let (db, src) = index_file(root, &path);
    let levels = chrono_q(&db, &["top", "level", "--source", &src]);
    let lv: Vec<String> = levels.lines().skip(3).map(|l| l.replace('\t', "=")).collect();
    Ok(Some(format!(
        "{} ({} KB) ya está indexado en chrono (source={src}; niveles: {}). Lo que acabas de leer/buscar es solo una parte: para contar, rankear, ver patrones o qué precede a algo, usa chrono (top, patterns, precede, timeline, events) en vez de recontar a mano; sus cifras son exactas sobre el fichero entero.",
        path.display(),
        size / 1024,
        lv.join(" ")
    )))
}

// ── Verificador de cifras (Stop) ──────────────────────────────────────────────

/// Al terminar, los conteos de la respuesta final que no salen de ninguna salida
/// de chrono de la sesión. Si hay, pide UNA vez que se comprueben o se marquen
/// como estimación (`decision: block` con el motivo; la segunda vez,
/// `stop_hook_active`, ya no bloquea). Solo actúa si la sesión usó chrono.
fn verify_numbers(ev: &Value) -> R<Option<String>> {
    // Opt-in (medido con Opus: al bloquear, el modelo responde al aviso y ESE mensaje queda como respuesta
    // final, tapando la buena; y los falsos positivos —un código HTTP, una suma de 4 cifras— lo disparan).
    // Se activa con CHRONO_VERIFY=1 o "verify": true en .chrono/intermediario.json.
    let cwd = ev.get("cwd").and_then(Value::as_str).map(PathBuf::from).unwrap_or_default();
    let by_config = find_root(&cwd)
        .and_then(|r| std::fs::read_to_string(r.join(".chrono/intermediario.json")).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|c| c.get("verify").and_then(Value::as_bool))
        .unwrap_or(false);
    let on = match std::env::var("CHRONO_VERIFY").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => by_config,
    };
    if !on || ev.get("stop_hook_active").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let Some(path) = ev.get("transcript_path").and_then(Value::as_str) else { return Ok(None) };
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (allowed, answer) = transcript_numbers(&text);
    if allowed.is_empty() || answer.is_empty() {
        return Ok(None);
    }
    let bad = unverified(&answer, &allowed);
    if bad.is_empty() {
        return Ok(None);
    }
    let list: Vec<String> = bad.iter().take(12).map(|n| n.to_string()).collect();
    let reason = format!(
        "[chrono · verificador] Estas cifras de tu respuesta no salen de ninguna salida de chrono de esta sesión: {}. Si alguna es un conteo sin comprobar, compruébala con chrono (informe, top, patterns) y VUELVE A ESCRIBIR LA RESPUESTA COMPLETA corregida. Si todas están bien (sumas, códigos, ids), vuelve a escribir la respuesta completa tal cual, sin comentar este aviso.",
        list.join(", ")
    );
    log(&format!("stop: {} cifras sin respaldo ({})", bad.len(), list.join(",")));
    Ok(Some(json!({"decision": "block", "reason": reason}).to_string()))
}

/// `(números con respaldo, texto de la respuesta final)` de un transcript. Con
/// respaldo: las salidas de chrono (MCP, CLI, intermediario) y las salidas
/// PEQUEÑAS de consultas MCP (agregados como `count_over_time`). De un
/// transcript (JSONL de Claude Code o `--output-format stream-json`).
fn transcript_numbers(text: &str) -> (std::collections::BTreeSet<i64>, String) {
    let mut used_chrono = false;
    let mut chrono_ids: std::collections::HashSet<String> = Default::default();
    let mut aggregate_ids: std::collections::HashSet<String> = Default::default();
    let mut allowed = std::collections::BTreeSet::new();
    let mut answer = String::new();
    for line in text.lines() {
        let Ok(e) = serde_json::from_str::<Value>(line) else { continue };
        let role = e.get("type").and_then(Value::as_str).unwrap_or("");
        let content = e.pointer("/message/content").cloned().unwrap_or(Value::Null);
        let Some(blocks) = content.as_array() else {
            if role == "user" && content.is_string() {
                answer.clear(); // una pregunta nueva: la respuesta que cuenta es la de después
            }
            continue;
        };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                    let cmd = b.pointer("/input/command").and_then(Value::as_str).unwrap_or("");
                    let id = b.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                    if name.contains("chrono") || (name == "Bash" && cmd.trim_start().starts_with("chrono")) {
                        chrono_ids.insert(id);
                    } else if name.starts_with("mcp__") && ["query", "stats", "count", "metric"].iter().any(|k| name.contains(k)) {
                        // Consultas que pueden devolver agregados exactos (count_over_time, stats…).
                        aggregate_ids.insert(id);
                    }
                }
                Some("tool_result") => {
                    let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                    let body = response_text(b.get("content"));
                    // Una salida pequeña de una consulta es un agregado (exacto); una grande son
                    // líneas crudas con miles de números que lo «validarían» todo.
                    if chrono_ids.contains(id) || body.contains("[chrono") {
                        used_chrono = true;
                    }
                    if chrono_ids.contains(id) || body.contains("[chrono") || (aggregate_ids.contains(id) && body.len() < 8_000) {
                        allowed.extend(numbers(&body));
                    }
                }
                Some("text") if role == "assistant" => {
                    answer = b.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                }
                Some("text") if role == "user" => answer.clear(),
                _ => {}
            }
        }
    }
    // Sin chrono en la sesión no hay nada contra lo que verificar.
    if !used_chrono {
        allowed.clear();
    }
    (allowed, answer)
}

/// Enteros sueltos de un texto (con separador de miles `.`/`,` o sin él).
fn numbers(s: &str) -> Vec<i64> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) {
            let start = i;
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_digit() || ((b[j] == b'.' || b[j] == b',') && j + 3 < b.len() && b[j + 1..j + 4].iter().all(u8::is_ascii_digit) && b.get(j + 4).map_or(true, |c| !c.is_ascii_digit()))) {
                j += 1;
            }
            let tok: String = s[start..j].chars().filter(char::is_ascii_digit).collect();
            if let Ok(n) = tok.parse::<i64>() {
                out.push(n);
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Conteos de la respuesta sin respaldo. No cuentan como conteo: fechas, horas,
/// versiones e IPs, porcentajes, puertos (`:5432`), `[pid]`, códigos HTTP junto a
/// su texto (`500 Internal`), números < 10, y lo que es suma o resta de dos
/// cifras de chrono (un total que el modelo sumó).
fn unverified(answer: &str, allowed: &std::collections::BTreeSet<i64>) -> Vec<i64> {
    let mut bad = std::collections::BTreeSet::new();
    for line in answer.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let counts: Vec<i64> = words
            .iter()
            .enumerate()
            .filter(|(k, w)| !is_not_a_count(w, words.get(k + 1).copied()) && !has_non_count_context(k.checked_sub(1).map(|i| words[i]), w, words.get(k + 1).copied()))
            .flat_map(|(_, w)| numbers(w))
            .collect();
        // Sumandos válidos: cifras de chrono que aparecen en esta misma línea.
        let here: Vec<i64> = counts.iter().copied().filter(|n| allowed.contains(n)).collect();
        for n in counts {
            // < 10: demasiado comunes; ≥ 10⁹: épocas e identificadores, no conteos.
            if !(10..1_000_000_000).contains(&n) || allowed.contains(&n) {
                continue;
            }
            let is_sum = here.iter().enumerate().any(|(i, a)| here.iter().skip(i + 1).any(|b| a + b == n));
            if !is_sum {
                bad.insert(n);
            }
        }
    }
    bad.into_iter().collect()
}

/// Unidades y prefijos que dicen que el número es una duración, un tamaño, una
/// posición o un código, no un conteo de eventos.
fn has_non_count_context(prev: Option<&str>, word: &str, next: Option<&str>) -> bool {
    let clean = |s: &str| s.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    const UNITS: &[&str] = &["s", "seg", "segs", "segundo", "segundos", "ms", "min", "mins", "minuto", "minutos", "h", "hora", "horas", "d", "día", "días", "kb", "mb", "gb", "kib", "mib", "b", "bytes", "tokens", "px", "chars", "caracteres"];
    const PREFIX: &[&str] = &["línea", "linea", "line", "l", "puerto", "port", "pid", "http", "código", "codigo", "status", "versión", "version", "v", "id", "#", "gap_secs", "window_secs", "bucket_secs", "limit"];
    // Unidad pegada: 52s, 120ms, 24h, 13KB.
    let w = word.trim_matches(|c: char| !c.is_alphanumeric());
    let tail: String = w.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ',').to_lowercase();
    if !tail.is_empty() && w.starts_with(|c: char| c.is_ascii_digit()) && UNITS.contains(&tail.as_str()) {
        return true;
    }
    if next.is_some_and(|n| UNITS.contains(&clean(n).as_str())) {
        return true;
    }
    prev.is_some_and(|p| PREFIX.contains(&clean(p).as_str()))
}

fn is_not_a_count(word: &str, next: Option<&str>) -> bool {
    let w = word.trim_matches(|c: char| "()[]{},;.:|*`\"'¿?¡!".contains(c));
    if !w.bytes().any(|c| c.is_ascii_digit()) {
        return true;
    }
    let bytes = w.as_bytes();
    let digit_at = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    // [128]: pid de una línea de log.
    if word.trim_matches(|c: char| ",;|*`".contains(c)).starts_with('[') && word.contains(']') {
        return true;
    }
    // Horas (19:08, 19:08:17.966) y puertos (:5432, host:8080).
    if let Some(p) = w.find(':') {
        if (p > 0 && digit_at(p - 1) && digit_at(p + 1)) || (p + 1 < bytes.len() && digit_at(p + 1) && w[p + 1..].bytes().all(|c| c.is_ascii_digit())) {
            return true;
        }
    }
    // Fechas 2026-10-14…
    if bytes.len() >= 10 && bytes[..4].iter().all(u8::is_ascii_digit) && bytes[4] == b'-' && digit_at(5) && bytes[7] == b'-' {
        return true;
    }
    // Versiones e IPs (2.30.0, 172.18.0.4).
    if w.matches('.').count() >= 2 && w.split('.').all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit())) {
        return true;
    }
    // Porcentajes.
    if w.ends_with('%') || next.is_some_and(|n| n.starts_with('%')) {
        return true;
    }
    // Códigos HTTP junto a su texto.
    if let Ok(code) = w.parse::<i64>() {
        if (100..600).contains(&code) && next.is_some_and(|n| ["OK", "Internal", "Not", "Bad", "Created", "Accepted", "Server", "Error", "Unauthorized", "Forbidden"].iter().any(|x| n.starts_with(x))) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verificador_marca_solo_conteos_sin_respaldo() {
        let allowed: std::collections::BTreeSet<i64> = [51, 11, 25, 28, 12, 8, 9, 6573].into_iter().collect();
        let ans = "FATAL 51 (postgres 51), ERROR 11; total graves 62 (51 + 11).\nA las 19:08:17 y el 2026-10-14T22:00:00Z; \
                   versión 2.30.0, HTTP 500 Internal, puerto :5432, [128].\nInventado: 79 líneas y 42 veces; 37%.";
        assert_eq!(unverified(ans, &allowed), vec![42, 79], "62 = 51 + 11 en la misma línea es una suma de cifras de chrono");
    }

    #[test]
    fn casa_herramientas_con_comodines() {
        assert!(tool_matches("mcp__grafana__query_loki_logs", "mcp__grafana__query_loki_logs"));
        assert!(tool_matches("mcp__*__query_loki*", "mcp__grafana-aura__query_loki_logs"));
        assert!(tool_matches("mcp__a__x | mcp__b__*", "mcp__b__y"));
        assert!(!tool_matches("mcp__grafana__query_loki_logs", "mcp__grafana__query_loki_stats"));
    }

    #[test]
    fn selector_indexado_sin_level_y_cobertura_por_union() {
        let drop = vec!["level".to_string()];
        assert_eq!(index_selector(r#"{service="x", level=~"ERROR|FATAL"}"#, &drop), r#"{service="x"}"#);
        assert_eq!(index_selector(r#"{level="ERROR"}"#, &drop), r#"{level="ERROR"}"#);
        assert_eq!(index_selector(r#"{service=~"a,b", container="c"}"#, &drop), r#"{service=~"a,b", container="c"}"#);
        let mut w = vec![(10, 20), (0, 12)];
        assert!(covered(&mut w, 2, 18));
        assert!(!covered(&mut w, 2, 25));
    }

    #[test]
    fn selector_de_logql() {
        assert_eq!(stream_selector(r#"{service=~"a|b"} |= "x" | json"#).as_deref(), Some(r#"{service=~"a|b"}"#));
        assert_eq!(stream_selector(r#"sum by (level) (count_over_time({app="x", n="{}"} [5m]))"#).as_deref(), Some(r#"{app="x", n="{}"}"#));
        assert!(is_metric("sum(count_over_time({a=\"b\"}[1m]))"));
        assert!(!is_metric("{a=\"b\"}"));
    }

    #[test]
    fn tiempos_rfc3339_y_relativos() {
        assert_eq!(to_ns(Some("2026-10-14T22:00:00Z"), "now").unwrap(), 1_792_015_200_000_000_000);
        assert_eq!(to_ns(Some("2026-10-14T17:00:00-05:00"), "now").unwrap(), 1_792_015_200_000_000_000);
        assert_eq!(to_ns(Some("2026-10-14T22:00:00.5Z"), "now").unwrap(), 1_792_015_200_500_000_000);
        let now = to_ns(None, "now").unwrap();
        let ago = to_ns(Some("now-1h"), "now").unwrap();
        assert!((now - ago - 3_600_000_000_000).abs() < 2_000_000_000);
    }

    #[test]
    fn truncado_por_metadatos() {
        let r = json!([{"type": "text", "text": "{\"data\":[],\"metadata\":{\"linesReturned\":100,\"maxLinesAllowed\":100}}"}]);
        assert!(looked_truncated(Some(&r), 100));
        let r = json!([{"type": "text", "text": "{\"data\":[],\"metadata\":{\"linesReturned\":7,\"maxLinesAllowed\":100,\"resultsTruncated\":false}}"}]);
        assert!(!looked_truncated(Some(&r), 100));
    }
}
