//! Fechas sin dependencias: epoch UTC ⇄ civil (algoritmo de Howard Hinnant).
//! UNIDAD CANÓNICA: **milisegundos** UTC desde epoch (`at_epoch` es ms en todo
//! el dominio). `chrono-source-git` y los adaptadores tienen su copia de estas
//! funciones (se duplican, no se amplía una API pública por 20 líneas).

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

/// "YYYY-MM-DDThh:mm:ss[.mmm]Z" a partir de MILISEGUNDOS UTC. La parte de
/// milisegundos solo aparece cuando no es cero (así las fuentes con resolución
/// de segundo —git, syslog— mantienen la salida limpia "…ssZ").
pub fn epoch_to_iso8601(epoch_ms: i64) -> String {
    let days = epoch_ms.div_euclid(86_400_000);
    let ms_of_day = epoch_ms.rem_euclid(86_400_000);
    let secs = ms_of_day / 1000;
    let millis = ms_of_day % 1000;
    let (y, m, d) = civil_from_days(days);
    let base = format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    );
    if millis == 0 {
        format!("{base}Z")
    } else {
        format!("{base}.{millis:03}Z")
    }
}

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
            let neg = -((x % 62_167_219_200_000) as i64); // 0000..1970
            assert_eq!(parse_since(&epoch_to_iso8601(neg)), Some(neg), "{}", epoch_to_iso8601(neg));
        }
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
