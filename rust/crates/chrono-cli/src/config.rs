//! `chrono config check`: valida `.chrono/config.json` y explica cada problema
//! con la ruta de la clave (`source_options["app.log"].tim_field`).
//!
//! La carga normal (`chrono_classify_rules::load`) es tolerante: ignora claves
//! que no conoce y, si un tipo no casa, cae a los valores por defecto. Aquí se
//! dice qué se ignoraría y por qué, sin abrir ninguna fuente.

use crate::i18n::t;
use chrono_classify_rules::CONFIG_VERSION;
use chrono_core::{Registry, SourceConfig};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// Un problema de la config: `error` si cambia lo que chrono hace (se ignora o
/// invalida el fichero), aviso si no.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub msg: String,
    pub error: bool,
}

/// Opciones que valen para cualquier fuente (las lee el CLI, no el adaptador).
const COMMON_OPTIONS: &[(&str, &str)] = &[
    ("adapter", "fuerza el adaptador (como --as)"),
    ("redact", "\"off\" desactiva la redacción de secretos"),
    ("dims", "claves de attrs guardadas como dims indexadas, con comas (top attr:k y --attr k:v sin recorrer events)"),
    ("entity_via", "rel de link: un evento sin entidad toma la del evento con su misma clave (resultado → uso)"),
];

const TOP_KEYS: &[&str] = &[
    "version",
    "fix_keywords",
    "ticket_patterns",
    "exclude_globs",
    "bug_labels",
    "bug_categories",
    "source_options",
    "body_max_bytes",
    "max_events_per_source",
    "commands",
    "intermediario",
    "relations",
];

const COMMAND_KEYS: &[&str] = &["run", "records", "options", "timeout_secs", "cwd"];
const INTERMEDIARIO_KEYS: &[&str] = &["enabled", "verify", "rules", "read"];
const RELATION_KEYS: &[&str] = &["source", "field", "pattern", "len"];

struct Checker<'a> {
    registry: &'a Registry,
    root: &'a Path,
    out: Vec<Finding>,
}

/// Ruta legible de una clave hija: `a.b`, o `a["x.log"]` si no es un identificador.
fn child(parent: &str, key: &str) -> String {
    let ident = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    match (parent.is_empty(), ident) {
        (true, true) => key.to_string(),
        (true, false) => format!("[{key:?}]"),
        (false, true) => format!("{parent}.{key}"),
        (false, false) => format!("{parent}[{key:?}]"),
    }
}

/// Distancia de edición (para sugerir la clave que se quiso escribir).
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn suggest<'k>(key: &str, known: impl Iterator<Item = &'k str>) -> Option<&'k str> {
    known.map(|k| (distance(key, k), k)).filter(|(d, _)| *d <= 2).min().map(|(_, k)| k)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

