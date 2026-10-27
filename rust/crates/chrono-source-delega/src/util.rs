//! Tiempo sin zona, redacción de términos prohibidos y lectura tolerante de los
//! ficheros de un trabajo (nombres nuevos en inglés y legados en español).

use chrono_core::time;
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

/// Offset local máximo admitido (±18 h, el rango de ISO 8601 y de chrono).
pub const MAX_OFFSET_S: i64 = 18 * 3600;

/// 0000-01-01T00:00:00Z y 9999-12-31T23:59:59.999Z en ms: fuera de los años
/// 0000–9999 una marca no es una fecha (un epoch en otra unidad, basura), y sin
/// tope una resta de dos de ellas desbordaba.
const MIN_MS: i64 = -62_167_219_200_000;
const MAX_MS: i64 = 253_402_300_799_999;

/// `ms` si cae en los años 0000–9999.
pub fn valid_ms(ms: i64) -> Option<i64> {
    (MIN_MS..=MAX_MS).contains(&ms).then_some(ms)
}

/// Segundos epoch (como los guarda delega en `state.json` y `meta`) → ms, o `None`
/// si no es un número finito o se sale de los años 0000–9999.
pub fn epoch_s_to_ms(s: f64) -> Option<i64> {
    let ms = s * 1000.0;
    (ms.is_finite() && ms >= MIN_MS as f64 && ms <= MAX_MS as f64).then_some(ms as i64)
}

/// `YYYY-MM-DDThh:mm:ss[.fff][Z|±hh:mm]` → `(ms leídos como si fueran UTC, offset explícito en s)`.
/// delega escribe hora local SIN zona; el offset se aplica fuera. El parser es
/// el de `chrono_core::time` (mismas fechas imposibles fuera que el resto de
/// fuentes); aquí solo se añade el tope de ±18 h al desfase.
pub fn parse_ts(s: &str) -> Option<(i64, Option<i64>)> {
    let (ms, tz) = time::parse_rfc3339_zoned(s.trim())?;
    match tz {
        Some(o) if o.abs() > MAX_OFFSET_S => None,
        _ => Some((ms, tz)),
    }
}

/// ms UTC de una marca de delega: con zona explícita la usa; sin ella resta `offset_s`
/// (hora local = UTC + offset).
pub fn ts_ms(s: &str, offset_s: i64) -> Option<i64> {
    let (ms, tz) = parse_ts(s)?;
    valid_ms(ms - tz.unwrap_or(offset_s).clamp(-MAX_OFFSET_S, MAX_OFFSET_S) * 1000)
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
