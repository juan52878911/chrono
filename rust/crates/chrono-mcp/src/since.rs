//! Parseo de `since` a epoch UTC: el de `chrono_core::time` (el mismo que el
//! CLI y las fuentes).

pub use chrono_core::time::parse_since;

/// `since` textual (vacío = sin ventana, 0) -> epoch para las consultas de
/// `chrono-metrics`. Un `since` mal formado ya lo rechazó
/// `crate::args::validate` con un error, como el CLI.
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
