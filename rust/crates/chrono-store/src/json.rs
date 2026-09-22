//! Serializador JSON determinista para `BTreeMap<String, String>`.
//!
//! `chrono-core` no depende de `serde` a propósito (dominio sin I/O), así que
//! los `attrs` de `Event`/`Touch` se vuelcan a las columnas TEXT `attrs` con
//! este serializador mínimo. `BTreeMap` ya itera en orden de clave, así que la
//! salida es determinista sin ordenar nada aquí.

use std::collections::BTreeMap;

/// Serializa un mapa de atributos a un objeto JSON `{"k":"v",...}`.
pub(crate) fn attrs_to_json(attrs: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(attrs.len() * 16 + 2);
    out.push('{');
    let mut first = true;
    for (k, v) in attrs {
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        escape_into(k, &mut out);
        out.push_str("\":\"");
        escape_into(v, &mut out);
        out.push('"');
    }
    out.push('}');
    out
}

/// Escapa comillas, backslash y caracteres de control como exige JSON.
fn escape_into(input: &str, out: &mut String) {
    for c in input.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapa_vacio_es_objeto_vacio() {
        assert_eq!(attrs_to_json(&BTreeMap::new()), "{}");
    }

    #[test]
    fn orden_determinista_y_escapado() {
        let mut m = BTreeMap::new();
        m.insert("b".to_string(), "línea\ncon \"comillas\" y \\barra".to_string());
        m.insert("a".to_string(), "1".to_string());
        assert_eq!(
            attrs_to_json(&m),
            "{\"a\":\"1\",\"b\":\"línea\\ncon \\\"comillas\\\" y \\\\barra\"}"
        );
    }
}
