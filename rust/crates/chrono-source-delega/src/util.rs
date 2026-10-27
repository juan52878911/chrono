//! Tiempo sin zona, redacción de términos prohibidos y lectura tolerante de los
//! ficheros de un trabajo (nombres nuevos en inglés y legados en español).

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Claves legadas de `meta` → nuevas (las mismas que `delega.util.LEGACY_META`).
const LEGACY_META: &[(&str, &str)] =
    &[("HOST", "NODE"), ("RJOB", "CHANNEL"), ("RAMA", "BRANCH"), ("DUENO", "OWNER"), ("FIN", "END")];

/// Ficheros del canal: nombre nuevo → legado (`delega.util.LEGACY_FILES`).
const LEGACY_FILES: &[(&str, &str)] = &[
    ("notices.jsonl", "avisos.jsonl"),
    ("progress.log", "progreso.log"),
    ("state.json", "estado.json"),
    ("result.json", "salida.json"),
    ("results.jsonl", "resultados.jsonl"),
    ("summary.json", "resumen.json"),
];

/// `meta` son líneas `CLAVE=valor`; la última gana. Las claves legadas se traducen.
pub fn read_meta(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = fs::read_to_string(dir.join("meta")) else { return out };
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let k = LEGACY_META.iter().find(|(old, _)| *old == k).map_or(k, |(_, new)| *new);
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

/// Ruta de un fichero del canal, cayendo al nombre legado si solo existe ese.
pub fn channel_file(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        if let Some((_, legacy)) = LEGACY_FILES.iter().find(|(n, _)| *n == name) {
            let l = dir.join(legacy);
            if l.exists() {
                return l;
            }
        }
    }
    p
}

/// JSON de un objeto, o un mapa vacío si no existe o no es un objeto.
pub fn read_json(path: &Path) -> Map<String, Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Primer número de `keys` presente en `obj`.
pub fn num(obj: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| obj.get(*k).and_then(Value::as_f64))
}

/// Primera cadena no vacía de `keys` presente en `obj`.
pub fn text(obj: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| obj.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string))
}

/// Días desde 1970-01-01 de una fecha civil (Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DDThh:mm:ss[.fff][Z|±hh:mm]` → `(ms leídos como si fueran UTC, offset explícito en s)`.
/// delega escribe hora local SIN zona; el offset se aplica fuera.
pub fn parse_ts(s: &str) -> Option<(i64, Option<i64>)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok());
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &s[19..];
    let mut ms = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        rest = &frac[digits.len()..];
        let d3: String = digits.chars().chain("000".chars()).take(3).collect();
        ms = d3.parse().unwrap_or(0);
    }
    let tz = match rest {
        "" => None,
        "Z" => Some(0),
        r if (r.starts_with('+') || r.starts_with('-')) && r.len() >= 5 => {
            let sign = if r.starts_with('-') { -1 } else { 1 };
            let r = r[1..].replace(':', "");
            let hh: i64 = r.get(0..2)?.parse().ok()?;
            let mm: i64 = r.get(2..4)?.parse().ok()?;
            Some(sign * (hh * 3600 + mm * 60))
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se;
    Some((secs * 1000 + ms, tz))
}

/// ms UTC de una marca de delega: con zona explícita la usa; sin ella resta `offset_s`
/// (hora local = UTC + offset).
pub fn ts_ms(s: &str, offset_s: i64) -> Option<i64> {
    let (ms, tz) = parse_ts(s)?;
    Some(ms - tz.unwrap_or(offset_s) * 1000)
}

/// Términos de un fichero de términos prohibidos: uno por línea, `#` comenta.
pub fn load_terms(path: &Path) -> Vec<String> {
    let Ok(t) = fs::read_to_string(path) else { return Vec::new() };
    let mut out: Vec<String> =
        t.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect();
    // Los largos primero, como `delega.scan._rx`: «Acme Corp» antes que «Acme».
    out.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()).then_with(|| a.cmp(b)));
    out
}

/// Sustituye cada aparición (sin distinguir mayúsculas) de un término por `[redacted]`.
pub fn redact(text: &str, terms: &[String]) -> String {
    if terms.is_empty() {
        return text.to_string();
    }
    // Carácter a carácter (primer carácter de su minúscula) en ambos lados: así los índices casan.
    let lowered: Vec<Vec<char>> =
        terms.iter().map(|t| t.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()).collect();
    let chars: Vec<char> = text.chars().collect();
    let low: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    'outer: while i < chars.len() {
        for t in &lowered {
            if !t.is_empty() && low.len() - i >= t.len() && low[i..i + t.len()] == t[..] {
                out.push_str("[redacted]");
                i += t.len();
                continue 'outer;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Primera línea no vacía, sin marcas de Markdown al principio, recortada a `max` caracteres y redactada.
pub fn first_line(text: &str, max: usize, terms: &[String]) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_start_matches(['#', '*', '>', '-', ' ']);
    // Se redacta antes de recortar: un término cortado por la mitad se escaparía.
    let red = redact(line, terms);
    if red.chars().count() <= max {
        red
    } else {
        let mut s: String = red.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}
