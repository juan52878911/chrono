//! Mapeo de un evento Nod v1 (dentro de su sobre) a un `chrono_core::Event`.
//! Única implementación del mapeo; la usan el cursor y los tests.
//!
//! Criterios (ver `docs/NOD.md` en la raíz del repo):
//! - `id` = `nod:<instance>:<run>:<seq>`: la identidad del contrato (§1.3). El
//!   store inserta con `INSERT OR IGNORE`, así que un evento repetido (entrega
//!   "al menos una vez", el mismo sobre dos veces) se descarta solo.
//! - `kind` = `nod.<type>` (prefijo para no chocar con `kind` de otras fuentes:
//!   `usage` o `command` son palabras genéricas).
//! - actor: `name` = `source.profile`, `key` = `source.instance`.
//! - `title`: estructurado y SIN texto libre del usuario (ni prompts, ni
//!   respuestas, ni `reason`): el clasificador de fixes y la búsqueda no deben
//!   tomar un prompt "arregla X" por un fix del repo, y así lo que la
//!   privacidad deja salir no se copia a más sitios.
//! - `attrs`: campos comunes + los escalares de `data` aplanados con `.`
//!   (`usage.input`, `repo.branch`…), sin los campos de texto libre.
//! - touches: `tool/<Tool>` en `tool.call`, y con privacidad `completo` el
//!   fichero que escribe Write/Edit/MultiEdit/NotebookEdit de Claude Code o
//!   `write`/`edit` de OpenCode (`edit:<ruta>`).
//! - links: `vcs.commit` → `{rel: "commit", target: sha}` y, si lleva tarea,
//!   `{rel: "ticket", target: task}` (también en `task.set`); `tool.result` →
//!   lo que produjo (`data.links`): `{rel: "url", target: url}` para PRs,
//!   issues y artifacts, `{rel: "file", target: ruta}` para ficheros. Así
//!   `chrono trace` une el `vcs.commit` con su commit de git y las sesiones
//!   que produjeron el mismo PR o documento.

use crate::simhash::{epoch_to_iso8601, simhash, tokenize};
use chrono_core::{Actor, Event, Link, Touch};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Tipos de evento de Nod v1 (§3). Cualquier otro se ignora (§3, §6).
pub const KNOWN_TYPES: &[&str] = &[
    "session.start",
    "session.end",
    "prompt.submit",
    "turn.start",
    "turn.step",
    "turn.end",
    "tool.call",
    "tool.result",
    "permission.request",
    "permission.decision",
    "agent.spawn",
    "agent.end",
    "usage",
    "command",
    "task.set",
    "vcs.commit",
];

/// Tipos que emite delega (`source.host == "delega"`) en el mismo bus: sus
/// trabajos y las decisiones que piden al humano. Solo se aceptan de ese host.
pub const DELEGA_TYPES: &[&str] = &[
    "job.launch",
    "job.notice",
    "job.alert",
    "job.result",
    "job.end",
    "job.say",
    "job.message",
    "decision.request",
    "decision.answer",
];

/// Campos de `data` que son texto libre (o estructuras grandes): no van a
/// `attrs`. `input` se usa solo para sacar la ruta del fichero editado.
/// `links` (lo que produjo un `tool.result`) va a `Event.links`, no a `attrs`.
const SKIP_DATA_KEYS: &[&str] = &["text", "answer", "input", "reason", "description", "title", "limits", "links"];

/// Texto libre que además puede traer un evento de delega (el resumen entra recortado en el título).
const SKIP_DELEGA_KEYS: &[&str] = &["summary", "question", "message", "brief", "result"];

/// Tools que escriben un fichero: las de Claude Code (Write, Edit…) y las de
/// OpenCode (`write`, `edit`, en minúsculas).
const EDIT_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit", "write", "edit"];

/// Campos de la entrada de una tool de edición con la ruta del fichero:
/// `file_path`/`notebook_path` en Claude Code, `filePath` en OpenCode.
const EDIT_PATH_FIELDS: &[&str] = &["file_path", "notebook_path", "filePath"];

/// Campos del sobre que el cursor necesita para mapear cada evento.
#[derive(Debug, Clone, Default)]
pub struct EnvelopeSource {
    pub instance: String,
    pub run: String,
    pub host: String,
    pub host_version: String,
    pub machine: String,
    pub profile: String,
    pub privacy: String,
}

