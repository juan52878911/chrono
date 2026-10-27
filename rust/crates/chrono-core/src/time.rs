//! Fechas sin dependencias: epoch UTC ⇄ civil (algoritmo de Howard Hinnant) y
//! el ÚNICO parser de fechas de texto de chrono. UNIDAD CANÓNICA:
//! **milisegundos** UTC desde epoch (`at_epoch` es ms en todo el dominio).
//!
//! Antes cada adaptador (jsonl, csv, textlog, template, changelog, oslog, git)
//! y el CLI tenían su copia; las de las fuentes aceptaban `25:00:00` o el 30 de
//! febrero y lo convertían en otra fecha válida. Ahora todos parsean aquí con
//! los mismos rangos que `--since`/`--until`: una fecha u hora imposible es
//! `None` y el evento entra sin tiempo (`at_epoch=0`), como lo ilegible.

/// "YYYY-MM-DDThh:mm:ss[.mmm]Z" a partir de MILISEGUNDOS UTC. La parte de
/// milisegundos solo aparece cuando no es cero (así las fuentes con resolución
/// de segundo —git, syslog— mantienen la salida limpia "…ssZ").
pub fn epoch_to_iso8601(epoch_ms: i64) -> String {
    let days = epoch_ms.div_euclid(86_400_000);
    let ms_of_day = epoch_ms.rem_euclid(86_400_000);
    let secs = ms_of_day / 1000;
    let millis = ms_of_day % 1000;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if millis == 0 {
        format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
    } else {
        format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
    }
}

/// Días desde 1970-01-01 -> (año, mes, día).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (año, mes, día) -> días desde 1970-01-01. Inversa de `civil_from_days`.
/// No valida: con un día fuera de rango da OTRA fecha (el 30 de febrero es el
/// 1 o 2 de marzo). Para datos de fuera, `days_from_civil_checked`.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as u64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe as i64 - 719_468
}

/// Días del mes `m` (1–12) del año `y`, con bisiestos gregorianos.
pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `days_from_civil` solo si la fecha existe (mes 1–12, día dentro del mes).
pub fn days_from_civil_checked(y: i64, m: u32, d: u32) -> Option<i64> {
    ((1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)).then(|| days_from_civil(y, m, d))
}

/// Segundos del día de `hh:mm:ss` si la hora existe (`ss` hasta 60: un segundo
/// intercalar cuenta como el 59). Fuera de rango: `None`.
pub fn secs_of_day_checked(hh: i64, mm: i64, ss: i64) -> Option<i64> {
    ((0..=23).contains(&hh) && (0..=59).contains(&mm) && (0..=60).contains(&ss)).then(|| hh * 3600 + mm * 60 + ss.min(59))
}

/// Parsea una fecha ISO-8601 flexible: `YYYY-MM-DD` (medianoche UTC) o
/// `YYYY-MM-DD[T ]hh[:mm[:ss[.fff]]]` con `Z` o un desfase (`+02:00`, `-0500`,
/// `+02`) opcionales; sin zona es UTC. Lo que escribe chrono
/// (`…T10:00:00.123Z`) vuelve a entrar. Una fecha u hora fuera de rango
/// (`2024-02-30`, `25:00`) o cualquier otra cosa: `None`. Es la de
/// `--since`/`--until`.
pub fn parse_since(s: &str) -> Option<i64> {
    let s = s.trim();
    if !s.is_ascii() {
        return None;
    }
    let (date, time) = match s.find(['T', 't', ' ']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s.strip_suffix(['Z', 'z']).unwrap_or(s), None),
    };
    let mut dp = date.split('-');
    let y = digits(dp.next()?, 4, 4)?;
    let m = digits(dp.next()?, 1, 2)?;
    let d = digits(dp.next()?, 1, 2)?;
    if dp.next().is_some() {
        return None;
    }
    let days = days_from_civil_checked(y, m as u32, d as u32)?;
    let mut ms = 0i64;
    if let Some(t) = time {
        let (t, offset_secs) = split_zone(t)?;
        let (hms, frac) = match t.split_once(['.', ',']) {
            Some((a, f)) => (a, Some(f)),
            None => (t, None),
        };
        let mut parts = hms.split(':');
        let hh = digits(parts.next()?, 1, 2)?;
        let mm = parts.next().map_or(Some(0), |p| digits(p, 2, 2))?;
        let ss = parts.next().map_or(Some(0), |p| digits(p, 2, 2))?;
        if parts.next().is_some() {
            return None;
        }
        let secs = secs_of_day_checked(hh, mm, ss)?;
        let frac_ms = match frac {
            // Más de tres cifras: se truncan a milisegundos.
            Some(f) => digits(&format!("{:0<3}", &f[..f.len().min(3)]), 3, 3).filter(|_| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))?,
            None => 0,
        };
        ms = (secs - offset_secs) * 1000 + frac_ms;
    }
    Some(days * 86_400_000 + ms)
}

