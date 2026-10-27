//! Tiempo desde valores JSON. Las fechas de texto y la conversión a ISO-8601
//! son las de `chrono_core::time` (una sola implementación para todas las
//! fuentes, con rangos validados); aquí solo queda lo propio de JSON: números
//! epoch en s/ms/µs/ns y cadenas de cifras.

use serde_json::Value;

pub use chrono_core::time::epoch_to_iso8601;
use chrono_core::time::parse_rfc3339;

/// 0000-01-01 y 9999-12-31T23:59:59.999 en ms: fuera, la unidad no era esa.
const MIN_MS: i64 = -62_167_219_200_000;
const MAX_MS: i64 = 253_402_300_799_999;

/// Epoch en MILISEGUNDOS a partir de un número JSON. La unidad sale de la
/// magnitud (hoy: s ≈ 1.7e9, ms ≈ 1.7e12, µs ≈ 1.7e15, ns ≈ 1.7e18); antes
/// todo lo que pasaba de 1e12 se tomaba por ms y un epoch en µs o ns daba el
/// año 55 969. Un valor fuera de los años 0000–9999 es `None`, no una fecha
/// absurda.
fn epoch_from_number(n: &serde_json::Number) -> Option<i64> {
    let ms = match n.as_i64() {
        // Enteros sin pasar por f64: un epoch en ns no cabe exacto en él.
        Some(i) => match i.unsigned_abs() {
            a if a > 100_000_000_000_000_000 => i / 1_000_000,
            a if a > 100_000_000_000_000 => i / 1_000,
            a if a > 1_000_000_000_000 => i,
            _ => i * 1000,
        },
        None => {
            let raw = n.as_f64().filter(|f| f.is_finite())?;
            let ms = match raw.abs() {
                a if a > 1e17 => raw / 1e6,
                a if a > 1e14 => raw / 1e3,
                a if a > 1e12 => raw,
                _ => raw * 1000.0,
            };
            // `as` satura: lo enorme cae fuera del rango de abajo.
            ms.round() as i64
        }
    };
    (MIN_MS..=MAX_MS).contains(&ms).then_some(ms)
}

/// Extrae el epoch UTC en MILISEGUNDOS de un valor JSON de tiempo: número
/// (epoch en s, ms, µs o ns, también como cadena de cifras) o string
/// RFC3339/ISO-8601. `None` si no reconoce el valor (el evento entra igual,
/// con `at_epoch=0` y `at=""`, ver `record.rs`).
pub fn parse_time_value(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => epoch_from_number(n),
        Value::String(s) => parse_rfc3339(s).or_else(|| epoch_from_digits(s)),
        _ => None,
    }
}

/// `"1704067200"`, `"1704067200.123"`: un epoch escrito como cadena. Al
/// menos 9 cifras enteras (de 1973 en adelante en segundos) para no leer como
/// epoch una fecha compacta (`"20240101"`) ni un número cualquiera.
fn epoch_from_digits(s: &str) -> Option<i64> {
    let s = s.trim();
    let int = s.split_once('.').map_or(s, |(i, _)| i);
    let int = int.strip_prefix('-').unwrap_or(int);
    if int.len() < 9 || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.' || b == b'-') {
        return None;
    }
    epoch_from_number(&s.parse().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_cero_es_epoch() {
        assert_eq!(epoch_to_iso8601(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn parse_rfc3339_con_z() {
        assert_eq!(parse_rfc3339("2024-01-01T00:00:00Z"), Some(1_704_067_200_000));
    }

    #[test]
    fn parse_rfc3339_con_offset_positivo() {
        // 2024-01-01T02:00:00+02:00 == 2024-01-01T00:00:00Z
        assert_eq!(parse_rfc3339("2024-01-01T02:00:00+02:00"), Some(1_704_067_200_000));
    }

    #[test]
    fn parse_rfc3339_con_fraccion() {
        // La fracción se conserva hasta ms: +123 ms.
        assert_eq!(parse_rfc3339("2024-01-01T00:00:00.123Z"), Some(1_704_067_200_123));
    }

    /// Tabla de fechas imposibles en un campo de tiempo JSON: ninguna da un
    /// instante (antes `25:00` y el 30 de febrero pasaban a otra fecha).
    #[test]
    fn parse_time_value_fechas_imposibles() {
        for bad in [
            "2024-13-40T00:00:00Z",
            "2024-01-01T25:00:00Z",
            "2024-01-01T23:60:00Z",
            "2024-01-01T23:59:61Z",
            "2024-02-30T10:00:00Z",
            "2023-02-29T10:00:00Z",
            "2024-04-31T10:00:00+02:00",
            "2024-01-01T10:00:00+24:00",
            "no es una fecha",
        ] {
            assert_eq!(parse_time_value(&Value::from(bad)), None, "{bad:?}");
        }
        assert_eq!(parse_time_value(&Value::from("2024-02-29T00:00:00Z")), Some(1_709_164_800_000));
    }

    #[test]
    fn epoch_desde_numero_segundos_y_milis() {
        // Entrada en segundos -> ×1000; entrada ya en ms -> tal cual. Ambas a ms.
        assert_eq!(parse_time_value(&Value::from(1_704_067_200i64)), Some(1_704_067_200_000));
        assert_eq!(parse_time_value(&Value::from(1_704_067_200_000i64)), Some(1_704_067_200_000));
    }
}