impl EnvelopeSource {
    /// Lee `source` de un sobre. `None` si falta algún campo obligatorio (§2)
    /// o el sobre no es `nod: 1`: ese sobre no se ingiere.
    pub fn from_envelope(env: &Map<String, Value>) -> Option<Self> {
        if env.get("nod").and_then(Value::as_i64) != Some(1) {
            return None;
        }
        let src = env.get("source")?.as_object()?;
        let s = |k: &str| src.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let out = EnvelopeSource {
            instance: s("instance"),
            run: s("run"),
            host: s("host"),
            host_version: s("host_version"),
            machine: s("machine"),
            profile: s("profile"),
            privacy: s("privacy"),
        };
        if out.instance.is_empty() || out.run.is_empty() || out.profile.is_empty() {
            return None;
        }
        Some(out)
    }
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Aplana `data` en `attrs`: escalares tal cual; objetos con `.` (dos niveles);
/// arrays de escalares unidos por `,` (p.ej. `tools`). Si la clave ya la puso
/// un campo común (p.ej. `agent`, `task`), va como `data.<clave>`.
fn flatten_data(data: &Map<String, Value>, attrs: &mut BTreeMap<String, String>) {
    for (k, v) in data {
        if SKIP_DATA_KEYS.contains(&k.as_str()) {
            continue;
        }
        let key = if attrs.contains_key(k) { format!("data.{k}") } else { k.clone() };
        match v {
            Value::Object(inner) => {
                // Los objetos anidados del contrato (usage, repo, edit, context)
                // no traen texto libre: se vuelcan enteros (`usage.input` sí).
                for (k2, v2) in inner {
                    if let Some(s) = scalar(v2) {
                        attrs.insert(format!("{key}.{k2}"), s);
                    }
                }
            }
            Value::Array(items) => {
                let parts: Vec<String> = items.iter().filter_map(scalar).collect();
                if parts.len() == items.len() {
                    attrs.insert(key, parts.join(","));
                }
            }
            Value::Null => {
                attrs.insert(key, String::new());
            }
            other => {
                if let Some(s) = scalar(other) {
                    attrs.insert(key, s);
                }
            }
        }
    }
}

/// Primeros 12 caracteres (un sha abreviado legible en el título).
fn short(s: &str) -> &str {
    match s.char_indices().nth(12) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Título estructurado, sin texto libre del usuario.
fn title_for(ty: &str, data: &Map<String, Value>) -> String {
    let d = |k: &str| data.get(k).and_then(scalar).unwrap_or_default();
    match ty {
        "tool.call" | "tool.result" | "permission.request" => format!("{ty} {}", d("tool")),
        "permission.decision" => format!("{ty} {} {} by {}", d("tool"), d("decision"), d("by")),
        "turn.step" => {
            let model = data.get("usage").and_then(|u| u.get("model")).and_then(Value::as_str).unwrap_or("");
            let model = if model.is_empty() { d("model") } else { model.to_string() };
            format!("{ty} {model}").trim_end().to_string()
        }
        "turn.end" => format!("{ty} {}", d("outcome")),
        "prompt.submit" => format!("{ty} {}", d("origin")),
        "session.end" => format!("{ty} {}", d("reason")),
        "agent.spawn" | "agent.end" => format!("{ty} {}", d("type")).trim_end().to_string(),
        "command" => format!("{ty} /{}", d("name")),
        "task.set" => format!("{ty} {}", d("task")).trim_end().to_string(),
        "vcs.commit" => {
            let branch = d("branch");
            if branch.is_empty() {
                format!("{ty} {}", short(&d("sha")))
            } else {
                format!("{ty} {} ({branch})", short(&d("sha")))
            }
        }
        _ => ty.to_string(),
    }
}

/// Título de un evento de delega: `<tipo> <trabajo>` y, si lo trae, el resumen corto
/// que delega pone a propósito en `data.summary` (primera línea, 160 caracteres).
fn delega_title(ty: &str, job: Option<&str>, data: &Map<String, Value>) -> String {
    let mut t = format!("{ty} {}", job.unwrap_or("")).trim_end().to_string();
    for k in ["kind", "alert", "notice"] {
        if let Some(v) = data.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()) {
            t = format!("{t} {v}");
            break;
        }
    }
    if let Some(s) = data.get("summary").and_then(Value::as_str) {
        let line: String = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(160).collect();
        if !line.is_empty() {
            t = format!("{t}: {line}");
        }
    }
    t
}

/// Con la opción `include_command` de la fuente (`.chrono/config.json`, `source_options` del fichero de
/// Nod), el título de un `tool.call` lleva el comando que ejecutó: `tool.call Bash: git log …`. Así
/// `correlate` o `events --grep` distinguen un `slice-ops.sh gate` de un `wiring update` alrededor de un
/// rechazo. Está apagada por defecto, porque el título no lleva texto libre del usuario, y solo actúa con
/// privacidad `completo` (sin ella Nod no envía el `input`). Se recorta a la primera línea y 160
/// caracteres: es determinista.
pub fn with_command(mut ev: Event, src: &EnvelopeSource, raw: &Map<String, Value>) -> Event {
    if src.privacy != "completo" || ev.kind != "nod.tool.call" {
        return ev;
    }
    let cmd = raw
        .get("data")
        .and_then(|d| d.get("input"))
        .and_then(|i| i.get("command"))
        .and_then(Value::as_str)
        .map(|c| c.lines().next().unwrap_or("").trim())
        .filter(|c| !c.is_empty());
    if let Some(c) = cmd {
        let c: String = c.chars().take(160).collect();
        ev.title = format!("{}: {c}", ev.title);
    }
    ev
}

/// Severidad derivada (no es nativa de Nod; sirve a `timeline --by level`):
/// ERROR = tool falló (no negada); WARN = permiso negado, tool negada o turno
/// abortado; INFO el resto.
fn level_for(ty: &str, data: &Map<String, Value>) -> &'static str {
    let b = |k: &str| data.get(k).and_then(Value::as_bool);
    match ty {
        "tool.result" if b("rejected") == Some(true) => "WARN",
        "tool.result" if b("ok") == Some(false) => "ERROR",
        "permission.decision" if data.get("decision").and_then(Value::as_str) == Some("deny") => "WARN",
        "turn.end" if data.get("outcome").and_then(Value::as_str) == Some("aborted") => "WARN",
        _ if DELEGA_TYPES.contains(&ty) => match data.get("level").and_then(Value::as_str).map(str::to_uppercase).as_deref() {
            Some("ERROR") => "ERROR",
            Some("WARN" | "WARNING") => "WARN",
            Some("INFO") => "INFO",
            _ if matches!(ty, "job.alert" | "decision.request") => "WARN",
            _ if ty == "job.end" && data.get("rc").and_then(Value::as_i64).is_some_and(|r| r != 0) => "WARN",
            _ => "INFO",
        },
        _ => "INFO",
    }
}

