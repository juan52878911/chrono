//! Fechas sin dependencias: epoch UTC ⇄ civil (algoritmo de Howard Hinnant).
//! `chrono-source-git` tiene su propia copia privada de `epoch_to_iso8601`;
//! se duplica aquí (15 líneas) antes que ampliar su API pública.

/// Segundos UTC desde epoch, ahora.
pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "YYYY-MM-DDThh:mm:ssZ" a partir de segundos UTC.
pub fn epoch_to_iso8601(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Parsea `--since`: `YYYY-MM-DD` (medianoche UTC) o `YYYY-MM-DDThh:mm:ss[Z]`.
/// Devuelve `None` si no lo entiende.
pub fn parse_since(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    if dp.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = 0i64;
    if let Some(t) = time {
        let parts: Vec<&str> = t.split(':').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        let hh: i64 = parts[0].parse().ok()?;
        let mm: i64 = parts.get(1).map_or(Some(0), |p| p.parse().ok())?;
        let ss: i64 = parts.get(2).map_or(Some(0), |p| p.parse().ok())?;
        secs = hh * 3600 + mm * 60 + ss;
    }
    Some(days_from_civil(y, m, d) * 86_400 + secs)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ida_y_vuelta() {
        assert_eq!(parse_since("2024-01-01"), Some(1_704_067_200));
        assert_eq!(epoch_to_iso8601(1_704_067_200), "2024-01-01T00:00:00Z");
        assert_eq!(parse_since("2024-01-01T01:00:00Z"), Some(1_704_070_800));
        assert_eq!(parse_since("1970-01-01"), Some(0));
        assert_eq!(parse_since("ayer"), None);
        assert_eq!(parse_since("2024-13-01"), None);
    }
}
