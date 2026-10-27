//! Reloj y duraciones del CLI. Las fechas (epoch UTC ⇄ civil, `--since`) son
//! las de `chrono_core::time`, compartidas con todas las fuentes.
//! UNIDAD CANÓNICA: **milisegundos** UTC desde epoch.

/// Milisegundos UTC desde epoch, ahora. Si `CHRONO_NOW` está fijado a un entero
/// de SEGUNDOS epoch (más cómodo de escribir a mano), se usa ese valor ×1000:
/// permite ingestas reproducibles byte a byte (el `.db` guarda
/// `sources.last_sync_at`, que si no varía con el reloj). Para tests de
/// determinismo y builds reproducibles.
pub fn now_epoch() -> i64 {
    if let Ok(v) = std::env::var("CHRONO_NOW") {
        if let Ok(secs) = v.trim().parse::<i64>() {
            return secs * 1000;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub use chrono_core::time::{epoch_to_iso8601, parse_since};

/// Parsea una duración a MILISEGUNDOS: entero de segundos (`3600`) o con sufijo
/// `s`/`m`/`h`/`d` (`90s`, `30m`, `1h`, `7d`), o milisegundos con `ms`
/// (`500ms`). Devuelve ms, o `None` si no lo entiende. Usado por `--bucket`
/// (timeline) y `--delta` (correlate).
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Sufijo en ms (`500ms`): se comprueba antes que `s` suelto.
    if let Some(num) = s.strip_suffix("ms").or_else(|| s.strip_suffix("MS")) {
        let n: i64 = num.trim().parse().ok()?;
        return if n > 0 { Some(n) } else { None };
    }
    let (num, mult_secs) = match s.as_bytes()[s.len() - 1] {
        b's' | b'S' => (&s[..s.len() - 1], 1),
        b'm' | b'M' => (&s[..s.len() - 1], 60),
        b'h' | b'H' => (&s[..s.len() - 1], 3600),
        b'd' | b'D' => (&s[..s.len() - 1], 86_400),
        _ => (s, 1),
    };
    let n: i64 = num.trim().parse().ok()?;
    if n <= 0 {
        return None;
    }
    n.checked_mul(mult_secs * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--bucket`/`--delta`/`--gap`: desbordes, signos, unicode y unidades raras.
    #[test]
    fn duraciones_bordes() {
        for (s, want) in [(" 5 m ", Some(300_000)), ("1H", Some(3_600_000)), ("2D", Some(172_800_000)), ("1MS", Some(1))] {
            assert_eq!(parse_duration(s), want, "{s:?}");
        }
        for bad in [
            "1.5h", "1e3", "+", "-1h", "0ms", "-5ms", "ms", "s", "1w", "1y", "1hh", "h1", "5é", "😀", "١h", "9223372036854775807d",
            "9223372036854775807ms0", "106751991167301d", " ", "\u{0}",
        ] {
            assert_eq!(parse_duration(bad), None, "{bad:?} debía rechazarse");
        }
        assert_eq!(parse_duration("9223372036854775807ms"), Some(i64::MAX));
    }

    #[test]
    fn ida_y_vuelta() {
        // Todo en MILISEGUNDOS.
        assert_eq!(parse_since("2024-01-01"), Some(1_704_067_200_000));
        assert_eq!(epoch_to_iso8601(1_704_067_200_000), "2024-01-01T00:00:00Z");
        assert_eq!(epoch_to_iso8601(1_704_067_200_123), "2024-01-01T00:00:00.123Z");
        assert_eq!(parse_since("2024-01-01T01:00:00Z"), Some(1_704_070_800_000));
        assert_eq!(parse_since("1970-01-01"), Some(0));
        assert_eq!(parse_since("ayer"), None);
        assert_eq!(parse_since("2024-13-01"), None);
    }

    #[test]
    fn duraciones() {
        assert_eq!(parse_duration("3600"), Some(3_600_000));
        assert_eq!(parse_duration("90s"), Some(90_000));
        assert_eq!(parse_duration("30m"), Some(1_800_000));
        assert_eq!(parse_duration("1h"), Some(3_600_000));
        assert_eq!(parse_duration("7d"), Some(604_800_000));
        assert_eq!(parse_duration("500ms"), Some(500));
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("-5"), None);
        assert_eq!(parse_duration("h"), None);
        assert_eq!(parse_duration(""), None);
    }
}
