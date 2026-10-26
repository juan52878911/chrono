//! Globs de exclusión de ruido (réplica de `matchAnyGlob` + `config.Default()`
//! del Go). En R1 no hay `config.json`: se aplican los valores por defecto del
//! Go para que hotspots/churn den lo mismo.
//!
//! También expande los globs de `chrono add '<dir>/**/*.jsonl'` ([`expand`]).

use std::path::{Path, PathBuf};

/// Globs por defecto del Go v0.1.1 (`internal/config/config.go`).
pub const DEFAULT_EXCLUDE_GLOBS: &[&str] = &[
    "*-lock.json",
    "*.lock",
    "package-lock.json",
    "*.min.js",
    "*.csv",
    "dist/*",
    "build/*",
    "vendor/*",
    "node_modules/*",
];

/// Un glob casa contra el basename, la ruta completa, o (si es `dir/*`)
/// contra cualquier segmento de directorio. Igual que el Go.
pub fn match_any(globs: &[&str], path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    for g in globs {
        if glob_match(g, base) || glob_match(g, path) {
            return true;
        }
        if let Some(dir) = g.strip_suffix("/*") {
            if path.split('/').any(|seg| seg == dir) {
                return true;
            }
        }
    }
    false
}

/// ¿Lleva comodines? (`*`, `?`, `[`).
pub fn is_pattern(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Ficheros que casan con `pattern` (ruta absoluta con `*`, `?` y `**`), en
/// orden de ruta. `*` y `?` no cruzan `/`; `**` casa con cero o más
/// directorios. No entra en enlaces simbólicos a directorios (evita ciclos).
pub fn expand(pattern: &str) -> Vec<PathBuf> {
    let segs: Vec<&str> = pattern.split('/').collect();
    let first = segs.iter().position(|s| is_pattern(s)).unwrap_or(segs.len());
    let base = segs[..first].join("/");
    let base = if base.is_empty() { "/".to_string() } else { base };
    let mut out = Vec::new();
    if first == segs.len() {
        if Path::new(&base).is_file() {
            out.push(PathBuf::from(base));
        }
        return out;
    }
    walk(Path::new(&base), &segs[first..], &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk(dir: &Path, rest: &[&str], out: &mut Vec<PathBuf>) {
    let Some((seg, tail)) = rest.split_first() else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(String, PathBuf, std::fs::FileType)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.file_name().to_string_lossy().into_owned(), e.path(), e.file_type().ok()?)))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if *seg == "**" {
        // Cero directorios: el resto del patrón aquí mismo (`a/**` = todo).
        walk(dir, if tail.is_empty() { &["*"] } else { tail }, out);
        for (_, path, ft) in &entries {
            if ft.is_dir() {
                walk(path, rest, out);
            }
        }
        return;
    }
    for (name, path, ft) in entries {
        if !glob_match(seg, &name) {
            continue;
        }
        if tail.is_empty() {
            // Un enlace a un fichero cuenta como fichero.
            if ft.is_file() || (ft.is_symlink() && path.is_file()) {
                out.push(path);
            }
        } else if ft.is_dir() {
            walk(&path, tail, out);
        }
    }
}

/// `filepath.Match` reducido: `*` (no cruza `/`), `?` y literales.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    fn rec(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => {
                // `*` consume 0..n caracteres que no sean '/'.
                let mut i = 0;
                loop {
                    if rec(&p[1..], &t[i..]) {
                        return true;
                    }
                    if i >= t.len() || t[i] == '/' {
                        return false;
                    }
                    i += 1;
                }
            }
            Some('?') => !t.is_empty() && t[0] != '/' && rec(&p[1..], &t[1..]),
            Some(&c) => !t.is_empty() && t[0] == c && rec(&p[1..], &t[1..]),
        }
    }
    rec(&p, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_con_doble_asterisco() {
        let dir = std::env::temp_dir().join(format!("chrono-glob-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        for f in ["a.jsonl", "p1/s1.jsonl", "p1/sub/s2.jsonl", "p2/s3.jsonl", "p2/nota.txt"] {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "{}").unwrap();
        }
        let d = dir.to_string_lossy();
        let rel = |v: Vec<PathBuf>| -> Vec<String> {
            v.iter().map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned()).collect()
        };
        assert_eq!(rel(expand(&format!("{d}/**/*.jsonl"))), ["a.jsonl", "p1/s1.jsonl", "p1/sub/s2.jsonl", "p2/s3.jsonl"]);
        assert_eq!(rel(expand(&format!("{d}/*/*.jsonl"))), ["p1/s1.jsonl", "p2/s3.jsonl"]);
        assert_eq!(rel(expand(&format!("{d}/p?/**"))), ["p1/s1.jsonl", "p1/sub/s2.jsonl", "p2/nota.txt", "p2/s3.jsonl"]);
        assert!(expand(&format!("{d}/nada/**/*.jsonl")).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn casa_como_el_go() {
        let g = DEFAULT_EXCLUDE_GLOBS;
        assert!(match_any(g, "Cargo.lock"));
        assert!(match_any(g, "web/package-lock.json"));
        assert!(match_any(g, "src/vendor/x.c"));
        assert!(match_any(g, "dist/app.js"));
        assert!(match_any(g, "data/big.csv"));
        assert!(!match_any(g, "src/lib.rs"));
        assert!(!match_any(g, "distro/notes.md"));
        assert!(!match_any(g, "src/lockfree.rs"));
    }
}
