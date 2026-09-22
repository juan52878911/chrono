//! Branches: estado de las ramas locales respecto a una base, leyendo git EN
//! VIVO (no el índice, que solo conoce HEAD). Paridad con `Branches` en
//! `internal/metrics/branches.go`.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Result;

/// Sin commits en este tiempo, la rama se marca como stale.
const STALE_DAYS: i64 = 90;

/// Separador de campos de `for-each-ref --format`: no puede aparecer en un
/// nombre de rama, autor o asunto.
const SEP: char = '\u{1f}';

/// El commit en la punta de una rama.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchTip {
    pub sha: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

/// Autor y nº de commits en los commits exclusivos de la rama.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchAuthor {
    pub name: String,
    pub commits: i64,
}

/// Estado de una rama respecto a la base.
#[derive(Debug, Clone, PartialEq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    pub tip: BranchTip,
    pub age_days: i64,
    /// Commits en la rama que no están en la base.
    pub ahead: i64,
    /// Commits en la base que no están en la rama.
    pub behind: i64,
    /// Toda la rama está ya en la base.
    pub merged: bool,
    /// Sin actividad en `STALE_DAYS` días.
    pub stale: bool,
    pub authors: Vec<BranchAuthor>,
    pub bus_factor: i64,
}

/// Estado de las ramas locales respecto a una base. Si `base` está vacía,
/// elige main, luego master, luego la rama actual. Devuelve `(base, current,
/// ramas)`: la actual primero, luego por fecha de punta descendente (orden
/// estable, igual que el Go).
pub fn branches(repo: &Path, base: &str) -> Result<(String, String, Vec<Branch>)> {
    let current = git_line(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let base = if base.is_empty() { pick_base(repo, &current) } else { base.to_string() };

    let fmt = format!(
        "%(refname:short){SEP}%(objectname:short){SEP}%(committerdate:iso8601){SEP}\
         %(committerdate:unix){SEP}%(authorname){SEP}%(contents:subject)"
    );
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["for-each-ref", &format!("--format={fmt}"), "refs/heads"])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git for-each-ref failed: {stderr}").into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);

    let mut out_branches = Vec::new();
    for line in text.trim_end_matches('\n').split('\n') {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(SEP).collect();
        if parts.len() < 6 {
            continue;
        }
        let name = parts[0].to_string();
        let commit_epoch: i64 = parts[3].trim().parse().unwrap_or(0);
        let age_days = ((now - commit_epoch) / 86_400).max(0);

        let mut b = Branch {
            current: name == current,
            tip: BranchTip {
                sha: parts[1].to_string(),
                date: parts[2].to_string(),
                author: parts[4].to_string(),
                subject: parts[5].to_string(),
            },
            age_days,
            ahead: 0,
            behind: 0,
            merged: false,
            stale: age_days >= STALE_DAYS,
            authors: Vec::new(),
            bus_factor: 0,
            name,
        };

        if b.name != base {
            if let Some(lr) =
                git_line(repo, &["rev-list", "--left-right", "--count", &format!("{base}...{}", b.name)])
            {
                let fields: Vec<&str> = lr.split_whitespace().collect();
                if fields.len() == 2 {
                    b.behind = fields[0].parse().unwrap_or(0);
                    b.ahead = fields[1].parse().unwrap_or(0);
                }
            }
            b.merged = b.ahead == 0;
            if b.ahead > 0 {
                let (authors, bus_factor) = branch_authors(repo, &base, &b.name);
                b.authors = authors;
                b.bus_factor = bus_factor;
            }
        } else {
            // La base está mergeada consigo misma por definición.
            b.merged = true;
        }
        out_branches.push(b);
    }

    // Orden estable: la actual primero; luego por fecha de punta descendente.
    out_branches.sort_by(|a, bb| match (a.current, bb.current) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => bb.tip.date.cmp(&a.tip.date),
    });

    Ok((base, current, out_branches))
}

/// Elige la rama base: main, master, o la actual como último recurso.
fn pick_base(repo: &Path, current: &str) -> String {
    for cand in ["main", "master"] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", "-q", &format!("refs/heads/{cand}")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return cand.to_string();
        }
    }
    current.to_string()
}

/// Autores de los commits exclusivos de la rama (`base..name`) y su bus
/// factor: nº mínimo de autores (en orden de `shortlog -sn`, por commits
/// desc) que acumulan más del 50% de esos commits.
fn branch_authors(repo: &Path, base: &str, name: &str) -> (Vec<BranchAuthor>, i64) {
    let out = match Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["shortlog", "-sn", "--no-merges", &format!("{base}..{name}")])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return (Vec::new(), 0),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut authors = Vec::new();
    let mut total = 0i64;
    for line in text.trim_end_matches('\n').split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((count, author_name)) = line.split_once('\t') else { continue };
        let n: i64 = count.trim().parse().unwrap_or(0);
        authors.push(BranchAuthor { name: author_name.to_string(), commits: n });
        total += n;
    }
    let mut bus = 0i64;
    let mut acc = 0i64;
    for a in &authors {
        bus += 1;
        acc += a.commits;
        if total > 0 && acc * 2 > total {
            break;
        }
    }
    (authors, bus)
}

/// Ejecuta git y devuelve la primera línea recortada, o `None` si falla o
/// sale vacía.
fn git_line(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
