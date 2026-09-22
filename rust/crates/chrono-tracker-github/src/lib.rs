//! `chrono-tracker-github` — adaptador Tracker de GitHub (PRs/issues vía `gh`).
//!
//! Paridad con el Go `internal/tracker/github.go`: `check` distingue
//! `NoGh`/`NoRemote`/`NoAuth`, `slug` prefiere `upstream` y cae a `origin`
//! (exige `github.com`), `fetch` pide `gh pr list`/`gh issue list` con
//! `--json number,title,state,mergedAt|closedAt,labels` (sin `body` ni
//! `commits`: eso revienta el límite de nodos GraphQL en repos grandes) y
//! tiene un timeout de ~90s por llamada.
//!
//! Diferencia deliberada frente al Go: allí un fallo de `gh issue list` se
//! tragaba en silencio (issues deshabilitados no es fatal). Aquí `fetch`
//! SURFACEA cualquier error de `gh` (PR o issue): decidir si es fatal o no
//! es cosa del llamador, no de este adaptador.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Error de este crate: envuelve I/O, `gh` y parseo sin exigir un tipo
/// concreto a los llamadores.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Timeout por llamada a `gh` (réplica del `context.WithTimeout` del Go).
const GH_TIMEOUT: Duration = Duration::from_secs(90);

/// Estado del forge para este repo: explica por qué está (o no) disponible,
/// para poder avisar al usuario con precisión en vez de omitirlo en silencio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `gh` instalado, autenticado y con remoto GitHub.
    Ok,
    /// `gh` no está en el PATH.
    NoGh,
    /// No hay remoto GitHub (`upstream`/`origin`).
    NoRemote,
    /// `gh` está pero no autenticado.
    NoAuth,
}

/// Devuelve el estado del forge para `repo`.
pub fn check(repo: &Path) -> Status {
    if !gh_in_path() {
        return Status::NoGh;
    }
    if slug(repo).is_none() {
        return Status::NoRemote;
    }
    match Command::new("gh")
        .args(["auth", "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => Status::Ok,
        _ => Status::NoAuth,
    }
}

/// `true` si `gh` se puede invocar (está en el PATH).
fn gh_in_path() -> bool {
    Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Elige el repo GitHub a consultar: prefiere `upstream` (flujo de fork,
/// donde viven los PRs) y si no, `origin`. Devuelve `owner/repo` solo si el
/// remoto apunta a github.com.
pub fn slug(repo: &Path) -> Option<String> {
    for remote in ["upstream", "origin"] {
        let out = match Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["remote", "get-url", remote])
            .output()
        {
            Ok(out) if out.status.success() => out,
            _ => continue,
        };
        let url = String::from_utf8_lossy(&out.stdout).to_string();
        if url.contains("github.com") {
            return Some(owner_repo(&url));
        }
    }
    None
}

fn owner_repo(remote_url: &str) -> String {
    let mut u = remote_url.trim().to_string();
    if let Some(stripped) = u.strip_suffix(".git") {
        u = stripped.to_string();
    }
    if let Some(i) = u.find("github.com") {
        u = u[i + "github.com".len()..].to_string();
        u = u.trim_start_matches([':', '/']).to_string();
    }
    u
}

/// Un PR o un issue normalizado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub number: i64,
    pub kind: String,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub merged: bool,
    pub closed_at: String,
}

/// Fila cruda de `gh --json`. `merged_at`/`closed_at` faltan según el
/// subcomando pedido (pr vs issue): `#[serde(default)]` los deja en "".
#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GhItem {
    number: i64,
    title: String,
    state: String,
    // gh emite `null` explícito (no ausente) para un PR sin mergear o un issue
    // abierto; con `String` serde falla. `Option` acepta null y ausente.
    #[serde(default, rename = "mergedAt")]
    merged_at: Option<String>,
    #[serde(default, rename = "closedAt")]
    closed_at: Option<String>,
    #[serde(default)]
    labels: Vec<GhLabel>,
}

/// Trae PRs e issues (todos los estados) hasta `limit` de cada tipo.
/// Devuelve los items y el slug (`owner/repo`) consultado.
pub fn fetch(repo: &Path, limit: usize) -> Result<(Vec<Item>, String)> {
    let nwo = slug(repo).ok_or("no hay remoto GitHub")?;

    let mut items = Vec::new();

    let pr_out = run_gh_json(&nwo, "pr", limit, "number,title,state,mergedAt,labels")?;
    items.extend(parse_items(&pr_out, "pr")?);

    let issue_out = run_gh_json(&nwo, "issue", limit, "number,title,state,closedAt,labels")?;
    items.extend(parse_items(&issue_out, "issue")?);

    Ok((items, nwo))
}

fn parse_items(json_bytes: &[u8], kind: &str) -> Result<Vec<Item>> {
    let raw: Vec<GhItem> = serde_json::from_slice(json_bytes)
        .map_err(|e| format!("parseando salida de `gh {kind} list`: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|x| Item {
            number: x.number,
            kind: kind.to_string(),
            title: x.title,
            state: x.state,
            merged: x.merged_at.as_deref().is_some_and(|s| !s.is_empty()),
            // Réplica del Go: concatena closedAt+mergedAt (el que no se pidió, o
            // el null de gh, llega como None → "").
            closed_at: format!(
                "{}{}",
                x.closed_at.as_deref().unwrap_or(""),
                x.merged_at.as_deref().unwrap_or("")
            ),
            labels: x.labels.into_iter().map(|l| l.name).collect(),
        })
        .collect())
}

/// Ejecuta `gh <kind> list -R <nwo> --state all --limit <limit> --json <fields>`
/// con un timeout de `GH_TIMEOUT`. Drena stdout/stderr en hilos aparte
/// mientras se espera, para no bloquear al hijo si llena el pipe.
fn run_gh_json(nwo: &str, kind: &str, limit: usize, fields: &str) -> Result<Vec<u8>> {
    let mut child = Command::new("gh")
        .arg(kind)
        .arg("list")
        .args(["-R", nwo])
        .args(["--state", "all"])
        .args(["--limit", &limit.to_string()])
        .args(["--json", fields])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("gh {kind} list: no se pudo ejecutar `gh`: {e}"))?;

    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    let stdout_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= GH_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    return Err(format!(
                        "gh {kind} list: timeout ({}s) consultando {kind}s",
                        GH_TIMEOUT.as_secs()
                    )
                    .into());
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("gh {kind} list: error esperando el proceso: {e}").into()),
        }
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();

    if !status.success() {
        let msg = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(format!("gh {kind} list: {msg}").into());
    }
    Ok(stdout)
}

/// Upsert de `items` en la tabla `issues`. `is_bug=1` si alguna label del
/// item está en `bug_labels` (comparación exacta, como el Go).
pub fn store_issues(store: &chrono_store::Store, items: &[Item], bug_labels: &[String]) -> Result<()> {
    let conn = store.conn();
    for it in items {
        let is_bug = it.labels.iter().any(|l| bug_labels.iter().any(|b| b == l));
        let labels_csv = it.labels.join(",");
        conn.execute(
            "INSERT INTO issues(number, kind, title, state, labels, merged, closed_at, is_bug)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(kind, number) DO UPDATE SET
                title     = excluded.title,
                state     = excluded.state,
                labels    = excluded.labels,
                merged    = excluded.merged,
                closed_at = excluded.closed_at,
                is_bug    = excluded.is_bug",
            rusqlite::params![
                it.number,
                it.kind,
                it.title,
                it.state,
                labels_csv,
                it.merged as i64,
                it.closed_at,
                is_bug as i64,
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
