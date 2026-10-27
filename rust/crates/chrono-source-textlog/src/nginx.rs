//! Parseo a mano del formato de log de acceso combined/common de nginx (y
//! apache, que comparte el mismo formato): sin `regex`, solo `split`/índices
//! sobre `&str`.
//!
//! `IP - user [DD/Mon/YYYY:HH:MM:SS +ZZZZ] "METHOD path HTTP/x.x" status bytes ["referer" "user-agent"]`
//!
//! La fecha trae año y offset de zona completos -> el epoch siempre es
//! determinista (a diferencia del RFC3164 de syslog, que no trae año).

use crate::preset::split_ws;
use chrono_core::time::{days_from_civil_checked, secs_of_day_checked};

const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn month_from_name(s: &str) -> Option<u32> {
    MONTHS.iter().position(|m| m.eq_ignore_ascii_case(s)).map(|i| i as u32 + 1)
}

/// Campos ya extraídos de una línea de acceso nginx/apache. Los campos de
/// texto ausentes ("-" en el log) quedan como cadena vacía.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NginxFields {
    pub ip: String,
    pub user: String,
    pub epoch: i64,
    pub method: String,
    pub path: String,
    pub status: String,
    pub bytes: String,
    pub referer: String,
    pub user_agent: String,
}

/// "+ZZZZ"/"-ZZZZ" -> segundos. Misma fórmula/signo que
/// `chrono_core::time::parse_rfc3339`: la hora local del log es UTC ± ese offset, y
/// `UTC = local - offset`.
fn parse_tz_offset(s: &str) -> Option<i64> {
    if s.len() != 5 {
        return None;
    }
    let sign: i64 = match s.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hh: i64 = s.get(1..3)?.parse().ok()?;
    let mm: i64 = s.get(3..5)?.parse().ok()?;
    Some(sign * (hh * 3600 + mm * 60))
}