impl Checker<'_> {
    fn err(&mut self, path: &str, en: String, es: String) {
        self.out.push(Finding { path: path.to_string(), msg: t(en, es), error: true });
    }

    fn warn(&mut self, path: &str, en: String, es: String) {
        self.out.push(Finding { path: path.to_string(), msg: t(en, es), error: false });
    }

    fn unknown_key(&mut self, path: &str, key: &str, known: &[&str], error: bool) {
        let hint = suggest(key, known.iter().copied()).map(|k| format!(" (¿{k}?)")).unwrap_or_default();
        let list = known.join(", ");
        let f = Finding {
            path: path.to_string(),
            msg: t(
                format!("unknown key, ignored{hint}; accepted: {list}"),
                format!("clave desconocida, se ignora{hint}; se aceptan: {list}"),
            ),
            error,
        };
        self.out.push(f);
    }

    fn want_type(&mut self, path: &str, v: &Value, want: &str) -> bool {
        let ok = match want {
            "string" => v.is_string(),
            "bool" => v.is_boolean(),
            "array" => v.is_array(),
            "object" => v.is_object(),
            "uint" => v.as_u64().is_some(),
            _ => true,
        };
        if !ok {
            let got = type_name(v);
            let want_es = match want {
                "string" => "texto",
                "uint" => "entero ≥ 0",
                "array" => "lista",
                "object" => "objeto",
                w => w,
            };
            self.err(
                path,
                format!("must be {want}, is {got}: the whole file falls back to defaults"),
                format!("debe ser {want_es} y es {got}: todo el fichero cae a los valores por defecto"),
            );
        }
        ok
    }

    fn string_list(&mut self, path: &str, v: &Value) {
        if !self.want_type(path, v, "array") {
            return;
        }
        for (i, item) in v.as_array().into_iter().flatten().enumerate() {
            self.want_type(&format!("{path}[{i}]"), item, "string");
        }
    }

    fn check(&mut self, v: &Value) {
        let Some(top) = v.as_object() else {
            self.err("", "the file must be a JSON object".into(), "el fichero debe ser un objeto JSON".into());
            return;
        };
        match top.get("version") {
            None => self.warn(
                "version",
                format!("missing: version 1 assumed (current: {CONFIG_VERSION})"),
                format!("falta: se asume la versión 1 (la actual es {CONFIG_VERSION})"),
            ),
            Some(v) => match v.as_u64() {
                Some(n) if (1..=u64::from(CONFIG_VERSION)).contains(&n) => {}
                Some(n) => self.err(
                    "version",
                    format!("version {n} not supported by this binary (up to {CONFIG_VERSION}): update chrono"),
                    format!("versión {n} no soportada por este binario (hasta {CONFIG_VERSION}): actualiza chrono"),
                ),
                None => {
                    self.want_type("version", v, "uint");
                }
            },
        }
        for (k, v) in top {
            let p = child("", k);
            match k.as_str() {
                "version" | "$schema" => {}
                "fix_keywords" | "ticket_patterns" | "exclude_globs" | "bug_labels" => self.string_list(&p, v),
                "bug_categories" => {
                    if self.want_type(&p, v, "object") {
                        for (cat, words) in v.as_object().into_iter().flatten() {
                            self.string_list(&child(&p, cat), words);
                        }
                    }
                }
                "body_max_bytes" | "max_events_per_source" => {
                    self.want_type(&p, v, "uint");
                }
                "source_options" => {
                    if self.want_type(&p, v, "object") {
                        for (key, opts) in v.as_object().into_iter().flatten() {
                            self.source_options(&child(&p, key), key, opts);
                        }
                    }
                }
                "commands" => {
                    if self.want_type(&p, v, "object") {
                        for (name, spec) in v.as_object().into_iter().flatten() {
                            self.command(&child(&p, name), spec);
                        }
                    }
                }
                "intermediario" => self.intermediario(&p, v),
                "relations" => {
                    if self.want_type(&p, v, "object") {
                        for (name, sides) in v.as_object().into_iter().flatten() {
                            self.relation(&child(&p, name), name, sides);
                        }
                    }
                }
                _ => self.unknown_key(&p, k, TOP_KEYS, true),
            }
        }
    }

    /// Opciones planas de texto; un valor que no es texto invalida todo el fichero.
    fn flat_options(&mut self, path: &str, v: &Value) -> Option<BTreeMap<String, String>> {
        if !self.want_type(path, v, "object") {
            return None;
        }
        let mut out = BTreeMap::new();
        let mut ok = true;
        for (k, val) in v.as_object().into_iter().flatten() {
            if self.want_type(&child(path, k), val, "string") {
                out.insert(k.clone(), val.as_str().unwrap_or_default().to_string());
            } else {
                ok = false;
            }
        }
        ok.then_some(out)
    }

    /// Una entrada de `source_options` (clave = ruta, nombre de fichero o glob).
    fn source_options(&mut self, path: &str, key: &str, v: &Value) {
        let Some(opts) = self.flat_options(path, v) else { return };
        // Adaptador: el de `adapter`; si no, el que detectaría la ruta si existe.
        let adapter = match opts.get("adapter") {
            Some(kind) => match self.registry.by_kind(kind) {
                Some(a) => Some(a),
                None => {
                    let kinds: Vec<&str> = self.registry.all().map(|s| s.kind()).collect();
                    self.err(
                        &child(path, "adapter"),
                        format!("unknown adapter {kind:?}; available: {}", kinds.join(", ")),
                        format!("adaptador desconocido {kind:?}; hay: {}", kinds.join(", ")),
                    );
                    return;
                }
            },
            None => {
                let p = Path::new(key);
                let p = if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) };
                p.is_file().then(|| self.registry.pick(&p)).flatten()
            }
        };
        if let Some(r) = opts.get("redact").filter(|r| *r != "off" && *r != "on") {
            self.err(&child(path, "redact"), format!("{r:?}: only \"off\" or \"on\""), format!("{r:?}: solo \"off\" u \"on\""));
        }
        // Cada dim es una clave de attrs: los mismos caracteres que `top attr:<clave>`.
        for d in opts.get("dims").map(|d| d.split(',').map(str::trim).collect::<Vec<_>>()).unwrap_or_default() {
            if d.is_empty() || !d.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
                self.err(&child(path, "dims"), format!("{d:?}: a dim is an attrs key ([A-Za-z0-9_.-])"), format!("{d:?}: una dim es una clave de attrs ([A-Za-z0-9_.-])"));
            }
        }
        match adapter {
            Some(a) => {
                let known: Vec<&str> = COMMON_OPTIONS.iter().chain(a.options()).map(|(k, _)| *k).collect();
                for k in opts.keys().filter(|k| !known.contains(&k.as_str())) {
                    let p = child(path, k);
                    self.unknown_key(&p, k, &known, true);
                }
                if let Err(e) = a.validate(&SourceConfig { options: opts.clone() }) {
                    self.err(path, e.to_string(), e.to_string());
                }
            }
            None => {
                // Sin adaptador conocido: vale cualquier opción de alguno.
                let all: Vec<&str> = COMMON_OPTIONS.iter().map(|(k, _)| *k).chain(self.registry.all().flat_map(|s| s.options().iter().map(|(k, _)| *k))).collect();
                for k in opts.keys().filter(|k| !all.contains(&k.as_str())) {
                    let p = child(path, k);
                    self.unknown_key(&p, k, &all, true);
                }
            }
        }
    }

    fn command(&mut self, path: &str, v: &Value) {
        let Some(spec) = v.as_object() else {
            self.want_type(path, v, "object");
            return;
        };
        match spec.get("run") {
            None => self.err(&child(path, "run"), "missing: what to run (argv list)".into(), "falta: qué ejecutar (lista argv)".into()),
            Some(run) => {
                self.string_list(&child(path, "run"), run);
                if run.as_array().is_some_and(Vec::is_empty) {
                    self.err(&child(path, "run"), "empty".into(), "vacía".into());
                }
            }
        }
        for (k, val) in spec {
            let p = child(path, k);
            match k.as_str() {
                "run" => {}
                "records" | "cwd" => {
                    self.want_type(&p, val, "string");
                }
                "timeout_secs" => {
                    self.want_type(&p, val, "uint");
                }
                "options" => {
                    if let Some(opts) = self.flat_options(&p, val) {
                        let jsonl = self.registry.by_kind("jsonl");
                        let mut wrapped = Map::new();
                        wrapped.insert("adapter".into(), Value::String("jsonl".into()));
                        wrapped.extend(opts.into_iter().map(|(k, v)| (k, Value::String(v))));
                        if jsonl.is_some() {
                            self.source_options(&p, "", &Value::Object(wrapped));
                        }
                    }
                }
                _ => self.unknown_key(&p, k, COMMAND_KEYS, true),
            }
        }
    }

    /// Una relación: nombre de clave y lista de extremos `{source, field, pattern, len}`.
    fn relation(&mut self, path: &str, name: &str, v: &Value) {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
            self.err(path, format!("{name:?}: a relation name is a key ([A-Za-z0-9_.-])"), format!("{name:?}: el nombre de una relación es una clave ([A-Za-z0-9_.-])"));
        }
        if !self.want_type(path, v, "array") {
            return;
        }
        let sides = v.as_array().map(Vec::as_slice).unwrap_or_default();
        if sides.is_empty() {
            self.warn(path, "no sides: relates nothing".into(), "sin extremos: no relaciona nada".into());
        }
        for (i, side) in sides.iter().enumerate() {
            let sp = format!("{path}[{i}]");
            if !self.want_type(&sp, side, "object") {
                continue;
            }
            for (k, val) in side.as_object().into_iter().flatten() {
                let p = child(&sp, k);
                match k.as_str() {
                    "source" | "field" => {
                        self.want_type(&p, val, "string");
                    }
                    "len" => {
                        self.want_type(&p, val, "uint");
                    }
                    "pattern" => {
                        if self.want_type(&p, val, "string") {
                            if let Err(e) = chrono_core::Pattern::parse(val.as_str().unwrap_or_default()) {
                                self.err(&p, format!("invalid pattern: {e}"), format!("patrón no válido: {e}"));
                            }
                        }
                    }
                    _ => self.unknown_key(&p, k, RELATION_KEYS, true),
                }
            }
        }
    }

    fn intermediario(&mut self, path: &str, v: &Value) {
        let Some(obj) = v.as_object() else {
            self.want_type(path, v, "object");
            return;
        };
        for (k, val) in obj {
            let p = child(path, k);
            match k.as_str() {
                "enabled" | "verify" => {
                    self.want_type(&p, val, "bool");
                }
                "read" => {
                    self.want_type(&p, val, "object");
                }
                "rules" => {
                    if !self.want_type(&p, val, "array") {
                        continue;
                    }
                    for (i, rule) in val.as_array().into_iter().flatten().enumerate() {
                        let rp = format!("{p}[{i}]");
                        if !self.want_type(&rp, rule, "object") {
                            continue;
                        }
                        match rule.get("tool") {
                            Some(tool) => {
                                self.want_type(&child(&rp, "tool"), tool, "string");
                            }
                            None => self.err(&child(&rp, "tool"), "missing: which tool the rule applies to".into(), "falta: a qué tool se aplica la regla".into()),
                        }
                        if let Some(a) = rule.get("adapter").and_then(Value::as_str).filter(|a| !matches!(*a, "loki" | "records")) {
                            self.err(&child(&rp, "adapter"), format!("{a:?}: only loki or records"), format!("{a:?}: solo loki o records"));
                        }
                    }
                }
                _ => self.unknown_key(&p, k, INTERMEDIARIO_KEYS, false),
            }
        }
    }
}

