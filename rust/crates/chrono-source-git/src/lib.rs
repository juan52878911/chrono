//! `chrono-source-git` — adaptador Source de git (streaming de `git log` a `Event`).
//!
//! Implementa `chrono_core::Source`/`Cursor`. Réplica de comportamiento del
//! original Go en `internal/ingest/gitlog.go` (formato de log, numstat,
//! renames, reverts, watermark, manifiesto). Ver `docs/DESIGN-GENERAL-CORE.md §2-3`.
//!
//! Limitaciones conocidas frente al Go (documentadas también en el resumen
//! final de la implementación):
//! - `change_type` de numstat solo distingue "R" (rename) de "M" (todo lo
//!   demás); numstat no trae por sí mismo A/D de forma fiable sin `--raw`.
//! - El SimHash se calcula solo sobre `title`+`body` (no sobre las rutas
//!   tocadas, que sí incluía el Go), y los tokens no filtran por longitud
//!   mínima — así lo pide el encargo de esta oleada.
//! - La extracción de tickets es un parseo manual simple (`#123`, `ABC-123`);
//!   los patrones configurables por repo quedan para más adelante.

mod cursor;
mod gitutil;
mod parse;
mod simhash;
mod timeutil;

pub use gitutil::{blob_lines, commit_exists, git_version, head_sha, is_shallow, ls_tree, Result};

use chrono_core::{CoreError, Result as CoreResult, Source, SourceConfig, Watermark};
use cursor::GitCursor;
use std::path::Path;
use std::process::{Command, Stdio};

/// Adaptador `Source` de git: streaming de `git log` a `Event`.
pub struct GitSource;

impl GitSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for GitSource {
    fn kind(&self) -> &str {
        "git"
    }

