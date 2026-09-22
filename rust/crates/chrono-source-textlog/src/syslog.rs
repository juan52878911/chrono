//! Parseo a mano de syslog: RFC5424 (preferido, trae timestamp ISO con año)
//! y RFC3164/BSD clásico (sin año propio: usa `fallback_year`). Sin `regex`.
//!
//! El PRI (`<NN>`) es obligatorio en la gramática de RFC5424, pero OPCIONAL
//! para RFC3164 aquí: los ficheros de syslog en disco (`/var/log/syslog`,
//! etc.) casi nunca lo incluyen -- lo añade el daemon solo al enviar por la
//! red -- así que sin PRI seguimos aceptando RFC3164, con `level=""` (ver
//! encargo: "o \"\" si no hay PRI").

use crate::preset::split_ws;
use crate::timeutil::{days_from_civil, parse_rfc3339};

const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Etiquetas de severidad 0..7, en el orden de RFC5424 §6.2.1.
const SEVERITY_LABELS: [&str; 8] =
    ["EMERG", "ALERT", "CRIT", "ERROR", "WARN", "NOTICE", "INFO", "DEBUG"];

fn month_from_name(s: &str) -> Option<u32> {
    MONTHS.iter().position(|m| m.eq_ignore_ascii_case(s)).map(|i| i as u32 + 1)
}

/// Campos ya unificados de una línea de syslog (RFC5424 o RFC3164): `tag`
/// vale APP-NAME en RFC5424 o TAG en RFC3164; `pid` vale PROCID o el `[PID]`
/// del TAG. Los campos ausentes ("-"/NILVALUE, o sin PRI) quedan en "".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyslogFields {
    pub level: String,
    pub facility: String,
    pub severity: String,
    pub host: String,
    pub tag: String,
    pub pid: String,
    pub msg: String,
    pub epoch: i64,
}

/// "<NN>" al inicio -> (valor PRI, resto de la línea tras '>'). `None` si no
/// hay un PRI bien formado (1 a 3 dígitos, valor 0..191).
fn parse_pri(line: &str) -> Option<(u32, &str)> {
    let rest = line.strip_prefix('<')?;
    let close = rest.find('>')?;
    if close == 0 || close > 3 {
        return None;
    }
    let digits = &rest[..close];
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pri: u32 = digits.parse().ok()?;
    if pri > 191 {
        return None;
    }
    Some((pri, &rest[close + 1..]))
}

/// PRI = facility*8 + severity.
fn severity_of(pri: u32) -> (u32, u32, &'static str) {
    let facility = pri / 8;
    let severity = pri % 8;
    (facility, severity, SEVERITY_LABELS[severity as usize])
}

/// Fin de un elemento STRUCTURED-DATA `[...]` de RFC5424: primer `]` fuera de
/// comillas, respetando el escape `\"` dentro de un valor de parámetro.
/// `None` si el elemento no cierra (línea truncada: no casa RFC5424).
fn find_sd_element_end(s: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ']' if !in_quotes => return Some(i),
            _ => {}
        }
    }
    None
}

/// Consume STRUCTURED-DATA ("-" NILVALUE, o una o más "[...]") y devuelve lo
/// que queda tras ella (el MSG, todavía sin recortar el espacio inicial).
fn skip_structured_data(r: &str) -> Option<&str> {
    let s = r.trim_start();
    if let Some(rest) = s.strip_prefix('-') {
        // Debe ser el NILVALUE completo, no un token que empieza por "-".
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some(rest);
        }
        return None;
    }
    let mut cur = s;
    if !cur.starts_with('[') {
        return None;
    }
    while cur.starts_with('[') {
        let end = find_sd_element_end(cur)?;
        cur = &cur[end + 1..];
    }
    Some(cur)
}

fn nilify(s: &str) -> String {
    if s == "-" {
        String::new()
    } else {
        s.to_string()
    }
}

/// RFC5424 tras el PRI: "VERSION TIMESTAMP HOST APP PROCID MSGID SD [MSG]".
/// `None` si la versión no es "1" o falta algún campo obligatorio hasta
/// STRUCTURED-DATA. Un TIMESTAMP nil ("-") o inválido no invalida el
/// formato: el epoch resultante es `None` (el llamador lo trata como "sin
/// tiempo", igual que jsonl).
fn try_rfc5424(rest: &str) -> Option<(String, String, String, String, Option<i64>)> {
    let (version, r) = split_ws(rest)?;
    if version != "1" {
        return None;
    }
    let (timestamp, r) = split_ws(r)?;
    let epoch = if timestamp == "-" { None } else { parse_rfc3339(timestamp) };
    let (host, r) = split_ws(r)?;
    let (appname, r) = split_ws(r)?;
    let (procid, r) = split_ws(r)?;
    let (_msgid, r) = split_ws(r)?;
    let after_sd = skip_structured_data(r)?;
    let msg = after_sd.trim_start();
    let msg = msg.strip_prefix('\u{feff}').unwrap_or(msg); // BOM opcional antes del MSG.
    Some((nilify(host), nilify(appname), nilify(procid), msg.to_string(), epoch))
}