/// "10/Oct/2000:13:55:36 -0700" -> epoch UTC en ms. Fuera: no tiene la forma
/// de una fecha nginx (la línea no casa). Dentro: `None` si la forma casa
/// pero la fecha no existe (30/Feb, 25:00, desfase de 25 h): la línea entra
/// sin tiempo, como en jsonl o csv.
fn parse_nginx_date(s: &str) -> Option<Option<i64>> {
    let mut top = s.splitn(2, ' ');
    let dt = top.next()?;
    let tz = top.next().unwrap_or("+0000");

    let mut it = dt.splitn(3, '/');
    let day: u32 = it.next()?.parse().ok()?;
    let month = month_from_name(it.next()?)?;
    let rest = it.next()?; // "YYYY:HH:MM:SS"

    let mut it2 = rest.splitn(2, ':');
    let year: i64 = it2.next()?.parse().ok()?;
    let time_part = it2.next()?;
    let mut t = time_part.splitn(3, ':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: i64 = t.next()?.parse().ok()?;
    let offset = parse_tz_offset(tz)?;
    let valida = || {
        let days = days_from_civil_checked(year, month, day)?;
        let secs = secs_of_day_checked(hh, mm, ss)?;
        (offset.abs() < 86_400).then_some(())?;
        // `at_epoch` es ms en todo el dominio; nginx tiene resolución de segundo.
        Some((days * 86_400 + secs - offset) * 1000)
    };
    Some(valida())
}

/// Contenido entre comillas dobles al inicio de `s` (sin soporte de escapes:
/// el combined log format no los usa) y el resto tras la comilla de cierre.
fn take_quoted(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some((&s[..end], &s[end + 1..]))
}

/// Parsea una línea de acceso nginx/apache (combined o common log format).
/// `None` si no casa el formato: la línea se descarta (ver
/// `cursor::TextlogCursor::next`), no aborta la ingesta.
pub fn parse(line: &str) -> Option<NginxFields> {
    let line = line.trim_end_matches(['\r', '\n']);

    let (ip, r) = split_ws(line)?;
    let (_ident, r) = split_ws(r)?; // rfc931 (identd): casi siempre "-", se ignora.
    let (user_raw, r) = split_ws(r)?;
    let user = if user_raw == "-" { String::new() } else { user_raw.to_string() };

    let r = r.trim_start().strip_prefix('[')?;
    let close = r.find(']')?;
    // Sin tiempo: `epoch = 0` (el `at` sale vacío, ver `record.rs`).
    let epoch = parse_nginx_date(&r[..close])?.unwrap_or(0);
    let r = r[close + 1..].trim_start();

    let (request, r) = take_quoted(r)?;
    let mut parts = request.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    if method.is_empty() || path.is_empty() {
        return None;
    }

    let (status_raw, r) = split_ws(r)?;
    if status_raw.is_empty() || !status_raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    let (bytes_raw, r) = split_ws(r)?;
    let bytes = if bytes_raw == "-" { String::new() } else { bytes_raw.to_string() };

    // referer y user-agent son opcionales (common log format no los trae).
    let mut referer = String::new();
    let mut user_agent = String::new();
    if let Some((val, r2)) = take_quoted(r.trim_start()) {
        if val != "-" {
            referer = val.to_string();
        }
        if let Some((val2, _)) = take_quoted(r2.trim_start()) {
            if val2 != "-" {
                user_agent = val2.to_string();
            }
        }
    }

    Some(NginxFields {
        ip: ip.to_string(),
        user,
        epoch,
        method,
        path,
        status: status_raw.to_string(),
        bytes,
        referer,
        user_agent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_completo() {
        let line = concat!(
            r#"127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /apache_pb.gif HTTP/1.0" 200 2326 "#,
            r#""http://www.example.com/start.html" "Mozilla/4.08""#
        );
        let f = parse(line).unwrap();
        assert_eq!(f.ip, "127.0.0.1");
        assert_eq!(f.user, "frank");
        assert_eq!(f.method, "GET");
        assert_eq!(f.path, "/apache_pb.gif");
        assert_eq!(f.status, "200");
        assert_eq!(f.bytes, "2326");
        assert_eq!(f.referer, "http://www.example.com/start.html");
        assert_eq!(f.user_agent, "Mozilla/4.08");
        // Epoch conocido, verificado independientemente (ver informe): 971211336.
        assert_eq!(f.epoch, 971_211_336_000);
    }

    #[test]
    fn common_sin_referer_ni_agent_ni_bytes() {
        let line = r#"10.0.0.1 - - [01/Jan/2020:00:00:00 +0000] "GET / HTTP/1.1" 404 -"#;
        let f = parse(line).unwrap();
        assert_eq!(f.user, "");
        assert_eq!(f.status, "404");
        assert_eq!(f.bytes, "");
        assert_eq!(f.referer, "");
        assert_eq!(f.user_agent, "");
        assert_eq!(f.epoch, 1_577_836_800_000);
    }

    /// Tabla: con la forma de nginx pero una fecha u hora imposible, la línea
    /// entra sin tiempo (`epoch = 0`); antes el 30/Feb era el 1 o 2 de marzo.
    #[test]
    fn fechas_imposibles_quedan_sin_tiempo() {
        for fecha in ["30/Feb/2024:10:00:00 +0000", "29/Feb/2023:10:00:00 +0000", "31/Apr/2024:10:00:00 +0000", "01/Jan/2024:25:00:00 +0000", "01/Jan/2024:10:60:00 +0000", "01/Jan/2024:10:00:00 +2500"] {
            let line = format!(r#"10.0.0.1 - - [{fecha}] "GET / HTTP/1.1" 200 5"#);
            let f = parse(&line).unwrap_or_else(|| panic!("{fecha}: la línea debía casar"));
            assert_eq!(f.epoch, 0, "{fecha}");
        }
        let f = parse(r#"10.0.0.1 - - [29/Feb/2024:00:00:00 +0000] "GET / HTTP/1.1" 200 5"#).unwrap();
        assert_eq!(f.epoch, 1_709_164_800_000);
    }

    #[test]
    fn basura_json_y_csv_no_casan() {
        assert!(parse(r#"{"foo": "bar"}"#).is_none());
        assert!(parse("a,b,c,d").is_none());
        assert!(parse("linea cualquiera sin formato").is_none());
    }
}
