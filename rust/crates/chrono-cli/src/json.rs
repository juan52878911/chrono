//! Emisor JSON mínimo (sin serde) para el envelope de salida.
//!
//! Los objetos conservan el orden de inserción (como el struct `Envelope` del
//! Go); los números flotantes se imprimen con `Display` de Rust, que da `1`
//! para `1.0` y `0.5` para `0.5`, igual que `encoding/json` en Go.

pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn obj() -> Json {
        Json::Obj(Vec::new())
    }

    /// Añade una clave a un objeto (no hace nada si `self` no es un objeto).
    pub fn set(mut self, key: &str, value: Json) -> Json {
        if let Json::Obj(ref mut fields) = self {
            fields.push((key.to_string(), value));
        }
        self
    }

    pub fn str(s: &str) -> Json {
        Json::Str(s.to_string())
    }

    /// Serializa indentado con 2 espacios (como `json.Encoder.SetIndent("", "  ")`).
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => out.push_str(&n.to_string()),
            Json::Float(f) => {
                if f.is_finite() {
                    out.push_str(&f.to_string());
                } else {
                    out.push_str("null");
                }
            }
            Json::Str(s) => write_str(s, out),
            Json::Arr(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    indent(out, depth + 1);
                    it.write(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push(']');
            }
            Json::Obj(fields) => {
                if fields.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    indent(out, depth + 1);
                    write_str(k, out);
                    out.push_str(": ");
                    v.write(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
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
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objeto_anidado_indentado() {
        let j = Json::obj()
            .set("a", Json::Int(1))
            .set("b", Json::Arr(vec![Json::Float(1.0), Json::Float(0.5)]))
            .set("c", Json::obj())
            .set("d", Json::Null)
            .set("e", Json::str("x\"y"));
        assert_eq!(
            j.pretty(),
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    0.5\n  ],\n  \"c\": {},\n  \"d\": null,\n  \"e\": \"x\\\"y\"\n}\n"
        );
    }
}
