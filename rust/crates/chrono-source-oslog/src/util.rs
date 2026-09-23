//! Utilidades compartidas por los parsers de macOS y Linux: escalares JSON a
//! texto, recorte de `title` e id determinista de respaldo.

use crate::simhash::fnv1a64_line;
use serde_json::Value;

/// Convierte un valor JSON escalar (string/number/bool) a texto; "" para
/// null, array, objeto o campo ausente (`None`).
pub(crate) fn as_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// Recorta `s` a lo sumo `max_chars` caracteres (respetando fronteras UTF-8).
pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Último segmento tras "/" de una ruta de proceso ("/usr/sbin/x" -> "x").
pub(crate) fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// `id` determinista de respaldo: FNV-1a 64 en hex de (timestamp crudo +
/// mensaje + índice del registro dentro de esta ejecución). El índice evita
/// que dos registros con el mismo timestamp+mensaje (frecuente en logs
/// repetitivos) colisionen en el mismo id; al no depender del reloj de pared
/// ni de nada externo, el mismo rango + misma config siempre da los mismos
/// ids en el mismo orden.
pub(crate) fn compute_id(ts_raw: &str, msg: &str, idx: usize) -> String {
    let line = format!("{ts_raw}|{msg}");
    format!("{:016x}", fnv1a64_line(line.as_bytes(), idx as u64))
}
