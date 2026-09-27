//! Resolución de repos y claves compartidas (docs/CONTRACTS.md): qué repo
//! responde cuando un solo proceso sirve muchos (`<nombre>`, `<nombre>@<rama>`
//! o ruta absoluta), la etiqueta `<nombre>@<rama>` de las respuestas y la
//! normalización de claves de entidad (`ruta` o `ruta#símbolo`).
//!
//! Solo `std::fs`: la rama se lee de `.git/HEAD` y los worktrees de
//! `.git/worktrees/*`, sin lanzar git. Paridad con el Go `internal/repos`.

use std::fmt;
use std::path::{Path, PathBuf};

/// Índice de un repo, relativo a su raíz: `<raíz>/<nombre>/.chrono/index.db`.
pub const INDEX_REL: &str = ".chrono/index.db";

/// Variable de entorno con la raíz de repos (p.ej. `/data/repos`).
pub const ENV_ROOT: &str = "CHRONO_REPOS";

/// Repo ya resuelto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub dir: PathBuf,
    pub index: PathBuf,
    pub label: String,
}

/// Error de resolución. `NoIndex` lleva el `Target` para poder sugerir
/// `chrono init <dir>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    NoIndex(Target),
    Invalid(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NoIndex(t) => write!(
                f,
                "no chrono index for {} ({}). Run: chrono init {}",
                t.label,
                t.dir.display(),
                t.dir.display()
            ),
            ResolveError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Dónde está (o estaría) el índice de `dir`.
pub fn index_path(dir: &Path) -> PathBuf {
    dir.join(INDEX_REL)
}

pub fn has_index(dir: &Path) -> bool {
    index_path(dir).is_file()
}

/// Directorio git de un árbol: `.git` si es carpeta, o el destino de
/// `gitdir: X` si `.git` es un fichero (worktree o submódulo).
fn git_dir(dir: &Path) -> Option<PathBuf> {
    let p = dir.join(".git");
    if p.is_dir() {
        return Some(p);
    }
    let s = std::fs::read_to_string(&p).ok()?;
    let g = s.trim().strip_prefix("gitdir:")?.trim();
    let g = PathBuf::from(g);
    Some(if g.is_absolute() { g } else { dir.join(g) })
}

/// Rama de un fichero HEAD: `ref: refs/heads/X` → X; separado → `HEAD`.
fn head_branch(head: &Path) -> Option<String> {
    let s = std::fs::read_to_string(head).ok()?;
    let s = s.trim();
    Some(match s.strip_prefix("ref:") {
        Some(r) => {
            let r = r.trim();
            r.strip_prefix("refs/heads/").unwrap_or(r).to_string()
        }
        None => "HEAD".to_string(),
    })
}

/// Rama actual del árbol (`None` si no es un repo git).
pub fn branch(dir: &Path) -> Option<String> {
    head_branch(&git_dir(dir)?.join("HEAD"))
}

fn base_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Etiqueta `<nombre>@<rama>`. El nombre es el del repo principal (un
/// worktree de kindling es `kindling@<rama>`). Sin git, solo `<nombre>`.
pub fn label(dir: &Path) -> String {
    let mut name = base_name(dir);
    let Some(g) = git_dir(dir) else { return name };
    if let Some(parent) = g.parent() {
        if base_name(parent) == "worktrees" {
            if let Some(main) = parent.parent().and_then(Path::parent) {
                name = base_name(main);
            }
        }
    }
    match head_branch(&g.join("HEAD")) {
        Some(b) => format!("{name}@{b}"),
        None => name,
    }
}

/// Separa `<nombre>@<rama>` (la rama puede llevar `/`, el nombre no).
pub fn split_label(s: &str) -> (&str, Option<&str>) {
    match s.split_once('@') {
        Some((n, b)) => (n, Some(b)),
        None => (s, None),
    }
}

/// Worktrees enlazados de un repo: (rama, ruta).
fn worktrees(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Some(g) = git_dir(dir) else { return out };
    let Ok(rd) = std::fs::read_dir(g.join("worktrees")) else {
        return out;
    };
    for e in rd.flatten() {
        let wd = e.path();
        let Some(br) = head_branch(&wd.join("HEAD")) else {
            continue;
        };
        let Ok(gd) = std::fs::read_to_string(wd.join("gitdir")) else {
            continue;
        };
        if br == "HEAD" {
            continue;
        }
        let p = PathBuf::from(gd.trim());
        let p = if p.is_absolute() { p } else { wd.join(p) };
        if let Some(tree) = p.parent() {
            out.push((br, tree.to_path_buf()));
        }
    }
    out
}

/// Nombre de directorio de una rama (mismo criterio que UltraMemory
/// `ramas.py`): lo que no sea `[A-Za-z0-9._-]` se vuelve `-`.
pub fn slug(branch: &str) -> String {
    let mut s = String::new();
    let mut dash = false;
    for c in branch.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            s.push(c);
            dash = false;
        } else if !dash {
            s.push('-');
            dash = true;
        }
    }
    let s = s.trim_matches(|c| c == '-' || c == '.');
    let s: String = s.chars().take(80).collect();
    if s.is_empty() {
        "rama".to_string()
    } else {
        s
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n != "."
        && n != ".."
        && !n.contains('/')
        && !n.contains('\\')
        && !n.starts_with('.')
}

fn absolute(p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|c| c.join(&p)).unwrap_or(p)
    }
}

