//! Rutas JSON para los selectores de campo (`time_field`, `message_field`,
//! `explode`, ...).
//!
//! Gramática de un selector:
//!
//! ```text
//! ruta     := segmento ( "." segmento )*
//! segmento := clave sufijo* | sufijo+
//! sufijo   := "[]" | "[" dígitos "]"
//! ```
//!
//! - `clave`: clave de objeto (o, si el valor actual es un array y la clave
//!   son solo dígitos, un índice de array: `data.0.id`).
//! - `[n]`: índice de array explícito (`data[0].id` == `data.0.id`).
//! - `[]`: comodín de array. En un selector de CAMPO resuelve al primer
//!   elemento para el que el resto de la ruta existe (`message.content[].name`
//!   = el `name` del primer item que lo tenga). La expansión "un evento por
//!   elemento" NO se pide con `[]` en un campo sino con la opción `explode`.
//!
//! Compatibilidad: primero se intenta SIEMPRE el selector completo como
//! clave literal de primer nivel (`obj.get(selector)`), de modo que `ts`,
//! `@timestamp` y también claves planas con punto (`log.level` de ECS)
//! resuelven exactamente como antes. Solo si esa clave no existe y el
//! selector contiene `.` o `[`, se interpreta como ruta.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Key(String),
    Any,
}

/// ¿Parece una ruta (y no una clave simple)?
fn is_path(sel: &str) -> bool {
    sel.contains('.') || sel.contains('[')
}

/// Parsea un selector a segmentos; `None` si está mal formado (segmento
/// vacío, corchete sin cerrar, índice no numérico).
fn parse(sel: &str) -> Option<Vec<Seg>> {
    let mut segs = Vec::new();
    for part in sel.split('.') {
        let (base, mut rest) = match part.find('[') {
            Some(i) => (&part[..i], &part[i..]),
            None => (part, ""),
        };
        if base.is_empty() && rest.is_empty() {
            return None; // "a..b", ".a" o "a."
        }
        if !base.is_empty() {
            segs.push(Seg::Key(base.to_string()));
        }
        while !rest.is_empty() {
            if !rest.starts_with('[') {
                return None;
            }
            let end = rest.find(']')?;
            let inner = &rest[1..end];
            if inner.is_empty() {
                segs.push(Seg::Any);
            } else if inner.chars().all(|c| c.is_ascii_digit()) {
                segs.push(Seg::Key(inner.to_string()));
            } else {
                return None;
            }
            rest = &rest[end + 1..];
        }
    }
    Some(segs)
}

fn walk<'a>(v: &'a Value, segs: &[Seg]) -> Option<&'a Value> {
    let Some((first, rest)) = segs.split_first() else {
        return Some(v);
    };
    match first {
        Seg::Key(k) => {
            let next = match v {
                Value::Object(m) => m.get(k),
                Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)),
                _ => None,
            };
            walk(next?, rest)
        }
        Seg::Any => v.as_array()?.iter().find_map(|e| walk(e, rest)),
    }
}

/// Resuelve el selector `sel` contra `obj` (ver gramática arriba). `None` si
/// falta algún tramo o el selector está mal formado.
pub fn lookup<'a>(obj: &'a Map<String, Value>, sel: &str) -> Option<&'a Value> {
    if sel.is_empty() {
        return None;
    }
    if let Some(v) = obj.get(sel) {
        return Some(v);
    }
    if !is_path(sel) {
        return None;
    }
    let segs = parse(sel)?;
    let (first, rest) = segs.split_first()?;
    match first {
        Seg::Key(k) => walk(obj.get(k)?, rest),
        Seg::Any => None,
    }
}

/// Normaliza la ruta de `explode`: tolera un `[]` final (`message.content[]`).
pub fn normalize_array_path(sel: &str) -> String {
    sel.trim().strip_suffix("[]").unwrap_or(sel.trim()).to_string()
}

/// Ámbito de resolución de campos de un evento: "elemento-luego-raíz".
///
/// Sin `explode`, `elem` es `None` y solo se mira la raíz (la línea). Con
/// `explode`, cada selector se resuelve primero contra el elemento del array
/// y, si ahí FALTA, contra el objeto raíz de la línea.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub elem: Option<&'a Map<String, Value>>,
    pub root: &'a Map<String, Value>,
}

impl<'a> Scope<'a> {
    pub fn root_only(root: &'a Map<String, Value>) -> Self {
        Self { elem: None, root }
    }

    pub fn get(&self, sel: &str) -> Option<&'a Value> {
        self.elem.and_then(|e| lookup(e, sel)).or_else(|| lookup(self.root, sel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn clave_simple_y_clave_literal_con_punto() {
        let o = obj(json!({"ts": 1, "log.level": "warn"}));
        assert_eq!(lookup(&o, "ts"), Some(&json!(1)));
        assert_eq!(lookup(&o, "log.level"), Some(&json!("warn")));
        assert_eq!(lookup(&o, "nada"), None);
        assert_eq!(lookup(&o, ""), None);
    }

    #[test]
    fn ruta_anidada_e_indices() {
        let o = obj(json!({"message": {"role": "user"}, "data": [{"id": "a"}, {"id": "b"}]}));
        assert_eq!(lookup(&o, "message.role"), Some(&json!("user")));
        assert_eq!(lookup(&o, "data.0.id"), Some(&json!("a")));
        assert_eq!(lookup(&o, "data.1.id"), Some(&json!("b")));
        assert_eq!(lookup(&o, "data[1].id"), Some(&json!("b")));
        assert_eq!(lookup(&o, "data.2.id"), None);
        assert_eq!(lookup(&o, "message.role.x"), None);
    }

    #[test]
    fn comodin_toma_el_primer_elemento_donde_existe_el_resto() {
        let o = obj(json!({"c": [{"type": "text"}, {"name": "Read"}, {"name": "Bash"}]}));
        assert_eq!(lookup(&o, "c[].name"), Some(&json!("Read")));
        assert_eq!(lookup(&o, "c.[].name"), Some(&json!("Read")));
    }

    #[test]
    fn rutas_mal_formadas_dan_none() {
        let o = obj(json!({"a": {"b": 1}}));
        assert_eq!(lookup(&o, "a..b"), None);
        assert_eq!(lookup(&o, ".a"), None);
        assert_eq!(lookup(&o, "a."), None);
        assert_eq!(lookup(&o, "a[x]"), None);
        assert_eq!(lookup(&o, "a[1"), None);
    }

    #[test]
    fn scope_elemento_luego_raiz() {
        let root = obj(json!({"timestamp": "t", "name": "raiz"}));
        let elem = obj(json!({"name": "Read"}));
        let sc = Scope { elem: Some(&elem), root: &root };
        assert_eq!(sc.get("name"), Some(&json!("Read")));
        assert_eq!(sc.get("timestamp"), Some(&json!("t")));
        assert_eq!(Scope::root_only(&root).get("name"), Some(&json!("raiz")));
    }

    #[test]
    fn normaliza_corchetes_finales() {
        assert_eq!(normalize_array_path("message.content[]"), "message.content");
        assert_eq!(normalize_array_path(" message.content "), "message.content");
    }
}
