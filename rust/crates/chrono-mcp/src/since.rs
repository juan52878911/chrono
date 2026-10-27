//! Parseo de `since` a epoch UTC. Copia reducida de `chrono-cli/src/timeutil.rs`
//! (mismo criterio de duplicación que usa `chrono-source-git` para
//! `epoch_to_iso8601`: 15 líneas, no vale la pena ampliar una API pública
//! solo para compartirlas).

/// Parsea una fecha de `since`/`until`: `YYYY-MM-DD` (medianoche UTC) o
/// `YYYY-MM-DD[T ]hh[:mm[:ss[.fff]]]` con `Z` o un desfase (`+02:00`, `-0500`,
/// `+02`) opcionales; sin zona es UTC. Lo que escribe chrono
/// (`…T10:00:00.123Z`) vuelve a entrar. Una fecha u hora fuera de rango
/// (`2024-02-30`, `25:00`) o cualquier otra cosa: `None`.
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
    if dp.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
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
        if parts.next().is_some() || hh > 23 || mm > 59 || ss > 60 {
            return None;
        }
        let frac_ms = match frac {
            // Más de tres cifras: se truncan a milisegundos.
            Some(f) => digits(&format!("{:0<3}", &f[..f.len().min(3)]), 3, 3).filter(|_| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))?,
            None => 0,
        };
        ms = (hh * 3600 + mm * 60 + ss.min(59) - offset_secs) * 1000 + frac_ms;
    }
    Some(days_from_civil(y, m as u32, d as u32) * 86_400_000 + ms)
}

/// Entre `min` y `max` cifras ASCII, sin signo.
fn digits(s: &str, min: usize, max: usize) -> Option<i64> {
    (s.len() >= min && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
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

/// `since` textual (vacío = sin ventana, 0) -> epoch para las consultas de
/// `chrono-metrics`. Un `since` mal formado ya lo rechazó
/// `crate::args::validate` con un error, como el CLI.
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
        assert_eq!(parse_since("2024-01-01"), Some(1_704_067_200_000));
        assert_eq!(parse_since("1970-01-01"), Some(0));
        assert_eq!(parse_since("ayer"), None);
        assert_eq!(since_to_epoch(""), 0);
        assert_eq!(since_to_epoch("no-es-fecha"), 0);
    }
}