/// Convierte el argumento `repo` en un árbol con su índice:
///
/// - ruta absoluta → ese árbol;
/// - `<nombre>` → `<root>/<nombre>`;
/// - `<nombre>@<rama>` → primero el directorio (o symlink) literal
///   `<root>/<nombre>@<rama con '/'→'-'>` (o su [`slug`]); si no existe,
///   `<root>/<nombre>` si está en esa rama; si no, su worktree de esa rama, o
///   `<root>/../ramas/<nombre>/<slug>` (disposición de UltraMemory).
///
/// Un árbol sin índice da `ResolveError::NoIndex` con la orden de `chrono init`.
pub fn resolve(root: Option<&Path>, spec: &str) -> Result<Target, ResolveError> {
    let spec = spec.trim();
    let inv = |m: String| Err(ResolveError::Invalid(m));
    if spec.is_empty() {
        return inv("empty repo".into());
    }
    let dir: PathBuf;
    if Path::new(spec).is_absolute() {
        dir = PathBuf::from(spec);
        if !dir.is_dir() {
            return inv(format!("repo {spec:?}: no such directory"));
        }
    } else {
        let (name, want) = split_label(spec);
        if !valid_name(name) {
            return inv(format!(
                "repo {spec:?}: expected a name, <name>@<branch> or an absolute path"
            ));
        }
        let Some(root) = root else {
            return inv(format!(
                "repo {spec:?}: no repos root configured (set {ENV_ROOT} or run `chrono mcp --repos DIR`), or pass an absolute path"
            ));
        };
        let base = root.join(name);
        // Un directorio o symlink con la etiqueta como nombre manda: es lo
        // que monta el operador (p.ej. UltraMemory: kindling@feature-x).
        let literal = want.and_then(|b| {
            [
                format!("{name}@{}", b.replace('/', "-")),
                format!("{name}@{}", slug(b)),
            ]
            .into_iter()
            .map(|c| root.join(c))
            .find(|p| p.is_dir())
        });
        if let Some(p) = literal {
            dir = p;
        } else if !base.is_dir() {
            return inv(format!(
                "repo {spec:?}: {} does not exist (the `repos` tool lists what is under {})",
                base.display(),
                root.display()
            ));
        } else {
            match want {
                Some(b) if branch(&base).as_deref() != Some(b) => {
                    let mut found = worktrees(&base)
                        .into_iter()
                        .find(|(br, p)| br == b && p.is_dir())
                        .map(|(_, p)| p);
                    if found.is_none() {
                        let c = root
                            .parent()
                            .unwrap_or(root)
                            .join("ramas")
                            .join(name)
                            .join(slug(b));
                        if c.is_dir() && branch(&c).as_deref() == Some(b) {
                            found = Some(c);
                        }
                    }
                    match found {
                        Some(p) => dir = p,
                        None => {
                            return inv(format!(
                                "repo {spec:?}: branch {b:?} is not checked out (neither {} nor any of its worktrees is on it)",
                                base.display()
                            ))
                        }
                    }
                }
                _ => dir = base,
            }
        }
    }
    let dir = absolute(dir);
    let t = Target {
        index: index_path(&dir),
        label: label(&dir),
        dir,
    };
    if !t.index.is_file() {
        return Err(ResolveError::NoIndex(t));
    }
    Ok(t)
}

