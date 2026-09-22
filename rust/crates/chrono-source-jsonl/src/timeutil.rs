//! Conversión epoch UNIX <-> ISO-8601 UTC y parseo manual de tiempo, sin
//! dependencias externas (mismo algoritmo civil de Howard Hinnant que usa
//! `chrono-source-git::timeutil`, duplicado aquí porque cada adaptador es un
//! crate independiente).

use serde_json::Value;

/// Convierte segundos UTC desde epoch a "YYYY-MM-DDThh:mm:ssZ".
pub fn epoch_to_iso8601(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs_of_day = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Días desde 1970-01-01 -> (año, mes, día). Algoritmo civil de Hinnant.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// (año, mes, día) -> días desde 1970-01-01. Inversa de `civil_from_days`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as u64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe as i64 - 719_468
}

/// Parsea a mano una fecha RFC3339/ISO-8601 ("YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]")
/// y devuelve el epoch UTC en segundos, o `None` si no encaja el formato.
/// Sin offset explícito se asume UTC.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let byte_at = |i: usize| bytes.get(i).copied();

    let y: i64 = s.get(0..4)?.parse().ok()?;
    if byte_at(4)? != b'-' {
        return None;
    }
    let mo: u32 = s.get(5..7)?.parse().ok()?;
    if byte_at(7)? != b'-' {
        return None;
    }
    let d: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    match byte_at(10)? {
        b'T' | b't' | b' ' => {}
        _ => return None,
    }
    let hh: i64 = s.get(11..13)?.parse().ok()?;
    if byte_at(13)? != b':' {
        return None;
    }
    let mm: i64 = s.get(14..16)?.parse().ok()?;
    if byte_at(16)? != b':' {
        return None;
    }
    let ss: i64 = s.get(17..19)?.parse().ok()?;

    let mut idx = 19;
    // Fracción de segundo opcional (".fff..."): se descarta (at_epoch es entero).
    if byte_at(idx) == Some(b'.') {
        idx += 1;
        while byte_at(idx).is_some_and(|c| c.is_ascii_digit()) {
            idx += 1;
        }
    }
    let rest = s.get(idx..)?;

    let offset_secs: i64 = if rest.is_empty() || rest.eq_ignore_ascii_case("z") {
        0 // sin zona horaria explícita, o "Z": se asume/es UTC.
    } else {
        let sign: i64 = match rest.as_bytes()[0] {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let rest2 = &rest[1..];
        let (oh, om): (i64, i64) = if rest2.len() >= 5 && rest2.as_bytes()[2] == b':' {
            (rest2.get(0..2)?.parse().ok()?, rest2.get(3..5)?.parse().ok()?)
        } else if rest2.len() >= 4 {
            (rest2.get(0..2)?.parse().ok()?, rest2.get(2..4)?.parse().ok()?)
        } else if rest2.len() == 2 {
            (rest2.parse().ok()?, 0)
        } else {
            return None;
        };
        sign * (oh * 3600 + om * 60)
    };

    let days = days_from_civil(y, mo, d);
    let local_secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    Some(local_secs - offset_secs)
}

/// Epoch en segundos a partir de un número JSON: soporta epoch-segundos y
/// epoch-milisegundos (heurística: |valor| > 1e12 -> son milisegundos).
fn epoch_from_number(n: &serde_json::Number) -> Option<i64> {
    let raw = n.as_f64()?;
    let secs = if raw.abs() > 1e12 { raw / 1000.0 } else { raw };
    Some(secs as i64)
}

/// Extrae el epoch UTC en segundos de un valor JSON de tiempo: número
/// (epoch-seg o epoch-ms) o string RFC3339/ISO-8601. `None` si no reconoce el
/// valor (el evento entra igual, con `at_epoch=0` y `at=""`, ver `record.rs`).
pub fn parse_time_value(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => epoch_from_number(n),
        Value::String(s) => parse_rfc3339(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_cero_es_epoch() {
        assert_eq!(epoch_to_iso8601(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn dias_civil_es_inversa_de_civil_from_days() {
        for epoch in [0i64, 951_868_800, 1_704_067_200, -86_400, 1_600_000_000] {
            let days = epoch.div_euclid(86_400);
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "epoch={epoch}");
        }
    }

    #[test]
    fn parse_rfc3339_con_z() {
        assert_eq!(parse_rfc3339("2024-01-01T00:00:00Z"), Some(1_704_067_200));
    }

    #[test]
    fn parse_rfc3339_con_offset_positivo() {
        // 2024-01-01T02:00:00+02:00 == 2024-01-01T00:00:00Z
        assert_eq!(parse_rfc3339("2024-01-01T02:00:00+02:00"), Some(1_704_067_200));
    }

    #[test]
    fn parse_rfc3339_con_fraccion() {
        assert_eq!(parse_rfc3339("2024-01-01T00:00:00.123Z"), Some(1_704_067_200));
    }

    #[test]
    fn parse_rfc3339_invalido() {
        assert_eq!(parse_rfc3339("no es una fecha"), None);
        assert_eq!(parse_rfc3339("2024-13-40T00:00:00Z"), None);
    }

    #[test]
    fn epoch_desde_numero_segundos_y_milis() {
        assert_eq!(parse_time_value(&Value::from(1_704_067_200i64)), Some(1_704_067_200));
        assert_eq!(parse_time_value(&Value::from(1_704_067_200_000i64)), Some(1_704_067_200));
    }
}