/// Fecha y hora RFC3339/ISO-8601 COMPLETAS (`YYYY-MM-DD[Tt ]HH:MM:SS[.fff][Z|±HH[:MM]]`)
/// -> epoch UTC en ms. A diferencia de `parse_since`, exige la hora con
/// segundos en posiciones fijas: lo usan las fuentes, donde una fecha suelta
/// (`"2024-01-01"`) en un campo cualquiera no debe tomarse por un instante.
/// Mismos rangos que `parse_since`.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let fixed = b.len() >= 19
        && b[4] == b'-'
        && b[7] == b'-'
        && matches!(b[10], b'T' | b't' | b' ')
        && b[13] == b':'
        && b[16] == b':'
        && [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18].iter().all(|&i| b[i].is_ascii_digit());
    if !fixed || s.trim() != s {
        return None;
    }
    parse_since(s)
}

/// Epoch UTC en ms de un valor de tiempo EN TEXTO: primero como epoch
/// entero/decimal (se asume en segundos y se pasa a ms, salvo que el valor ya
/// parezca ms —|v| > 1e12—, en cuyo caso se toma tal cual), y si no parsea
/// como número, como RFC3339/ISO-8601. `None` si nada casa (el evento entra
/// igual, con `at_epoch=0` y `at=""`).
pub fn parse_time_str(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(n) = t.parse::<i64>() {
        return Some(if n.abs() > 1_000_000_000_000 { n } else { n * 1000 });
    }
    if let Ok(f) = t.parse::<f64>() {
        if f.is_finite() {
            let ms = if f.abs() > 1e12 { f } else { f * 1000.0 };
            return Some(ms as i64);
        }
    }
    parse_rfc3339(t)
}

