//! Validación de los argumentos de `tools/call` contra el `inputSchema` de
//! cada herramienta, con las mismas reglas que las flags del CLI: un argumento
//! que no existe, de otro tipo, un entero que no es > 0 o una fecha que no se
//! entiende es un error que nombra el argumento y lo que espera. Antes se
//! descartaban en silencio (un `since` mal escrito consultaba toda la
//! historia; `level: 5` contaba todos los niveles).
//!
//! Lo que ya valía sigue valiendo: un número o un booleano dentro de una
//! cadena (`"10"`, `"true"`, como los mandan a menudo los modelos), un entero
//! con decimales cero (`10.0`) y `null` como «no lo paso».

use serde_json::Value;

use crate::since::parse_since;
use crate::Result;

/// Formato de fecha que se pide en los errores de `since`/`until`.
const FECHA: &str = "usa YYYY-MM-DD o YYYY-MM-DDThh:mm:ssZ, p. ej. 2026-10-13 o 2026-10-13T22:00:00Z";

/// Comprueba `args` contra el esquema de la herramienta `name`. Una
/// herramienta desconocida no se comprueba aquí: la rechaza el despacho.
pub(crate) fn validate(name: &str, args: &Value) -> Result<()> {
    let Some(def) = crate::tools::tool_defs().into_iter().find(|d| d["name"] == name) else { return Ok(()) };
    let obj = match args {
        Value::Null => return Ok(()),
        Value::Object(o) => o,
        other => return Err(format!("{name}: los argumentos deben ser un objeto JSON, no {}", tipo(other)).into()),
    };
    let props = def["inputSchema"]["properties"].as_object().cloned().unwrap_or_default();
    for (k, v) in obj {
        let Some(prop) = props.get(k) else {
            let mut admite: Vec<&str> = props.keys().map(String::as_str).collect();
            admite.sort_unstable();
            let admite = if admite.is_empty() { "ninguno".to_string() } else { admite.join(", ") };
            return Err(format!("{name}: argumento desconocido '{k}' (admite: {admite})").into());
        };
        if v.is_null() {
            continue;
        }
        match prop["type"].as_str().unwrap_or("string") {
            "integer" => {
                let n = entero(v).ok_or_else(|| format!("{name}: '{k}' no válido: {} (usa un entero > 0)", corto(v)))?;
                if n <= 0 {
                    return Err(format!("{name}: '{k}' no válido: {n} (usa un entero > 0)").into());
                }
                // Los segundos se pasan a ms: que no desborde.
                if k.ends_with("_secs") && n.checked_mul(1000).is_none() {
                    return Err(format!("{name}: '{k}' demasiado grande: {n} segundos").into());
                }
            }
            "boolean" => {
                if booleano(v).is_none() {
                    return Err(format!("{name}: '{k}' no válido: {} (usa true o false)", corto(v)).into());
                }
            }
            _ => {
                let Some(s) = v.as_str() else {
                    return Err(format!("{name}: '{k}' debe ser una cadena de texto, no {} ({})", tipo(v), corto(v)).into());
                };
                if let Some(opciones) = prop["enum"].as_array() {
                    let s = s.trim();
                    if !s.is_empty() && !opciones.iter().any(|o| o == s) {
                        let ops: Vec<&str> = opciones.iter().filter_map(Value::as_str).collect();
                        return Err(format!("{name}: '{k}' no válido: {s:?} (usa {})", ops.join(" | ")).into());
                    }
                }
            }
        }
    }
    let fecha = |k: &str| -> Result<Option<i64>> {
        match obj.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => parse_since(s).map(Some).ok_or_else(|| format!("{name}: '{k}' no válido: {s:?} ({FECHA})").into()),
        }
    };
    let (since, until) = (fecha("since")?, fecha("until")?);
    // Una ventana vacía daba un resultado vacío sin más (como en el CLI).
    if let (Some(s), Some(u)) = (since, until) {
        if u <= s {
            return Err(format!("{name}: 'until' no es posterior a 'since': la ventana está vacía").into());
        }
    }
    Ok(())
}

/// Un entero: número sin decimales o cadena con uno (`"10"`, `" 10 "`).
pub(crate) fn entero(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18).map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Un booleano, o `"true"`/`"false"` en una cadena.
pub(crate) fn booleano(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn tipo(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "un booleano",
        Value::Number(_) => "un número",
        Value::String(_) => "una cadena",
        Value::Array(_) => "una lista",
        Value::Object(_) => "un objeto",
    }
}

/// El valor tal cual, recortado para el mensaje de error.
fn corto(v: &Value) -> String {
    let s = v.to_string();
    match s.char_indices().nth(60) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}
