//! Nivel deducido del texto de un log que llega sin nivel estructurado.
//!
//! Muchos logs no traen el campo (el colector no supo extraerlo, o es una línea
//! de texto de Postgres/pgbouncer). Las cifras por nivel deben seguir siendo las
//! de la fuente, así que esto NO rellena `level`: el writer lo guarda aparte, en
//! `attrs.level_inferred`, y las consultas lo usan solo si se pide.
//!
//! Conservador y determinista: una palabra de nivel en MAYÚSCULAS seguida de
//! `:` `]` o espacio (`FATAL:`, `[ERROR]`, `WARNING  x`), o `level=error` /
//! `"level":"error"`, en los primeros 200 caracteres de la primera línea. Gana
//! la primera que aparece.

const UPPER: &[(&str, &str)] = &[
    ("PANIC", "FATAL"),
    ("FATAL", "FATAL"),
    ("CRITICAL", "CRITICAL"),
    ("CRIT", "CRITICAL"),
    ("ERROR", "ERROR"),
    ("WARNING", "WARNING"),
    ("WARN", "WARNING"),
];

/// Nivel normalizado (`FATAL`, `CRITICAL`, `ERROR`, `WARNING`) o `None`.
pub fn infer_level(text: &str) -> Option<&'static str> {
    let line = text.lines().next().unwrap_or("");
    let head: String = line.chars().take(200).collect();
    let bytes = head.as_bytes();
    let mut best: Option<(usize, &'static str)> = None;
    for (word, norm) in UPPER {
        let mut from = 0;
        while let Some(pos) = head[from..].find(word) {
            let at = from + pos;
            let end = at + word.len();
            let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
            let after_ok = end == bytes.len() || matches!(bytes[end], b':' | b']' | b' ' | b'\t' | b'|' | b'>');
            if before_ok && after_ok {
                if best.map_or(true, |(p, _)| at < p) {
                    best = Some((at, norm));
                }
                break;
            }
            from = end;
        }
    }
    if best.is_none() {
        let low = head.to_ascii_lowercase();
        for (key, norm) in [("fatal", "FATAL"), ("critical", "CRITICAL"), ("error", "ERROR"), ("warning", "WARNING"), ("warn", "WARNING")] {
            for pat in [format!("level={key}"), format!("\"level\":\"{key}\""), format!("\"level\": \"{key}\"")] {
                if let Some(p) = low.find(&pat) {
                    let end = p + pat.len();
                    if pat.ends_with('"') || end == low.len() || !low.as_bytes()[end].is_ascii_alphanumeric() {
                        if best.map_or(true, |(b, _)| p < b) {
                            best = Some((p, norm));
                        }
                    }
                }
            }
        }
    }
    best.map(|(_, n)| n)
}

#[cfg(test)]
mod tests {
    use super::infer_level;

    #[test]
    fn deduce_niveles_tipicos() {
        assert_eq!(infer_level("2026-10-14 19:07:35.936 UTC [128] ERROR:  catalog version mismatch"), Some("ERROR"));
        assert_eq!(infer_level("FATAL:  terminating connection due to administrator command"), Some("FATAL"));
        assert_eq!(infer_level("[WARN] disk almost full"), Some("WARNING"));
        assert_eq!(infer_level("ts=1 level=error msg=boom"), Some("ERROR"));
        assert_eq!(infer_level(r#"{"level":"warning","msg":"x"}"#), Some("WARNING"));
        assert_eq!(infer_level("CRITICAL:shared.rls_politica:#####"), Some("CRITICAL"));
    }

    #[test]
    fn no_inventa() {
        assert_eq!(infer_level("GET /api/errors 200 OK"), None);
        assert_eq!(infer_level("ERRORS_TOTAL increased"), None);
        assert_eq!(infer_level("the error was handled"), None);
        assert_eq!(infer_level("terror: fatalism"), None);
        assert_eq!(infer_level(""), None);
    }

    #[test]
    fn gana_la_primera() {
        assert_eq!(infer_level("WARNING: retrying after ERROR: timeout"), Some("WARNING"));
    }
}
