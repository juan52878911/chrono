//! Parseo de `since` a epoch UTC: el de `chrono_core::time` (el mismo que el
//! CLI y las fuentes).

pub use chrono_core::time::parse_since;

/// `since` textual (posiblemente vacío o inválido) -> epoch para las
/// consultas de `chrono-metrics`. A diferencia del CLI, el MCP no aborta la
/// llamada por un `since` mal formado: simplemente ignora la ventana (0).
pub fn since_to_epoch(s: &str) -> i64 {
    if s.is_empty() {
        return 0;
    }
    parse_since(s).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_fechas_simples() {
        assert_eq!(parse_since("2024-01-01"), Some(1_704_067_200_000));
        assert_eq!(parse_since("1970-01-01"), Some(0));
        assert_eq!(parse_since("ayer"), None);
        assert_eq!(since_to_epoch(""), 0);
        assert_eq!(since_to_epoch("no-es-fecha"), 0);
    }
}