/// RFC3164 tras el (opcional) PRI: "Mon DD HH:MM:SS HOST TAG[PID]: MSG".
/// `fallback_year` porque el formato no trae año.
fn try_rfc3164(rest: &str, fallback_year: i64) -> Option<(String, String, String, String, i64)> {
    let (mon_name, r) = split_ws(rest)?;
    let month = month_from_name(mon_name)?;
    let (day_str, r) = split_ws(r)?;
    let day: u32 = day_str.parse().ok()?;
    let (time_str, r) = split_ws(r)?;
    let mut t = time_str.splitn(3, ':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: i64 = t.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=60).contains(&ss) {
        return None;
    }

    let (host, r) = split_ws(r)?;
    let r = r.trim_start();
    let colon = r.find(':')?;
    let tag_part = &r[..colon];
    let msg = r[colon + 1..].trim_start().to_string();

    let (tag, pid) = match tag_part.find('[') {
        Some(br) if tag_part.ends_with(']') && br < tag_part.len() - 1 => {
            (tag_part[..br].to_string(), tag_part[br + 1..tag_part.len() - 1].to_string())
        }
        _ => (tag_part.to_string(), String::new()),
    };
    if tag.is_empty() || host.is_empty() {
        return None;
    }

    let days = days_from_civil(fallback_year, month, day);
    let epoch = days * 86_400 + hh * 3600 + mm * 60 + ss;
    Some((host.to_string(), tag, pid, msg, epoch))
}

/// Parsea una línea de syslog: intenta RFC5424 primero (si hay PRI), luego
/// RFC3164 (con o sin PRI). `None` si ninguno casa.
pub fn parse(line: &str, fallback_year: i64) -> Option<SyslogFields> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (pri, rest) = match parse_pri(line) {
        Some((p, r)) => (Some(p), r),
        None => (None, line),
    };

    if let Some(p) = pri {
        if let Some((host, tag, pid, msg, epoch)) = try_rfc5424(rest) {
            let (facility, severity, level) = severity_of(p);
            return Some(SyslogFields {
                level: level.to_string(),
                facility: facility.to_string(),
                severity: severity.to_string(),
                host,
                tag,
                pid,
                msg,
                epoch: epoch.unwrap_or(0),
            });
        }
    }

    if let Some((host, tag, pid, msg, epoch)) = try_rfc3164(rest, fallback_year) {
        let (level, facility, severity) = match pri {
            Some(p) => {
                let (fac, sev, lvl) = severity_of(p);
                (lvl.to_string(), fac.to_string(), sev.to_string())
            }
            None => (String::new(), String::new(), String::new()),
        };
        return Some(SyslogFields { level, facility, severity, host, tag, pid, msg, epoch });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc5424_con_structured_data_y_pri() {
        let line = r#"<34>1 2023-10-11T22:14:15.003Z mymachine.example.com su - ID47 [ex@32473 iut="3"] BOMsu root failed"#;
        let f = parse(line, 1970).unwrap();
        assert_eq!(f.host, "mymachine.example.com");
        assert_eq!(f.tag, "su");
        assert_eq!(f.pid, "");
        assert_eq!(f.level, "CRIT"); // 34 = facility 4, severity 2 (CRIT)
        assert_eq!(f.facility, "4");
        assert_eq!(f.severity, "2");
        assert!(f.msg.starts_with("BOMsu"));
        assert_eq!(f.epoch, 1_697_062_455);
    }

    #[test]
    fn rfc5424_nilvalues_y_sd_nil() {
        let line = "<13>1 - - - - - - mensaje sin nada mas";
        let f = parse(line, 1970).unwrap();
        assert_eq!(f.host, "");
        assert_eq!(f.tag, "");
        assert_eq!(f.pid, "");
        assert_eq!(f.epoch, 0); // timestamp nil -> sin tiempo, epoch=0 documentado.
        assert_eq!(f.msg, "mensaje sin nada mas");
    }

    #[test]
    fn rfc3164_con_pri_y_pid() {
        let line = "<13>Oct 11 22:14:15 host sshd[1234]: Accepted password for root";
        let f = parse(line, 2023).unwrap();
        assert_eq!(f.host, "host");
        assert_eq!(f.tag, "sshd");
        assert_eq!(f.pid, "1234");
        assert_eq!(f.msg, "Accepted password for root");
        assert_eq!(f.level, "NOTICE"); // 13 = facility 1, severity 5 (NOTICE)
    }

    #[test]
    fn rfc3164_sin_pri_usa_year_option_y_level_vacio() {
        let line = "Oct 11 22:14:15 host sshd[1234]: Accepted password for root";
        let f = parse(line, 2023).unwrap();
        assert_eq!(f.level, "");
        assert_eq!(f.facility, "");
        // 2023-10-11T22:14:15Z
        assert_eq!(f.epoch, 1_697_062_455);
    }

    #[test]
    fn rfc3164_sin_year_option_usa_1970_determinista() {
        let line = "Jan 1 00:00:00 host tag: msg";
        let f = parse(line, 1970).unwrap();
        assert_eq!(f.epoch, 0);
    }

    #[test]
    fn basura_json_y_csv_no_casan() {
        assert!(parse(r#"{"foo": "bar"}"#, 1970).is_none());
        assert!(parse("a,b,c,d", 1970).is_none());
    }
}