    fn detect(&self, path: &Path) -> i32 {
        let ok = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--show-toplevel"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            100
        } else {
            0
        }
    }

    fn open(
        &self,
        path: &Path,
        watermark: Option<Watermark>,
        _cfg: &SourceConfig,
    ) -> CoreResult<Box<dyn chrono_core::Cursor>> {
        let shallow = is_shallow(path)
            .map_err(|e| CoreError::Other(format!("comprobando shallow: {e}")))?;
        if shallow {
            return Err(CoreError::Other(
                "el repo es un clon shallow (depth limitado): las métricas serían falsas. Clona completo"
                    .to_string(),
            ));
        }

        let head = head_sha(path).map_err(|e| CoreError::Other(format!("leyendo HEAD: {e}")))?;

        let range = match watermark {
            None => None,
            Some(wm) => {
                let sha = wm
                    .value
                    .strip_prefix("sha:")
                    .ok_or_else(|| CoreError::Diverged(wm.value.clone()))?;
                if !commit_exists(path, sha) {
                    return Err(CoreError::Diverged(sha.to_string()));
                }
                Some(format!("{sha}..HEAD"))
            }
        };

        let cur = GitCursor::spawn(path, head, range)?;
        Ok(Box::new(cur))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempRepo {
        dir: std::path::PathBuf,
    }

    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "chrono-source-git-test-{tag}-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    impl TempRepo {
        fn new() -> Self {
            let dir = unique_dir("repo");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Cmd::new("git").arg("init").arg("-q").arg(&dir).status().unwrap();
            run(&dir, &["config", "user.email", "test@example.com"]);
            run(&dir, &["config", "user.name", "Test"]);
            // Silencia firmas GPG heredadas del entorno del usuario.
            run(&dir, &["config", "commit.gpgsign", "false"]);
            Self { dir }
        }

        fn path(&self) -> &Path {
            &self.dir
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn run(dir: &Path, args: &[&str]) {
        let out = Cmd::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed in {dir:?}");
    }

    fn commit(dir: &Path, file: &str, contents: &str, msg: &str) -> String {
        std::fs::write(dir.join(file), contents).unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-q", "-m", msg]);
        String::from_utf8(
            Cmd::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    fn rev_list_count(dir: &Path) -> usize {
        let out = Cmd::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-list", "--count", "--no-merges", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().parse().unwrap()
    }

    #[test]
    fn detect_reconoce_repo_git_y_rechaza_lo_demas() {
        let repo = TempRepo::new();
        commit(repo.path(), "a.txt", "hola\n", "primero");
        let src = GitSource::new();
        assert_eq!(src.detect(repo.path()), 100);

        let not_git = unique_dir("not-a-repo");
        std::fs::create_dir_all(&not_git).unwrap();
        assert_eq!(src.detect(&not_git), 0);
        let _ = std::fs::remove_dir_all(&not_git);
    }

    #[test]
    fn drena_todos_los_commits_no_merge() {
        let repo = TempRepo::new();
        commit(repo.path(), "a.txt", "1\n", "primero");
        commit(repo.path(), "b.txt", "2\n", "segundo");
        commit(repo.path(), "a.txt", "1\n2\n", "tercero");

        let src = GitSource::new();
        let cfg = SourceConfig::default();
        let mut cur = src.open(repo.path(), None, &cfg).unwrap();

        let mut count = 0;
        while let Some(_ev) = cur.next().unwrap() {
            count += 1;
        }
        assert_eq!(count, rev_list_count(repo.path()));
        assert_eq!(count, 3);
    }

    #[test]
    fn touches_de_un_commit_conocido() {
        let repo = TempRepo::new();
        commit(repo.path(), "a.txt", "uno\n", "primero");
        commit(repo.path(), "b.txt", "dos\ntres\n", "segundo");

        let src = GitSource::new();
        let cfg = SourceConfig::default();
        let mut cur = src.open(repo.path(), None, &cfg).unwrap();

        // El cursor entrega en orden de `git log` (más reciente primero).
        let first = cur.next().unwrap().unwrap();
        assert_eq!(first.title, "segundo");
        assert_eq!(first.touches.len(), 1);
        assert_eq!(first.touches[0].entity, "b.txt");
        let added: i64 = first.touches[0].attrs["added"].parse().unwrap();
        let deleted: i64 = first.touches[0].attrs["deleted"].parse().unwrap();
        assert_eq!(first.touches[0].weight, added + deleted);
        assert_eq!(added, 2);
        assert_eq!(deleted, 0);
    }

    #[test]
    fn rename_produce_touch_con_old_path() {
        let repo = TempRepo::new();
        commit(repo.path(), "old.txt", "contenido\n", "primero");
        run(repo.path(), &["mv", "old.txt", "new.txt"]);
        run(repo.path(), &["commit", "-q", "-m", "rename"]);

        let src = GitSource::new();
        let cfg = SourceConfig::default();
        let mut cur = src.open(repo.path(), None, &cfg).unwrap();

        let first = cur.next().unwrap().unwrap();
        assert_eq!(first.title, "rename");
        assert_eq!(first.touches.len(), 1);
        assert_eq!(first.touches[0].entity, "new.txt");
        assert_eq!(first.touches[0].attrs["change_type"], "R");
        assert_eq!(first.touches[0].attrs["old_path"], "old.txt");
    }

    #[test]
    fn revert_produce_link() {
        let repo = TempRepo::new();
        commit(repo.path(), "a.txt", "uno\n", "primero");
        let sha = commit(repo.path(), "a.txt", "uno\ndos\n", "segundo");
        run(repo.path(), &["revert", "--no-edit", &sha]);

        let src = GitSource::new();
        let cfg = SourceConfig::default();
        let mut cur = src.open(repo.path(), None, &cfg).unwrap();

        let first = cur.next().unwrap().unwrap(); // el revert es el commit más reciente.
        assert!(first.title.to_lowercase().starts_with("revert"));
        let revert_link = first.links.iter().find(|l| l.rel == "reverts");
        assert!(revert_link.is_some(), "esperaba Link{{rel: reverts}}");
        assert!(sha.starts_with(&revert_link.unwrap().target));
    }

    #[test]
    fn epoch_y_simhash() {
        let repo = TempRepo::new();
        commit(repo.path(), "a.txt", "uno\n", "un mensaje no vacio");

        let src = GitSource::new();
        let cfg = SourceConfig::default();
        let mut cur = src.open(repo.path(), None, &cfg).unwrap();
        let ev = cur.next().unwrap().unwrap();

        assert!(ev.at_epoch > 0);
        assert!(ev.at.ends_with('Z'));
        assert_ne!(ev.simhash, 0);
    }

    #[test]
    fn watermark_y_manifest() {
        let repo = TempRepo::new();
        commit(repo.path(), "a.txt", "uno\n", "primero");

        let src = GitSource::new();
        let cfg = SourceConfig::default();
        let head = head_sha(repo.path()).unwrap();
        let mut cur = src.open(repo.path(), None, &cfg).unwrap();
        while cur.next().unwrap().is_some() {}

        let wm = cur.watermark();
        assert_eq!(wm.kind, "git");
        assert_eq!(wm.value, format!("sha:{head}"));

        let manifest = cur.manifest();
        assert_eq!(manifest["first_parent"], "false");
        assert_eq!(manifest["no_merges"], "true");
        assert_eq!(manifest["mailmap_used"], "true");
        assert_eq!(manifest["bulk_threshold"], "50");
        assert!(!manifest["git_version"].is_empty());
    }

    #[test]
    fn watermark_incremental_y_divergencia() {
        let repo = TempRepo::new();
        let first_sha = commit(repo.path(), "a.txt", "uno\n", "primero");
        commit(repo.path(), "b.txt", "dos\n", "segundo");

        let src = GitSource::new();
        let cfg = SourceConfig::default();

        // Rango incremental: solo el commit posterior al watermark.
        let wm = Watermark {
            kind: "git".to_string(),
            value: format!("sha:{first_sha}"),
        };
        let mut cur = src.open(repo.path(), Some(wm), &cfg).unwrap();
        let mut count = 0;
        while cur.next().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 1);

        // SHA inexistente -> Diverged.
        let bad_wm = Watermark {
            kind: "git".to_string(),
            value: "sha:0000000000000000000000000000000000000000".to_string(),
        };
        match src.open(repo.path(), Some(bad_wm), &cfg) {
            Err(CoreError::Diverged(_)) => {}
            other => panic!("esperaba CoreError::Diverged, obtuve otra cosa (ok={})", other.is_ok()),
        }
    }

    #[test]
    fn rechaza_repos_shallow() {
        let origin = TempRepo::new();
        commit(origin.path(), "a.txt", "uno\n", "primero");
        commit(origin.path(), "b.txt", "dos\n", "segundo");

        let shallow_dir = unique_dir("shallow");
        let url = format!("file://{}", origin.path().to_string_lossy());
        let status = Cmd::new("git")
            .args(["clone", "-q", "--depth", "1", &url])
            .arg(&shallow_dir)
            .status();
        // Si el clone shallow falla en este entorno (permisos/versión de git),
        // no invalidamos el resto de la suite: solo verificamos si se pudo crear.
        if status.map(|s| s.success()).unwrap_or(false) {
            let src = GitSource::new();
            let cfg = SourceConfig::default();
            match src.open(&shallow_dir, None, &cfg) {
                Err(CoreError::Other(_)) => {}
                other => panic!("esperaba CoreError::Other, obtuve otra cosa (ok={})", other.is_ok()),
            }
            let _ = std::fs::remove_dir_all(&shallow_dir);
        }
    }

    #[test]
    fn helpers_libres() {
        let repo = TempRepo::new();
        commit(repo.path(), "a.txt", "l1\nl2\nl3\n", "primero");

        let head = head_sha(repo.path()).unwrap();
        assert_eq!(head.len(), 40);
        assert!(!is_shallow(repo.path()).unwrap());

        let tree = ls_tree(repo.path()).unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].0, "a.txt");

        let oids: Vec<String> = tree.iter().map(|(_, o)| o.clone()).collect();
        let lines = blob_lines(repo.path(), &oids).unwrap();
        assert_eq!(lines[0].1, 3);
    }
}
