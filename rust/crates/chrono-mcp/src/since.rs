//! Parseo de `since` a epoch UTC. Copia reducida de `chrono-cli/src/timeutil.rs`
//! (mismo criterio de duplicación que usa `chrono-source-git` para
//! `epoch_to_iso8601`: 15 líneas, no vale la pena ampliar una API pública
//! solo para compartirlas).

/// Parsea `since`: `YYYY-MM-DD` (medianoche UTC) o `YYYY-MM-DDThh:mm:ss[Z]`.
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

/// `since` textual (posiblemente vacío o inválido) -> epoch para las
/// consultas de `chrono-metrics`. A diferencia del CLI, el MCP no aborta la
/// llamada por un `since` mal formado: simplemente ignora la ventana (0).
pub fn since_to_epoch(s: &str) -> i64 {
    if s.is_empty() {
        return 0;
    }
    parse_since(s).unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_fechas_simples() {
        assert_eq!(parse_since("2024-01-01"), Some(1_704_067_200));
        assert_eq!(parse_since("1970-01-01"), Some(0));
        assert_eq!(parse_since("ayer"), None);
        assert_eq!(since_to_epoch(""), 0);
        assert_eq!(since_to_epoch("no-es-fecha"), 0);
    }
}