/// Entre `min` y `max` cifras ASCII, sin signo.
fn digits(s: &str, min: usize, max: usize) -> Option<i64> {
    (s.len() >= min && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

/// Separa la zona del final de la hora: `Z`, `±hh`, `±hhmm` o `±hh:mm` →
/// segundos a restar para llegar a UTC. Una zona mal formada: `None`.
fn split_zone(t: &str) -> Option<(&str, i64)> {
    if let Some(base) = t.strip_suffix(['Z', 'z']) {
        return Some((base, 0));
    }
    let Some(p) = t.rfind(['+', '-']) else { return Some((t, 0)) };
    let off = &t[p + 1..];
    let (h, m) = match off.len() {
        2 => (off, "00"),
        4 => (&off[..2], &off[2..]),
        5 if off.as_bytes()[2] == b':' => (&off[..2], &off[3..]),
        _ => return None,
    };
    let (h, m) = (digits(h, 2, 2)?, digits(m, 2, 2)?);
    if h > 23 || m > 59 {
        return None;
    }
    let secs = h * 3600 + m * 60;
    Some((&t[..p], if t.as_bytes()[p] == b'+' { secs } else { -secs }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--since`/`--until` con zona, fracción, fechas imposibles y basura.
    #[test]
    fn since_bordes() {
        let base = 1_704_067_200_000; // 2024-01-01T00:00:00Z
        let ok: &[(&str, i64)] = &[
            ("2024-01-01", base),
            ("2024-01-01Z", base),
            (" 2024-01-01 ", base),
            ("2024-1-1", base),
            ("2024-01-01T10", base + 36_000_000),
            ("2024-01-01T10:00", base + 36_000_000),
            ("2024-01-01T10:00:00Z", base + 36_000_000),
            ("2024-01-01t10:00:00z", base + 36_000_000),
            ("2024-01-01 10:00:00", base + 36_000_000),
            ("2024-01-01T10:00:00.123Z", base + 36_000_123),
            ("2024-01-01T10:00:00,5Z", base + 36_000_500),
            ("2024-01-01T10:00:00.123456789Z", base + 36_000_123),
            ("2024-01-01T12:00:00+02:00", base + 36_000_000),
            ("2024-01-01T12:00:00+0200", base + 36_000_000),
            ("2024-01-01T12:00:00+02", base + 36_000_000),
            ("2024-01-01T05:00:00-05:00", base + 36_000_000),
            ("2024-01-01T00:30:00.250+01:00", base - 1_800_000 + 250),
            ("2024-02-29", base + 59 * 86_400_000),
            ("2000-02-29", 951_782_400_000),
            ("0000-01-01", -62_167_219_200_000),
            ("9999-12-31", 253_402_214_400_000),
        ];
        for (s, want) in ok {
            assert_eq!(parse_since(s), Some(*want), "{s:?}");
        }
        for bad in [
            "", "ayer", "24h", "now-1h", "2024", "2024-01", "2024-01-01-01", "2024-13-01", "2024-00-10", "2024-01-00",
            "2024-01-32", "2023-02-29", "1900-02-29", "2024-04-31", "2024-01-01T", "2024-01-01T24:00:00", "2024-01-01T10:60",
            "2024-01-01T10:00:61", "2024-01-01T-1:00", "2024-01-01T10:00:00.", "2024-01-01T10:00:00.1a", "2024-01-01T1:2:3:4",
            "2024-01-01T10:00:00+2", "2024-01-01T10:00:00+25:00", "2024-01-01T10:00:00+02:60", "2024-01-01T10:00:00+02:00:00",
            "+2024-01-01", "-2024-01-01", "99999-01-01", "9223372036854775807-01-01", "2024-01-01T10:00:00Zjunk",
            "２０２４-01-01", "2024-01-01T10:00:00＋02:00", "2024-01-01T😀", "2024‐01‐01",
        ] {
            assert_eq!(parse_since(bad), None, "{bad:?} debía rechazarse");
        }
    }

    /// Lo que escribe chrono vuelve a entrar igual, en todo el rango.
    #[test]
    fn since_ida_y_vuelta_aleatoria() {
        let mut x: u64 = 0x5eed_0007;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let ms = (x % 253_402_300_799_999) as i64; // 1970..9999
            let iso = epoch_to_iso8601(ms);
            assert_eq!(parse_since(&iso), Some(ms), "{iso}");
            assert_eq!(parse_rfc3339(&iso), Some(ms), "{iso}");
            let neg = -((x % 62_167_219_200_000) as i64); // 0000..1970
            assert_eq!(parse_since(&epoch_to_iso8601(neg)), Some(neg), "{}", epoch_to_iso8601(neg));
        }
    }

    #[test]
    fn dias_civil_es_inversa_de_civil_from_days() {
        for days in [0i64, 11_016, 19_723, -1, 18_518, -719_528, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
            assert_eq!(days_from_civil_checked(y, m, d), Some(days));
        }
    }

    /// Tabla del parser que usan las fuentes: lo válido entra a ms exactos,
    /// lo imposible (hora 24+, minuto 60, 30 de febrero, 31 de abril, mes 13,
    /// desfase 25 h) y lo que no tiene hora completa son `None`.
    #[test]
    fn rfc3339_tabla() {
        let base = 1_704_067_200_000; // 2024-01-01T00:00:00Z
        for (s, want) in [
            ("2024-01-01T00:00:00Z", base),
            ("2024-01-01T02:00:00+02:00", base),
            ("2024-01-01T00:00:00.123Z", base + 123),
            ("2024-01-01T00:00:00.5Z", base + 500),
            ("2024-01-01 00:00:00", base),
            ("2024-01-01T00:00:00+0000", base),
            ("2023-12-31 19:00:00.000000-0500", base),
            ("2024-02-29T00:00:00Z", base + 59 * 86_400_000),
            ("2024-01-01T23:59:60Z", base + 86_399_000),
        ] {
            assert_eq!(parse_rfc3339(s), Some(want), "{s:?}");
            assert_eq!(parse_time_str(s), Some(want), "{s:?}");
        }
        for bad in [
            "2024-01-01T25:00:00Z", "2024-01-01T24:00:00Z", "2024-01-01T10:60:00Z", "2024-01-01T10:00:61Z", "2024-02-30T00:00:00Z",
            "2023-02-29T00:00:00Z", "2024-04-31T00:00:00Z", "2024-13-01T00:00:00Z", "2024-00-01T00:00:00Z", "2024-01-00T00:00:00Z",
            "2024-01-01T00:00:00+25:00", "2024-01-01", "2024-01-01T10:00", "2024-1-1T10:00:00Z",
            "2024-01-01T00:00:00Zbasura", "no es una fecha", "",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad:?} debía rechazarse");
            assert_eq!(parse_time_str(bad), None, "{bad:?} debía rechazarse");
        }
    }

    #[test]
    fn parse_time_str_epoch_segundos_y_milis() {
        assert_eq!(parse_time_str("1704067200"), Some(1_704_067_200_000));
        assert_eq!(parse_time_str("1704067200000"), Some(1_704_067_200_000));
    }

    #[test]
    fn hora_y_fecha_comprobadas() {
        assert_eq!(secs_of_day_checked(23, 59, 60), Some(86_399));
        assert_eq!(secs_of_day_checked(24, 0, 0), None);
        assert_eq!(days_from_civil_checked(2024, 2, 30), None);
        assert_eq!(days_from_civil_checked(2024, 2, 29), Some(19_782));
    }
}
