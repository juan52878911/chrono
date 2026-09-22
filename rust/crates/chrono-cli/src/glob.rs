//! Globs de exclusión de ruido (réplica de `matchAnyGlob` + `config.Default()`
//! del Go). En R1 no hay `config.json`: se aplican los valores por defecto del
//! Go para que hotspots/churn den lo mismo.

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
