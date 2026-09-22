//! Utilidades libres para invocar `git` (dimensionado de tamaños y metadatos),
//! pensadas para que el orquestador las reutilice. Réplica de las funciones
//! homónimas de `internal/ingest/gitlog.go` (`IsShallow`, `headSHA`, `lsTree`,
//! `blobLines`).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Error propio de este crate: cualquier fallo de I/O o de invocación a git.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn run(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git {args:?} failed: {stderr}").into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// SHA de HEAD.
pub fn head_sha(repo: &Path) -> Result<String> {
    run(repo, &["rev-parse", "HEAD"])
}

/// `true` si el repo es un clon shallow (métricas serían falsas).
pub fn is_shallow(repo: &Path) -> Result<bool> {
    let out = run(repo, &["rev-parse", "--is-shallow-repository"])?;
    Ok(out == "true")
}

/// `true` si `sha` existe como commit alcanzable en el repo.
pub fn commit_exists(repo: &Path, sha: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `git --version`, recortado.
pub fn git_version() -> Result<String> {
    let out = Command::new("git").arg("--version").output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Pares (ruta, OID de blob) de HEAD, vía `git ls-tree -r -z HEAD`.
pub fn ls_tree(repo: &Path) -> Result<Vec<(String, String)>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-tree", "-r", "-z", "HEAD"])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git ls-tree failed: {stderr}").into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut res = Vec::new();
    for rec in text.split('\0') {
        if rec.is_empty() {
            continue;
        }
        let Some(tab) = rec.find('\t') else { continue };
        let meta: Vec<&str> = rec[..tab].split_whitespace().collect();
        if meta.len() < 3 {
            continue;
        }
        let oid = meta[2].to_string();
        let path = rec[tab + 1..].to_string();
        res.push((path, oid));
    }
    Ok(res)
}

/// Cuenta líneas de cada OID vía un único proceso `git cat-file --batch`.
/// Réplica de `blobLines` en Go: binarios (con NUL) cuentan 0 líneas.
pub fn blob_lines(repo: &Path, oids: &[String]) -> Result<Vec<(String, i64)>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    // Los OIDs se escriben desde un hilo aparte mientras este hilo lee stdout.
    // Si se escribieran todos ANTES de leer, con más de ~1.600 OIDs (64 KB, el
    // buffer del pipe) git se bloquearía escribiendo su stdout, dejaría de leer
    // stdin y ambos procesos se quedarían esperando (deadlock real en bun).
    // El hilo suelta el `ChildStdin` al terminar: eso cierra el pipe y manda
    // EOF a `git cat-file --batch` para que acabe.
    let mut stdin = child.stdin.take().ok_or("no se pudo abrir stdin")?;
    let to_write: Vec<String> = oids.to_vec();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for oid in &to_write {
            writeln!(stdin, "{oid}")?;
        }
        Ok(())
    });

    let stdout = child.stdout.take().ok_or("no se pudo abrir stdout")?;
    let mut br = BufReader::with_capacity(1 << 20, stdout);
    let mut res = Vec::new();

    loop {
        let mut header = String::new();
        let n = br.read_line(&mut header)?;
        if n == 0 {
            break;
        }
        let fields: Vec<&str> = header.split_whitespace().collect();
        if fields.len() < 3 {
            // "<oid> missing" u otra línea sin contenido asociado.
            continue;
        }
        let oid = fields[0].to_string();
        let size: usize = fields[2].parse().unwrap_or(0);
        // git añade un '\n' final tras el contenido exacto de `size` bytes.
        let mut buf = vec![0u8; size + 1];
        br.read_exact(&mut buf)?;
        let content = &buf[..size];
        let has_nul = content.contains(&0u8);
        let mut lines = 0i64;
        if !has_nul {
            for &b in content {
                if b == b'\n' {
                    lines += 1;
                }
            }
            if size > 0 && content[size - 1] != b'\n' {
                lines += 1;
            }
        }
        res.push((oid, lines));
    }

    let _ = child.wait();
    match writer.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("escribiendo OIDs a git cat-file: {e}").into()),
        Err(_) => return Err("el hilo que escribía OIDs a git cat-file falló".into()),
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;

    fn init_repo(dir: &Path) {
        Cmd::new("git").arg("init").arg("-q").arg(dir).status().unwrap();
        Cmd::new("git")
            .args(["-C", dir.to_str().unwrap(), "config", "user.email", "a@b.c"])
            .status()
            .unwrap();
        Cmd::new("git")
            .args(["-C", dir.to_str().unwrap(), "config", "user.name", "Ana"])
            .status()
            .unwrap();
    }

    #[test]
    fn head_sha_ls_tree_y_blob_lines() {
        let dir = std::env::temp_dir().join(format!(
            "chrono-source-git-test-gitutil-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);
        std::fs::write(dir.join("a.txt"), "linea1\nlinea2\nlinea3\n").unwrap();
        Cmd::new("git")
            .args(["-C", dir.to_str().unwrap(), "add", "."])
            .status()
            .unwrap();
        Cmd::new("git")
            .args(["-C", dir.to_str().unwrap(), "commit", "-q", "-m", "init"])
            .status()
            .unwrap();

        let head = head_sha(&dir).unwrap();
        assert_eq!(head.len(), 40);
        assert!(!is_shallow(&dir).unwrap());

        let tree = ls_tree(&dir).unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].0, "a.txt");

        let oids: Vec<String> = tree.iter().map(|(_, o)| o.clone()).collect();
        let lines = blob_lines(&dir, &oids).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].1, 3);

        // Regresión (deadlock en bun): más OIDs de los que caben en el buffer
        // del pipe (64 KB ≈ 1.600 OIDs). El mismo OID 5.000 veces basta.
        let many: Vec<String> = std::iter::repeat_n(oids[0].clone(), 5000).collect();
        let lines = blob_lines(&dir, &many).unwrap();
        assert_eq!(lines.len(), 5000);
        assert!(lines.iter().all(|(_, n)| *n == 3));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
