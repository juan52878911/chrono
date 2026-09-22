//! `chrono-classify-rules` — clasificador Level-0 (reglas/taxonomía).
//!
//! Implementa `chrono_core::Classifier`. Paridad con el Go `internal/classify/`
//! (conventional commits, fix keywords) + `internal/config/` (defaults y carga
//! de `.chrono/config.json`) + la taxonomía de `internal/metrics/metrics.go`
//! (`CommonBugs`, incluida su función `cleanBody`).
//!
//! Deliberadamente sin dependencia de `regex`: los patrones que reproduce
//! (prefijo conventional-commit, subcadenas de keywords) son simples y se
//! resuelven con parsing manual sobre `&str`, manteniendo el crate ligero.

use chrono_core::{Classifier, Event, Label};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Prefijos de conventional commits reconocidos para la task `kind`.
/// Incluye `revert`, a diferencia del regex Go (`classify.go`) que solo
/// detecta reverts por señal git-nativa; aquí también se reconoce el prefijo
/// textual "revert:" como valor de `kind`.
const CONVENTIONAL_KINDS: [&str; 11] = [
    "feat", "fix", "docs", "refactor", "chore", "test", "perf", "build", "ci", "style", "revert",
];

/// Trailers de cuerpo de commit que `cleanBody` (Go, `internal/metrics/metrics.go`)
/// descarta antes de buscar categorías de bug.
const TRAILERS: [&str; 13] = [
    "co-authored-by:",
    "signed-off-by:",
    "reviewed-by:",
    "acked-by:",
    "tested-by:",
    "reported-by:",
    "cc:",
    "fixes:",
    "closes:",
    "refs:",
    "see-also:",
    "author:",
    "date:",
];

/// Reglas configurables por repo. Paridad de campos y defaults con
/// `internal/config.Config` / `internal/config.Default()` (Go).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Palabras que marcan un commit como "fix" (case-insensitive, multiidioma).
    pub fix_keywords: Vec<String>,
    /// Regex (formato Go) para extraer ids de ticket. No se usa en este crate
    /// (lo consumirá el futuro extractor de tickets); se conserva para
    /// paridad de esquema con `.chrono/config.json`.
    pub ticket_patterns: Vec<String>,
    /// Globs de ficheros a excluir de hotspots/churn (ruido generado).
    pub exclude_globs: Vec<String>,
    /// Labels de PR/issue que marcan un bug (señal determinista del forge).
    pub bug_labels: Vec<String>,
    /// Taxonomía de categorías de bug: categoría -> palabras clave que la marcan.
    pub bug_categories: BTreeMap<String, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Config::defaults()
    }
}

