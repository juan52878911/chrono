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

    /// Cambia cada `source_id` que tenga alias (`claude`, `cmd:loki`) por el
    /// alias, para `--compact`: una fuente glob se acortaba a `jsonl:*.jsonl`,
    /// que no dice cuál es. `aliases` son pares `(id, alias)`.
    pub fn with_source_aliases(self, aliases: &[(String, String)]) -> Json {
        match self {
            Json::Obj(fields) => Json::Obj(
                fields
                    .into_iter()
                    .map(|(k, v)| match v {
                        Json::Str(s) if k == "source_id" => {
                            let alias = aliases.iter().find(|(id, _)| *id == s).map(|(_, a)| a.clone());
                            (k, Json::Str(alias.unwrap_or(s)))
                        }
                        v => (k, v.with_source_aliases(aliases)),
                    })
                    .collect(),
            ),
            Json::Arr(items) => Json::Arr(items.into_iter().map(|v| v.with_source_aliases(aliases)).collect()),
            other => other,
        }
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

/// Salida compacta para un agente (`--compact`): solo el `result` del envelope, sin metadatos ni
/// `generated_at`, así que el mismo índice da siempre los mismos bytes. Una lista de objetos sale como
/// tabla (una cabecera y una fila por elemento, columnas separadas por tabulador); un objeto anidado se
/// aplana como `clave.subclave`; una lista de valores, separada por comas. Si el presupuesto recortó el
/// resultado, lo dice en una última línea.
impl Json {
    pub fn compact(&self) -> String {
        let mut out = String::new();
        match self.get("result") {
            Some(r) => compact_value("", r, &mut out),
            None => compact_value("", self, &mut out),
        }
        if let Some(tb) = self.get("token_budget") {
            if matches!(tb.get("truncated"), Some(Json::Bool(true))) {
                let n = |k: &str| match tb.get(k) {
                    Some(Json::Int(v)) => v.to_string(),
                    _ => "?".to_string(),
                };
                out.push_str(&format!("truncated: {} of max {} (raise --limit)\n", n("returned"), n("max_items")));
            }
        }
        out
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn is_scalar(&self) -> bool {
        !matches!(self, Json::Arr(_) | Json::Obj(_))
    }
}

fn scalar_text(v: &Json) -> String {
    match v {
        Json::Null => "-".to_string(),
        Json::Bool(b) => b.to_string(),
        Json::Int(n) => n.to_string(),
        Json::Float(f) if f.is_finite() => {
            let s = format!("{:.3}", f);
            let s = s.trim_end_matches('0').trim_end_matches('.');
            if s.is_empty() || s == "-0" { "0".to_string() } else { s.to_string() }
        }
        Json::Float(_) => "-".to_string(),
        Json::Str(s) => s.replace(['\t', '\n', '\r'], " "),
        Json::Arr(items) => {
            if items.is_empty() {
                "-".to_string()
            } else {
                items.iter().map(scalar_text).collect::<Vec<_>>().join(",")
            }
        }
        Json::Obj(fields) => fields.iter().map(|(k, v)| format!("{k}={}", scalar_text(v))).collect::<Vec<_>>().join(","),
    }
}

/// Columnas de una fila: escalares tal cual; un objeto, aplanado un nivel (`clave.subclave`).
fn row_cells(row: &Json, prefix: &str, cols: &mut Vec<(String, String)>) {
    if let Json::Obj(fields) = row {
        for (k, v) in fields {
            let name = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match v {
                Json::Obj(_) if prefix.is_empty() => row_cells(v, &name, cols),
                _ => cols.push((name.clone(), cell_text(&name, v))),
            }
        }
    }
}

/// El texto de una celda. Un `source_id` (`jsonl:/ruta/absoluta/larga/x.jsonl`)
/// se acorta a `tipo:nombre` (`jsonl:x.jsonl`): en una tabla se repite en cada
/// fila y la ruta absoluta era la mayor parte de los bytes. El JSON completo
/// sigue llevando el id entero; `--compact` es para leer, no para citar rutas.
fn cell_text(name: &str, v: &Json) -> String {
    match v {
        Json::Str(s) if name == "source_id" || name.ends_with(".source_id") => short_source_id(s),
        _ => scalar_text(v),
    }
}

/// `tipo:/a/b/c.ext` → `tipo:c.ext`; sin `:` o sin `/`, igual.
fn short_source_id(s: &str) -> String {
    match s.split_once(':') {
        Some((kind, rest)) if rest.contains('/') => {
            let base = rest.trim_end_matches('/').rsplit('/').next().unwrap_or(rest);
            format!("{kind}:{base}").replace(['\t', '\n', '\r'], " ")
        }
        _ => scalar_text(&Json::Str(s.to_string())),
    }
}

fn compact_value(key: &str, v: &Json, out: &mut String) {
    match v {
        Json::Obj(fields) => {
            for (k, val) in fields {
                let name = if key.is_empty() { k.clone() } else { format!("{key}.{k}") };
                compact_value(&name, val, out);
            }
        }
        Json::Arr(items) if !items.is_empty() && items.iter().all(|i| matches!(i, Json::Obj(_))) => {
            // Cabecera: la unión de columnas en orden de aparición (filas heterogéneas incluidas).
            let rows: Vec<Vec<(String, String)>> = items
                .iter()
                .map(|r| {
                    let mut c = Vec::new();
                    row_cells(r, "", &mut c);
                    c
                })
                .collect();
            let mut header: Vec<String> = Vec::new();
            for r in &rows {
                for (c, _) in r {
                    if !header.contains(c) {
                        header.push(c.clone());
                    }
                }
            }
            out.push_str(&format!("{} ({}):\n", if key.is_empty() { "rows" } else { key }, rows.len()));
            out.push_str(&header.join("\t"));
            out.push('\n');
            for r in &rows {
                let line: Vec<String> = header
                    .iter()
                    .map(|h| r.iter().find(|(c, _)| c == h).map(|(_, v)| v.clone()).unwrap_or_else(|| "-".to_string()))
                    .collect();
                out.push_str(&line.join("\t"));
                out.push('\n');
            }
        }
        _ => {
            debug_assert!(v.is_scalar() || matches!(v, Json::Arr(_)));
            out.push_str(&format!("{}: {}\n", if key.is_empty() { "value" } else { key }, cell_text(key, v)));
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
    #[test]
    fn compact_es_tabla_sin_envelope_y_determinista() {
        let env = Json::obj()
            .set("schema_version", Json::Int(5))
            .set("generated_at", Json::str("2026-10-13T00:00:00Z"))
            .set("token_budget", Json::obj().set("max_items", Json::Int(2)).set("returned", Json::Int(2)).set("truncated", Json::Bool(true)))
            .set(
                "result",
                Json::obj().set("for", Json::str("a.rs")).set(
                    "coupled",
                    Json::Arr(vec![
                        Json::obj().set("b", Json::str("b.rs")).set("support", Json::Int(7)).set("confidence", Json::Float(0.8)),
                        Json::obj().set("b", Json::str("c\tx.rs")).set("support", Json::Int(3)).set("confidence", Json::Float(1.0 / 3.0)),
                    ]),
                ),
            );
        let c = env.compact();
        assert_eq!(c, "for: a.rs\ncoupled (2):\nb\tsupport\tconfidence\nb.rs\t7\t0.8\nc x.rs\t3\t0.333\ntruncated: 2 of max 2 (raise --limit)\n");
        assert!(!c.contains("generated_at") && !c.contains("schema_version"));
        assert_eq!(c, env.compact());
    }

    #[test]
    fn compact_acorta_source_id() {
        let r = Json::obj().set(
            "related",
            Json::Arr(vec![Json::obj().set("id", Json::str("a")).set("source_id", Json::str("jsonl:/private/tmp/x/y/logs.jsonl"))]),
        );
        let c = Json::obj().set("result", r.set("anchor", Json::obj().set("source_id", Json::str("git:/Users/j/CRM_Aura")))).compact();
        assert!(c.contains("jsonl:logs.jsonl"), "{c}");
        assert!(c.contains("anchor.source_id: git:CRM_Aura"), "{c}");
        assert!(!c.contains("/private/tmp"), "{c}");
    }

    #[test]
    fn compact_usa_el_alias_de_la_fuente() {
        let r = Json::obj().set(
            "events",
            Json::Arr(vec![
                Json::obj().set("source_id", Json::str("jsonl:/h/.claude/projects/*.jsonl")),
                Json::obj().set("source_id", Json::str("jsonl:/x/app.jsonl")),
            ]),
        );
        let aliases = vec![("jsonl:/h/.claude/projects/*.jsonl".to_string(), "claude".to_string())];
        let c = Json::obj().set("result", r).with_source_aliases(&aliases).compact();
        assert!(c.contains("\nclaude\n") && c.contains("jsonl:app.jsonl") && !c.contains("*.jsonl"), "{c}");
    }

    #[test]
    fn compact_aplana_objetos_y_rellena_columnas() {
        let r = Json::obj().set(
            "owners",
            Json::Arr(vec![
                Json::obj().set("name", Json::str("Ana")).set("tip", Json::obj().set("sha", Json::str("abc"))),
                Json::obj().set("name", Json::str("Luis")).set("tags", Json::Arr(vec![Json::str("x"), Json::str("y")])),
            ]),
        );
        let c = Json::obj().set("result", r).compact();
        assert_eq!(c, "owners (2):\nname\ttip.sha\ttags\nAna\tabc\t-\nLuis\t-\tx,y\n");
    }

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
