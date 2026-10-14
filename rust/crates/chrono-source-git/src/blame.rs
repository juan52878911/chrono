//! `git blame` de un rango de un fichero en una REVISIÓN fija, parseado del
//! formato `--porcelain`. Base de `chrono explain`.
//!
//! Determinismo: se pide la revisión que tiene el índice (no el árbol de
//! trabajo), y se neutralizan las opciones de configuración del usuario que
//! cambiarían la atribución (`blame.ignoreRevsFile`, marcas de líneas
//! ignoradas/no atribuibles). Misma revisión + mismo fichero + mismo rango =
//! misma respuesta, byte a byte.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::Result;

/// Un commit que escribió líneas del rango.
#[derive(Debug, Clone, PartialEq)]
pub struct BlameCommit {
    pub sha: String,
    pub author: String,
    /// `author-time` (segundos UTC).
    pub author_time: i64,
    pub summary: String,
    /// Commit frontera (`boundary`): la historia disponible empieza en él.
    pub boundary: bool,
    /// Líneas del rango que atribuye a este commit.
    pub lines: u32,
}

/// Resultado de un blame: el rango efectivo (acotado al fichero) y sus commits
/// en orden de primera aparición en el rango.
#[derive(Debug, Clone, PartialEq)]
pub struct Blame {
    pub from: u32,
    pub to: u32,
    pub total_lines: u32,
    pub commits: Vec<BlameCommit>,
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        // Configuración del usuario que alteraría la atribución: fuera.
        .args(["-c", "blame.ignoreRevsFile=", "-c", "blame.markIgnoredLines=false", "-c", "blame.markUnblamableLines=false"])
        .args(args)
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git {}: {stderr}", args.join(" ")).into());
    }
    Ok(out.stdout)
}

/// Número de líneas del fichero en `rev` (0 si está vacío).
pub fn file_lines(repo: &Path, rev: &str, file: &str) -> Result<u32> {
    let blob = git(repo, &["cat-file", "-p", &format!("{rev}:{file}")])?;
    if blob.is_empty() {
        return Ok(0);
    }
    let n = blob.iter().filter(|&&b| b == b'\n').count();
    Ok((n + usize::from(*blob.last().unwrap() != b'\n')) as u32)
}

/// Blame de `file` en `rev`, líneas `range` (1-based, inclusivas; `None` = todo
/// el fichero). El rango se acota al tamaño real; uno que empieza fuera del
/// fichero es un error.
pub fn blame(repo: &Path, rev: &str, file: &str, range: Option<(u32, u32)>) -> Result<Blame> {
    let total = file_lines(repo, rev, file)?;
    if total == 0 {
        return Ok(Blame { from: 0, to: 0, total_lines: 0, commits: Vec::new() });
    }
    let (from, to) = match range {
        None => (1, total),
        Some((a, b)) => {
            let (a, b) = (a.max(1), b.max(a.max(1)));
            if a > total {
                return Err(format!("{file} has {total} lines; the range starts at {a}").into());
            }
            (a, b.min(total))
        }
    };
    let out = git(repo, &["blame", "--porcelain", "-L", &format!("{from},{to}"), rev, "--", file])?;
    Ok(Blame { from, to, total_lines: total, commits: parse_porcelain(&String::from_utf8_lossy(&out)) })
}

/// Parsea `git blame --porcelain`. Cada línea del fichero va precedida de una
/// cabecera `<sha> <orig> <final> [<n>]`; la PRIMERA vez que aparece un commit
/// le siguen sus metadatos (`author`, `author-time`, `summary`, `boundary`…).
pub fn parse_porcelain(text: &str) -> Vec<BlameCommit> {
    let mut order: Vec<String> = Vec::new();
    let mut by_sha: BTreeMap<String, BlameCommit> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.starts_with('\t') {
            // La línea de código: cuenta para el commit de la cabecera previa.
            if let Some(sha) = &current {
                if let Some(c) = by_sha.get_mut(sha) {
                    c.lines += 1;
                }
            }
            continue;
        }
        let mut parts = line.splitn(2, ' ');
        let head = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        if head.len() == 40 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
            current = Some(head.to_string());
            if !by_sha.contains_key(head) {
                order.push(head.to_string());
                by_sha.insert(
                    head.to_string(),
                    BlameCommit { sha: head.to_string(), author: String::new(), author_time: 0, summary: String::new(), boundary: false, lines: 0 },
                );
            }
            continue;
        }
        let Some(sha) = &current else { continue };
        let Some(c) = by_sha.get_mut(sha) else { continue };
        match head {
            "author" => c.author = rest.to_string(),
            "author-time" => c.author_time = rest.trim().parse().unwrap_or(0),
            "summary" => c.summary = rest.to_string(),
            "boundary" => c.boundary = true,
            _ => {}
        }
    }
    order.into_iter().filter_map(|s| by_sha.remove(&s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORCELAIN: &str = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 10 1 2
author Ana
author-time 1700000000
summary feat: proxy
boundary
filename src/main.py
\tdef proxy():
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 11 2
\t    pass
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 5 3 1
author Luis
author-time 1710000000
summary fix(gateway): timeout
filename src/main.py
\t    return 1
";

    #[test]
    fn parsea_commits_lineas_y_metadatos() {
        let c = parse_porcelain(PORCELAIN);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].author.as_str(), c[0].lines, c[0].boundary), ("Ana", 2, true));
        assert_eq!(c[0].summary, "feat: proxy");
        assert_eq!((c[1].author.as_str(), c[1].lines, c[1].author_time), ("Luis", 1, 1710000000));
    }

    #[test]
    fn blame_real_acota_el_rango_y_es_determinista() {
        let dir = std::env::temp_dir().join(format!("chrono-blame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success(), "{args:?}");
        };
        g(&["init", "-q"]);
        g(&["config", "user.email", "a@a"]);
        g(&["config", "user.name", "Ana"]);
        std::fs::write(dir.join("f.txt"), "uno\ndos\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "primero"]);
        std::fs::write(dir.join("f.txt"), "uno\ndos\ntres\n").unwrap();
        g(&["commit", "-q", "-am", "fix: tres"]);
        let head = String::from_utf8(Command::new("git").arg("-C").arg(&dir).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap();
        let head = head.trim();
        let b = blame(&dir, head, "f.txt", Some((2, 99))).unwrap();
        assert_eq!((b.from, b.to, b.total_lines), (2, 3, 3), "el final se acota al fichero");
        assert_eq!(b.commits.len(), 2);
        assert_eq!(b.commits.iter().map(|c| c.lines).sum::<u32>(), 2);
        assert_eq!(b, blame(&dir, head, "f.txt", Some((2, 99))).unwrap(), "misma entrada, misma salida");
        assert!(blame(&dir, head, "f.txt", Some((10, 12))).is_err(), "rango fuera del fichero");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