/// Convierte un evento Nod (objeto de `events[]`) en un `Event` de chrono, o
/// `None` si el tipo es desconocido o le faltan campos obligatorios (`seq`,
/// `ts`, `type`, `session`).
pub fn to_event(src: &EnvelopeSource, ev: &Map<String, Value>, source_id: &str) -> Option<Event> {
    let seq = ev.get("seq").and_then(Value::as_u64).filter(|s| *s >= 1)?;
    let ts_ms = ev.get("ts").and_then(Value::as_i64).filter(|t| *t >= 0)?;
    let ty = ev.get("type").and_then(Value::as_str)?;
    let delega = src.host == "delega" && DELEGA_TYPES.contains(&ty);
    if !KNOWN_TYPES.contains(&ty) && !delega {
        return None;
    }
    let empty = Map::new();
    let data = ev.get("data").and_then(Value::as_object).unwrap_or(&empty);
    // delega: el trabajo va en la raíz o en `data`; sin sesión, la sesión es el trabajo.
    let job = delega
        .then(|| ev.get("job").or_else(|| data.get("job")).and_then(Value::as_str).filter(|j| !j.is_empty()))
        .flatten();
    let session = match ev.get("session").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None if delega => job.unwrap_or("delega"),
        None => return None,
    };

    let mut attrs: BTreeMap<String, String> = BTreeMap::new();
    attrs.insert("nod.type".into(), ty.into());
    attrs.insert("instance".into(), src.instance.clone());
    attrs.insert("run".into(), src.run.clone());
    attrs.insert("seq".into(), seq.to_string());
    attrs.insert("session".into(), session.into());
    attrs.insert("profile".into(), src.profile.clone());
    attrs.insert("host".into(), src.host.clone());
    attrs.insert("privacy".into(), src.privacy.clone());
    if !src.host_version.is_empty() {
        attrs.insert("host_version".into(), src.host_version.clone());
    }
    if !src.machine.is_empty() {
        attrs.insert("machine".into(), src.machine.clone());
    }
    for k in ["turn", "agent", "task"] {
        if let Some(v) = ev.get(k).and_then(Value::as_str) {
            attrs.insert(k.into(), v.into());
        }
    }
    if let Some(j) = job {
        attrs.insert("job".into(), j.into());
    }
    if delega {
        let kept: Map<String, Value> = data.iter().filter(|(k, _)| !SKIP_DELEGA_KEYS.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
        flatten_data(&kept, &mut attrs);
    } else {
        flatten_data(data, &mut attrs);
    }

    let mut touches = Vec::new();
    if let Some(j) = job {
        touches.push(Touch { entity: j.to_string(), entity_type: "job".into(), weight: 1, attrs: BTreeMap::new() });
    }
    if ty == "tool.call" {
        if let Some(tool) = data.get("tool").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            touches.push(Touch { entity: format!("tool/{tool}"), entity_type: "tool".into(), weight: 1, attrs: BTreeMap::new() });
            if src.privacy == "completo" && EDIT_TOOLS.contains(&tool) {
                let input = data.get("input");
                let path = EDIT_PATH_FIELDS
                    .iter()
                    .find_map(|f| input.and_then(|i| i.get(*f)).and_then(Value::as_str).filter(|p| !p.is_empty()))
                    .unwrap_or("");
                if !path.is_empty() {
                    let mut ta = BTreeMap::new();
                    ta.insert("path".to_string(), path.to_string());
                    ta.insert("tool".to_string(), tool.to_string());
                    touches.push(Touch {
                        entity: format!("edit:{path}"),
                        entity_type: "edited-file".into(),
                        weight: 1,
                        attrs: ta,
                    });
                }
            }
        }
    }

    let task = ev.get("task").and_then(Value::as_str).filter(|t| !t.is_empty());
    let mut links = Vec::new();
    match ty {
        "vcs.commit" => {
            if let Some(sha) = data.get("sha").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                links.push(Link { rel: "commit".into(), target: sha.to_string() });
            }
            if let Some(t) = task {
                links.push(Link { rel: "ticket".into(), target: t.to_string() });
            }
        }
        "task.set" => {
            if let Some(t) = data.get("task").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                links.push(Link { rel: "ticket".into(), target: t.to_string() });
            }
        }
        "tool.result" => {
            // Lo que produjo la tool (§3, opc.; solo con privacidad `completo`).
            for l in data.get("links").and_then(Value::as_array).into_iter().flatten() {
                let field = |k: &str| l.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
                let link = match l.get("kind").and_then(Value::as_str) {
                    Some("link") => field("url").map(|u| Link { rel: "url".into(), target: u.to_string() }),
                    Some("file") => field("path").map(|p| Link { rel: "file".into(), target: p.to_string() }),
                    _ => None,
                };
                if let Some(link) = link {
                    if !links.contains(&link) {
                        links.push(link);
                    }
                }
            }
        }
        _ => {}
    }

    let title = if delega { delega_title(ty, job, data) } else { title_for(ty, data) };
    // `ts` de Nod ya es ms Unix: es `at_epoch` tal cual (ms en todo el dominio).
    Some(Event {
        id: format!("nod:{}:{}:{seq}", src.instance, src.run),
        source_id: source_id.to_string(),
        kind: format!("nod.{ty}"),
        at: epoch_to_iso8601(ts_ms),
        at_epoch: ts_ms,
        actor: Actor { name: src.profile.clone(), key: src.instance.clone() },
        simhash: simhash(&tokenize(&title)),
        title,
        body: String::new(),
        level: level_for(ty, data).to_string(),
        attrs,
        touches,
        links,
        is_bulk: false,
    })
}