impl Config {
    /// Reglas sensatas de arranque, mismos valores que `internal/config.Default()` (Go).
    pub fn defaults() -> Config {
        Config {
            fix_keywords: vec![
                "fix".into(),
                "bug".into(),
                "hotfix".into(),
                "patch".into(),
                "fixes".into(),
                "fixed".into(),
                "broken".into(),
                "arregl".into(),
                "corrig".into(),
                "solucion".into(),
                "repara".into(),
                "error".into(),
                "falla".into(),
                "roto".into(),
            ],
            ticket_patterns: vec![
                r"\b([A-Z][A-Z0-9]+-\d+)\b".into(), // Jira: PROJ-123, BUG-047
                r"(#\d+)".into(),                   // GitHub: #123, Closes #123
            ],
            exclude_globs: vec![
                "*-lock.json".into(),
                "*.lock".into(),
                "package-lock.json".into(),
                "*.min.js".into(),
                "*.csv".into(),
                "dist/*".into(),
                "build/*".into(),
                "vendor/*".into(),
                "node_modules/*".into(),
            ],
            bug_labels: vec![
                "bug".into(),
                "defect".into(),
                "regression".into(),
                "error".into(),
                "fix".into(),
                "hotfix".into(),
            ],
            bug_categories: BTreeMap::from([
                (
                    "memory-safety".to_string(),
                    vec![
                        "memory leak".into(),
                        "leak".into(),
                        "use-after-free".into(),
                        "double-free".into(),
                        "segfault".into(),
                        "oom".into(),
                        "out of memory".into(),
                        "uninitialized".into(),
                        "buffer overflow".into(),
                    ],
                ),
                (
                    "crash".to_string(),
                    vec![
                        "crash".into(),
                        "panic".into(),
                        "abort".into(),
                        "assertion".into(),
                        "sigsegv".into(),
                        "sigabrt".into(),
                        "fatal".into(),
                    ],
                ),
                (
                    "concurrency".to_string(),
                    vec![
                        "race condition".into(),
                        "data race".into(),
                        "deadlock".into(),
                        "mutex".into(),
                        "thread safety".into(),
                        "atomic".into(),
                    ],
                ),
                (
                    "network".to_string(),
                    vec![
                        "tls".into(),
                        "ssl".into(),
                        "socket".into(),
                        "websocket".into(),
                        "http/2".into(),
                        "http2".into(),
                        "tcp".into(),
                        "keep-alive".into(),
                        "handshake".into(),
                        "timeout".into(),
                        "fetch".into(),
                    ],
                ),
                (
                    "install-deps".to_string(),
                    vec![
                        "lockfile".into(),
                        "bun.lock".into(),
                        "dependency".into(),
                        "dependencies".into(),
                        "registry".into(),
                        "workspace".into(),
                        "node_modules".into(),
                        "npm install".into(),
                    ],
                ),
                (
                    "types".to_string(),
                    vec![
                        "typescript".into(),
                        "d.ts".into(),
                        "tsconfig".into(),
                        "type definition".into(),
                        "typings".into(),
                    ],
                ),
                (
                    "parser".to_string(),
                    vec![
                        "parser".into(),
                        "lexer".into(),
                        "transpiler".into(),
                        "syntax error".into(),
                        "tokenizer".into(),
                        "ast ".into(),
                    ],
                ),
                (
                    "performance".to_string(),
                    vec![
                        "performance".into(),
                        "regression".into(),
                        "memory usage".into(),
                        "cpu usage".into(),
                        "slow".into(),
                    ],
                ),
                (
                    "security".to_string(),
                    vec![
                        "security".into(),
                        "vulnerability".into(),
                        "cve".into(),
                        "injection".into(),
                        "sanitize".into(),
                    ],
                ),
                (
                    "data-db".to_string(),
                    vec![
                        "sqlite".into(),
                        "postgres".into(),
                        "mysql".into(),
                        "serialize".into(),
                        "deserialize".into(),
                        "encoding".into(),
                        "sql query".into(),
                    ],
                ),
                ("windows".to_string(), vec!["windows".into(), "win32".into()]),
                (
                    "build".to_string(),
                    vec![
                        "bundler".into(),
                        "compile error".into(),
                        "linker".into(),
                        "codegen".into(),
                        "cross-compile".into(),
                    ],
                ),
            ]),
        }
    }
}

/// Lee `<repo>/.chrono/config.json`; si falta, es ilegible o inválido, usa
/// `Config::defaults()`. Los campos ausentes del JSON toman su valor default
/// (paridad con `internal/config.Load()`, que parte de `Default()` y hace
/// `json.Unmarshal` encima).
pub fn load(repo: &Path) -> Config {
    let p = repo.join(".chrono").join("config.json");
    match std::fs::read(&p) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| Config::defaults()),
        Err(_) => Config::defaults(),
    }
}

/// Clasificador Level-0: reglas y taxonomía deterministas, sin modelo.
pub struct RulesClassifier {
    cfg: Config,
}

impl RulesClassifier {
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }
}