/// Valida el texto de una config. `root` es la raíz del índice (para resolver
/// las claves de `source_options` que son rutas relativas).
pub fn check(text: &str, registry: &Registry, root: &Path) -> Vec<Finding> {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return vec![Finding {
                path: format!("{}:{}", e.line(), e.column()),
                msg: t(format!("invalid JSON: {e}"), format!("JSON no válido: {e}")),
                error: true,
            }]
        }
    };
    let mut c = Checker { registry, root, out: Vec::new() };
    c.check(&v);
    c.out
}

/// `chrono config check`: imprime los problemas de `<root>/.chrono/config.json`
/// (y del antiguo `intermediario.json` si sigue ahí). Devuelve si hay errores.
pub fn run_check(root: &Path, registry: &Registry) -> bool {
    let path = chrono_classify_rules::config_path(root);
    let legacy = root.join(".chrono/intermediario.json");
    let mut findings: Vec<(String, Finding)> = Vec::new();
    let mut has_config = false;
    if let Ok(text) = std::fs::read_to_string(&path) {
        has_config = true;
        findings.extend(check(&text, registry, root).into_iter().map(|f| ("config.json".to_string(), f)));
        let has_key = serde_json::from_str::<Value>(&text).ok().is_some_and(|v| v.get("intermediario").is_some());
        if has_key && legacy.is_file() {
            findings.push((
                "intermediario.json".into(),
                Finding {
                    path: String::new(),
                    msg: t(
                        "ignored: config.json already has \"intermediario\" (delete this file)",
                        "se ignora: config.json ya tiene \"intermediario\" (borra este fichero)",
                    ),
                    error: false,
                },
            ));
        }
    }
    if let Ok(text) = std::fs::read_to_string(&legacy) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let mut c = Checker { registry, root, out: Vec::new() };
            c.intermediario("intermediario", &v);
            findings.extend(c.out.into_iter().map(|f| ("intermediario.json".to_string(), f)));
        }
        findings.push((
            "intermediario.json".into(),
            Finding {
                path: String::new(),
                msg: t(
                    "legacy file: move its content under \"intermediario\" in config.json",
                    "fichero antiguo: mueve su contenido a \"intermediario\" en config.json",
                ),
                error: false,
            },
        ));
    }
    if !has_config {
        eprintln!("{}", t(
            format!("{}: not found (defaults in use)", path.display()),
            format!("{}: no existe (se usan los valores por defecto)", path.display()),
        ));
    }
    let errors = findings.iter().filter(|(_, f)| f.error).count();
    for (file, f) in &findings {
        let mark = if f.error { "✗" } else { "·" };
        let at = if f.path.is_empty() { file.clone() } else { format!("{file} {}", f.path) };
        println!("{mark} {at}: {}", f.msg);
    }
    let warnings = findings.len() - errors;
    if has_config && findings.is_empty() {
        println!("{}", t(format!("{}: ok (version {CONFIG_VERSION})", path.display()), format!("{}: correcto (versión {CONFIG_VERSION})", path.display())));
    } else if has_config || !findings.is_empty() {
        println!("{}", t(format!("{errors} errors, {warnings} warnings"), format!("{errors} errores, {warnings} avisos")));
    }
    errors > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(text: &str) -> Vec<(String, bool)> {
        let reg = crate::index::build_registry();
        check(text, &reg, Path::new("/nonexistent")).into_iter().map(|f| (f.path, f.error)).collect()
    }

    #[test]
    fn config_minima_correcta() {
        assert!(findings(r#"{"version": 1}"#).is_empty());
    }

    #[test]
    fn json_roto_dice_linea_y_columna() {
        assert_eq!(findings("{\n  \"version\": 1,\n}"), [("3:1".to_string(), true)]);
    }

    #[test]
    fn clave_desconocida_con_ruta_y_sugerencia() {
        let reg = crate::index::build_registry();
        let f = check(r#"{"version":1,"source_options":{"app.log":{"adapter":"jsonl","tim_field":"ts"}}}"#, &reg, Path::new("/x"));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, r#"source_options["app.log"].tim_field"#);
        assert!(f[0].msg.contains("time_field"), "{}", f[0].msg);
    }

    #[test]
    fn tipos_que_invalidan_el_fichero() {
        let f = findings(r#"{"version":1,"body_max_bytes":"10","source_options":{"a.log":{"year":2024}},"fix_keywords":["fix",3]}"#);
        let paths: Vec<&str> = f.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["body_max_bytes", r#"source_options["a.log"].year"#, "fix_keywords[1]"], "en el orden del fichero");
        assert!(f.iter().all(|(_, e)| *e));
    }

    #[test]
    fn version_futura_y_ausente() {
        assert_eq!(findings(r#"{"version": 99}"#), [("version".to_string(), true)]);
        assert_eq!(findings("{}"), [("version".to_string(), false)]);
    }

    #[test]
    fn valores_de_opciones_los_valida_el_adaptador() {
        let f = findings(r#"{"version":1,"source_options":{"x":{"adapter":"jsonl","explode_where":"type=a"}}}"#);
        assert_eq!(f, [(r#"source_options.x"#.to_string(), true)]);
        let f = findings(r#"{"version":1,"source_options":{"x":{"adapter":"nope"}}}"#);
        assert_eq!(f, [(r#"source_options.x.adapter"#.to_string(), true)]);
        // `dims` es común a todos los adaptadores; cada una, una clave de attrs.
        assert!(findings(r#"{"version":1,"source_options":{"x":{"adapter":"jsonl","dims":"host, container"}}}"#).is_empty());
        let f = findings(r#"{"version":1,"source_options":{"x":{"adapter":"jsonl","dims":"host,$bad"}}}"#);
        assert_eq!(f, [(r#"source_options.x.dims"#.to_string(), true)]);
    }

    #[test]
    fn relaciones() {
        assert!(findings(r#"{"version":2,"relations":{"prueba":[{"source":"pruebas","field":"file","pattern":"prueba-(\\d+)"},{"field":"title","len":7}]}}"#).is_empty());
        let f = findings(r#"{"version":2,"relations":{"$x":[{"sorce":"a","pattern":"(a","len":"7"}],"vacia":[]}}"#);
        let has = |path: &str, error: bool| f.iter().any(|(p, e)| p == path && *e == error);
        assert!(has(r#"relations["$x"]"#, true), "{f:?}");
        assert!(has(r#"relations["$x"][0].sorce"#, true), "{f:?}");
        assert!(has(r#"relations["$x"][0].pattern"#, true), "{f:?}");
        assert!(has(r#"relations["$x"][0].len"#, true), "{f:?}");
        assert!(has("relations.vacia", false), "{f:?}");
        assert_eq!(f.len(), 5, "{f:?}");
    }

    #[test]
        fn comandos_e_intermediario() {
        let f = findings(
            r#"{"version":1,"commands":{"gh":{"run":[],"option":{}}},
               "intermediario":{"enabled":"si","rules":[{"adapter":"x"}],"extra":1}}"#,
        );
        let got: Vec<(&str, bool)> = f.iter().map(|(p, e)| (p.as_str(), *e)).collect();
        assert_eq!(
            got,
            [
                ("commands.gh.run", true),
                ("commands.gh.option", true),
                ("intermediario.enabled", true),
                ("intermediario.rules[0].tool", true),
                ("intermediario.rules[0].adapter", true),
                ("intermediario.extra", false),
            ]
        );
    }
}