/// Fila de [`list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub repo: String,
    pub name: String,
    pub path: PathBuf,
    pub indexed: bool,
}

/// Directorios de primer nivel de `root` que son repos git (o tienen índice),
/// ordenados por nombre, con si están indexados.
pub fn list(root: &Path) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root)?.flatten() {
        let path = e.path();
        let name = base_name(&path);
        if !path.is_dir() || name.starts_with('.') {
            continue;
        }
        let indexed = has_index(&path);
        if git_dir(&path).is_none() && !indexed {
            continue;
        }
        out.push(Entry {
            repo: label(&path),
            name,
            path,
            indexed,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Normaliza una clave de entidad de entrada a la ruta relativa a la raíz
/// del repo: quita `#símbolo` (una respuesta a nivel de fichero lo ignora;
/// así entra tal cual un id `ruta#símbolo` de codegraph), quita `./` y hace
/// relativa una ruta absoluta que cae dentro de `repo_dir`.
pub fn entity_key(key: &str, repo_dir: Option<&Path>) -> String {
    let k = key.trim();
    let k = k.split_once('#').map_or(k, |(p, _)| p);
    let mut k = k.to_string();
    if let Some(rd) = repo_dir {
        if let Ok(rel) = Path::new(&k).strip_prefix(rd) {
            if Path::new(&k).is_absolute() {
                k = rel.to_string_lossy().into_owned();
            }
        }
    }
    let k = k.replace('\\', "/");
    let mut s = k.as_str();
    while let Some(r) = s.strip_prefix("./") {
        s = r;
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Tmp {
            let p = std::env::temp_dir().join(format!(
                "chrono-repos-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    fn fake_repo(dir: &Path, branch: &str, indexed: bool) {
        write(
            &dir.join(".git/HEAD"),
            &format!("ref: refs/heads/{branch}\n"),
        );
        if indexed {
            write(&index_path(dir), "");
        }
    }

    fn fake_worktree(repo: &Path, wt: &Path, id: &str, branch: &str) {
        let meta = repo.join(".git/worktrees").join(id);
        write(&meta.join("HEAD"), &format!("ref: refs/heads/{branch}\n"));
        write(
            &meta.join("gitdir"),
            &format!("{}\n", wt.join(".git").display()),
        );
        write(&wt.join(".git"), &format!("gitdir: {}\n", meta.display()));
        write(&index_path(wt), "");
    }

    #[test]
    fn resuelve_nombre_etiqueta_worktree_y_ruta() {
        let tmp = Tmp::new();
        let root = tmp.0.join("repos");
        fake_repo(&root.join("alpha"), "main", true);
        let wt = tmp.0.join("wt");
        fake_worktree(&root.join("alpha"), &wt, "w1", "claude/feature-x");

        let t = resolve(Some(&root), "alpha").unwrap();
        assert_eq!(t.label, "alpha@main");
        assert_eq!(t.dir, root.join("alpha"));
        assert_eq!(
            resolve(Some(&root), "alpha@main").unwrap().label,
            "alpha@main"
        );
        let t = resolve(Some(&root), "alpha@claude/feature-x").unwrap();
        assert_eq!(t.dir, wt);
        assert_eq!(t.label, "alpha@claude/feature-x");
        let abs = root.join("alpha");
        assert_eq!(
            resolve(None, abs.to_str().unwrap()).unwrap().label,
            "alpha@main"
        );
    }

    #[test]
    fn resuelve_disposicion_ultramemory() {
        let tmp = Tmp::new();
        let root = tmp.0.join("repos");
        fake_repo(&root.join("kindling"), "main", true);
        let rama = tmp.0.join("ramas/kindling/claude-fix-x");
        fake_repo(&rama, "claude/fix-x", true);
        assert_eq!(
            resolve(Some(&root), "kindling@claude/fix-x").unwrap().dir,
            rama
        );
    }

    /// `chrono mcp -repos /data/chrono/arboles`: carpeta de symlinks
    /// `kindling` y `kindling@<slug>` a árboles con su índice.
    #[cfg(unix)]
    #[test]
    fn symlinks_como_ultramemory() {
        use std::os::unix::fs::symlink;
        let tmp = Tmp::new();
        let main = tmp.0.join("repos/kindling");
        let wt = tmp.0.join("ramas/kindling/claude-feature-x");
        fake_repo(&main, "main", true);
        fake_worktree(&main, &wt, "claude-feature-x", "claude/feature-x");
        let root = tmp.0.join("arboles");
        std::fs::create_dir_all(&root).unwrap();
        symlink(&main, root.join("kindling")).unwrap();
        symlink(&wt, root.join("kindling@claude-feature-x")).unwrap();
        symlink(&wt, root.join("kindling@feature-y")).unwrap();
        symlink(tmp.0.join("no-existe"), root.join("roto@x")).unwrap();
        for (spec, want) in [
            ("kindling", "kindling"),
            ("kindling@claude-feature-x", "kindling@claude-feature-x"),
            ("kindling@claude/feature-x", "kindling@claude-feature-x"),
            ("kindling@feature-y", "kindling@feature-y"),
        ] {
            assert_eq!(
                resolve(Some(&root), spec).unwrap().dir,
                root.join(want),
                "{spec}"
            );
        }
        assert_eq!(
            resolve(Some(&root), "kindling@claude-feature-x")
                .unwrap()
                .label,
            "kindling@claude/feature-x"
        );
        let got: Vec<String> = list(&root)
            .unwrap()
            .into_iter()
            .map(|e| format!("{}={}", e.name, e.repo))
            .collect();
        assert_eq!(
            got,
            vec![
                "kindling=kindling@main",
                "kindling@claude-feature-x=kindling@claude/feature-x",
                "kindling@feature-y=kindling@claude/feature-x",
            ]
        );
    }

    #[test]
    fn errores_claros() {
        let tmp = Tmp::new();
        let root = tmp.0.clone();
        fake_repo(&root.join("beta"), "develop", false);
        match resolve(Some(&root), "beta") {
            Err(e @ ResolveError::NoIndex(_)) => {
                assert!(
                    e.to_string()
                        .contains(&format!("chrono init {}", root.join("beta").display())),
                    "{e}"
                )
            }
            other => panic!("se esperaba NoIndex: {other:?}"),
        }
        assert!(resolve(Some(&root), "nope")
            .unwrap_err()
            .to_string()
            .contains("does not exist"));
        assert!(resolve(Some(&root), "beta@main")
            .unwrap_err()
            .to_string()
            .contains("not checked out"));
        for bad in ["../etc", "a/b", ".hidden", ".."] {
            assert!(
                resolve(Some(&root), bad).is_err(),
                "{bad} debería rechazarse"
            );
        }
        assert!(resolve(None, "beta")
            .unwrap_err()
            .to_string()
            .contains(ENV_ROOT));
    }

    #[test]
    fn lista_repos_bajo_la_raiz() {
        let tmp = Tmp::new();
        fake_repo(&tmp.0.join("b"), "main", false);
        fake_repo(&tmp.0.join("a"), "dev", true);
        std::fs::create_dir_all(tmp.0.join("notarepo")).unwrap();
        let l = list(&tmp.0).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].repo.as_str(), l[0].indexed), ("a@dev", true));
        assert_eq!((l[1].repo.as_str(), l[1].indexed), ("b@main", false));
    }

    #[test]
    fn clave_de_entidad() {
        let rd = Path::new("/repo");
        for (inp, want) in [
            ("src/a.go", "src/a.go"),
            ("src/a.go#Parse", "src/a.go"),
            ("./src/a.go#T.method", "src/a.go"),
            ("/repo/src/a.go#x", "src/a.go"),
            ("/elsewhere/a.go", "/elsewhere/a.go"),
            ("pkg/", "pkg/"),
        ] {
            assert_eq!(entity_key(inp, Some(rd)), want, "{inp}");
        }
    }

    #[test]
    fn slug_como_ultramemory() {
        assert_eq!(
            slug("claude/kindling-docs-en-es-612886"),
            "claude-kindling-docs-en-es-612886"
        );
        assert_eq!(slug("feat//x!"), "feat-x");
        assert_eq!(slug("///"), "rama");
    }
}