/// Reconoce un prefijo conventional-commit en `title` (case-sensitive, como el
/// regex Go `^(feat|fix|...)(\([^)]*\))?!?:`). Devuelve el kind sin el scope.
fn conventional_kind(title: &str) -> Option<&'static str> {
    let t = title.trim_start();
    for kind in CONVENTIONAL_KINDS {
        let Some(mut rest) = t.strip_prefix(kind) else { continue };
        if let Some(after_paren) = rest.strip_prefix('(') {
            match after_paren.find(')') {
                Some(idx) => rest = &after_paren[idx + 1..],
                None => continue, // scope sin cerrar: no es un match válido.
            }
        }
        let rest = rest.strip_prefix('!').unwrap_or(rest);
        if rest.starts_with(':') {
            return Some(kind);
        }
    }
    None
}

fn is_trailer(low: &str) -> bool {
    TRAILERS.iter().any(|t| low.starts_with(t))
}

/// Replica `cleanBody` (Go, `internal/metrics/metrics.go`): quita líneas vacías,
/// trailers (Signed-off-by, Co-authored-by, Closes, ...) y líneas con URLs o
/// "noreply", antes de buscar categorías de bug en el cuerpo.
fn clean_body(body: &str) -> String {
    let mut out = String::new();
    for line in body.split('\n') {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let low = l.to_lowercase();
        if is_trailer(&low) || low.contains("noreply") || low.contains("://") {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

impl Classifier for RulesClassifier {
    fn classify(&self, ev: &Event) -> Vec<Label> {
        let mut labels = Vec::new();
        let title = ev.title.trim();

        // --- kind: prefijo conventional-commit, si existe. ---
        let kind = conventional_kind(title);
        if let Some(k) = kind {
            labels.push(Label {
                task: "kind".into(),
                label: k.to_string(),
                confidence: 1.0,
                source: "rules".into(),
                evidence: vec![format!("conventional prefix: {k}:")],
            });
        }

        // --- is_fix: prefijo conventional "fix", Link{rel:"reverts"} o fix_keywords. ---
        let title_lower = ev.title.to_lowercase();
        let body_lower = ev.body.to_lowercase();

        let has_revert_link = ev.links.iter().any(|l| l.rel == "reverts");
        let is_conventional_fix = kind == Some("fix");

        let matched_keyword = self.cfg.fix_keywords.iter().find(|kw| {
            let kwl = kw.to_lowercase();
            !kwl.is_empty() && (title_lower.contains(&kwl) || body_lower.contains(&kwl))
        });

        let (is_fix, confidence, evidence) = if is_conventional_fix {
            (true, 1.0, vec!["conventional prefix: fix:".to_string()])
        } else if has_revert_link {
            (true, 1.0, vec!["link rel=reverts".to_string()])
        } else if let Some(kw) = matched_keyword {
            (true, 0.8, vec![format!("fix_keyword: {kw}")])
        } else {
            (false, 1.0, Vec::new())
        };

        labels.push(Label {
            task: "is_fix".into(),
            label: is_fix.to_string(),
            confidence,
            source: "rules".into(),
            evidence,
        });

        // --- bug_category: solo si is_fix, primera categoría cuya keyword aparezca. ---
        if is_fix {
            let cleaned = clean_body(&ev.body).to_lowercase();
            let text = format!("{title_lower}\n{cleaned}");
            'cats: for (category, keywords) in &self.cfg.bug_categories {
                for kw in keywords {
                    let kwl = kw.to_lowercase();
                    if !kwl.is_empty() && text.contains(&kwl) {
                        labels.push(Label {
                            task: "bug_category".into(),
                            label: category.clone(),
                            confidence: 0.7,
                            source: "rules".into(),
                            evidence: vec![kw.clone()],
                        });
                        break 'cats;
                    }
                }
            }
        }

        labels
    }

    fn name(&self) -> &str {
        "rules"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_core::Link;

    fn ev(title: &str, body: &str) -> Event {
        Event {
            title: title.into(),
            body: body.into(),
            ..Default::default()
        }
    }

    fn find<'a>(labels: &'a [Label], task: &str) -> Option<&'a Label> {
        labels.iter().find(|l| l.task == task)
    }

    #[test]
    fn conventional_fix_con_categoria_memory_safety() {
        let c = RulesClassifier::new(Config::defaults());
        let labels = c.classify(&ev("fix: segfault on connect", ""));

        let kind = find(&labels, "kind").expect("kind presente");
        assert_eq!(kind.label, "fix");
        assert_eq!(kind.confidence, 1.0);

        let is_fix = find(&labels, "is_fix").expect("is_fix presente");
        assert_eq!(is_fix.label, "true");
        assert_eq!(is_fix.confidence, 1.0);

        // "segfault" está en bug_categories["memory-safety"] (no en "crash").
        let cat = find(&labels, "bug_category").expect("bug_category presente");
        assert_eq!(cat.label, "memory-safety");
        assert_eq!(cat.evidence, vec!["segfault".to_string()]);
    }

    #[test]
    fn revert_por_link_marca_is_fix() {
        let c = RulesClassifier::new(Config::defaults());
        let mut e = ev("Revert \"feat: add cache\"", "This reverts commit abc1234.");
        e.links.push(Link { rel: "reverts".into(), target: "abc1234".into() });
        let labels = c.classify(&e);

        let is_fix = find(&labels, "is_fix").expect("is_fix presente");
        assert_eq!(is_fix.label, "true");
        assert_eq!(is_fix.confidence, 1.0);
        assert_eq!(is_fix.evidence, vec!["link rel=reverts".to_string()]);
    }

    #[test]
    fn feat_no_es_fix() {
        let c = RulesClassifier::new(Config::defaults());
        let labels = c.classify(&ev("feat: add cache", "Adds an in-memory LRU cache."));

        let kind = find(&labels, "kind").expect("kind presente");
        assert_eq!(kind.label, "feat");

        let is_fix = find(&labels, "is_fix").expect("is_fix presente");
        assert_eq!(is_fix.label, "false");

        assert!(find(&labels, "bug_category").is_none());
    }

    #[test]
    fn mensaje_sin_senales_no_produce_kind_ni_fix() {
        let c = RulesClassifier::new(Config::defaults());
        let labels = c.classify(&ev(
            "Update readme with usage examples",
            "Just docs polish, nothing to see here.",
        ));

        assert!(find(&labels, "kind").is_none());
        let is_fix = find(&labels, "is_fix").expect("is_fix presente");
        assert_eq!(is_fix.label, "false");
        assert!(find(&labels, "bug_category").is_none());
    }

    #[test]
    fn fix_keyword_sin_prefijo_conventional_da_confianza_08() {
        let c = RulesClassifier::new(Config::defaults());
        let labels = c.classify(&ev("Correct broken pagination on the users endpoint", ""));

        let is_fix = find(&labels, "is_fix").expect("is_fix presente");
        assert_eq!(is_fix.label, "true");
        assert_eq!(is_fix.confidence, 0.8);
        assert!(find(&labels, "kind").is_none());
    }

    #[test]
    fn load_respeta_config_json_parcial() {
        let dir = std::env::temp_dir().join(format!(
            "chrono-classify-rules-test-{}-{}",
            std::process::id(),
            "load_respeta_config_json_parcial"
        ));
        let chrono_dir = dir.join(".chrono");
        std::fs::create_dir_all(&chrono_dir).unwrap();
        std::fs::write(
            chrono_dir.join("config.json"),
            r#"{"fix_keywords":["kaboom"]}"#,
        )
        .unwrap();

        let cfg = load(&dir);
        assert_eq!(cfg.fix_keywords, vec!["kaboom".to_string()]);
        // Campos ausentes del JSON conservan el default.
        assert_eq!(cfg.bug_labels, Config::defaults().bug_labels);
        assert!(!cfg.bug_categories.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_sin_config_usa_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "chrono-classify-rules-test-{}-{}",
            std::process::id(),
            "load_sin_config_usa_defaults"
        ));
        std::fs::remove_dir_all(&dir).ok();
        let cfg = load(&dir);
        assert_eq!(cfg, Config::defaults());
    }
}
