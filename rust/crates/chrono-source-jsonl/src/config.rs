//! Resolución de configuración del adaptador: qué campo de un objeto JSON es
//! el tiempo, el mensaje, el nivel, la entidad y el actor.
//!
//! Los logs JSONL reales suelen ser heterogéneos línea a línea (un logger
//! manda `ts` epoch, otro `@timestamp` ISO); por eso la autodetección de
//! "primer candidato presente" se hace POR LÍNEA (`resolve_field`, usada
//! desde `record::line_to_event`), no una sola vez para todo el fichero.
//! Un override explícito en `SourceConfig.options` sí es global: se aplica
//! igual a todas las líneas.
//!
//! Los selectores pueden ser rutas JSON (`message.role`, `data.0.id`,
//! `message.content[].name`, ver `path`) y, con la opción `explode`, se
//! resuelven en "elemento-luego-raíz" (ver `record`). Un selector sin `.` ni
//! `[` es una clave de primer nivel, igual que antes.
//!
//! `resolve_config`/`JsonlConfig` calculan además una resolución "de
//! muestra" (a partir de la primera línea no vacía del fichero) que solo se
//! usa para `manifest()`: información para el usuario sobre qué campos se
//! autodetectaron, estable entre un `init` y un `sync` incremental.

use crate::path::{self, Scope};
use chrono_core::{CoreError, Result as CoreResult, SourceConfig};
use serde_json::Map;
use serde_json::Value;

pub const TIME_FIELDS: &[&str] = &["ts", "time", "timestamp", "@timestamp", "date", "eventTime"];
pub const MESSAGE_FIELDS: &[&str] = &["msg", "message", "text", "log", "event"];
pub const LEVEL_FIELDS: &[&str] = &["level", "severity", "lvl", "loglevel"];
pub const ENTITY_FIELDS: &[&str] =
    &["service", "logger", "name", "path", "source", "unit", "container", "component"];
pub const ACTOR_FIELDS: &[&str] = &["host", "hostname", "service", "pod", "node"];

/// Opciones que lee el adaptador (ver `Source::options`).
pub const OPTIONS: &[(&str, &str)] = &[
    ("time_field", "campo del tiempo (ruta JSON: a.b, a.0, a[].b)"),
    ("message_field", "campo del título"),
    ("level_field", "campo del nivel"),
    ("entity_field", "campo de la entidad"),
    ("actor_field", "campo del actor"),
    ("kind", "kind fijo de los eventos (por defecto log)"),
    ("kind_field", "campo del que sale el kind"),
    ("id_field", "campo con el id estable del registro"),
    ("uid_field", "id global (a|b): el mismo valor en otra línea u otro fichero es el mismo evento"),
    ("links", "enlaces rel=campo separados por comas"),
    ("where", "solo las líneas con clave=valor o clave=v1|v2 (en la raíz)"),
    ("explode", "ruta a un array: un evento por elemento"),
    ("explode_where", "filtro de explode: clave=valor o clave=v1|v2"),
    ("level_map", "traduce valores del nivel: true=ERROR,false= (vacío = sin nivel)"),
    ("error_message_field", "título de los eventos ERROR: campo o a|b (primera línea, acotada)"),
    ("attrs", "solo estas claves van a attrs (lista con comas; por defecto, todos los escalares)"),
    ("entity_join", "campo=campo_previo: sin entidad, hereda la del evento previo del fichero con ese valor"),
    ("preset", "opciones con nombre: claude-sessions"),
];

/// Presets: conjuntos de opciones con nombre. Las opciones de la fuente pisan
/// a las del preset.
pub const PRESETS: &[(&str, &[(&str, &str)])] = &[(
    // Transcripciones de Claude Code (`~/.claude/projects/**/*.jsonl`): un
    // evento por uso de tool y por resultado. Entidad = la tool (el resultado la
    // hereda de su `tool_use` por `tool_use_id`); nivel ERROR si `is_error`. Ni
    // el input de la tool ni el contenido del resultado entran en el índice,
    // salvo la primera línea de los errores (redactada como todo el título).
    "claude-sessions",
    &[
        // Solo los mensajes: las líneas `api-request-blob` repiten la petición
        // entera a la API (usos y resultados ya contados) y no traen tiempo.
        ("where", "type=assistant|user"),
        ("explode", "message.content"),
        ("explode_where", "type=tool_use|tool_result"),
        ("time_field", "timestamp"),
        ("kind_field", "type"),
        ("entity_field", "name"),
        ("entity_join", "tool_use_id=id"),
        // Una sesión reanudada o bifurcada copia el historial en otro fichero:
        // el mismo `toolu_…` es la misma llamada y cuenta una vez.
        ("uid_field", "id|tool_use_id"),
        ("message_field", "name"),
        ("error_message_field", "content|content.0.text"),
        ("level_field", "is_error"),
        ("level_map", "true=ERROR,false="),
        ("links", "tool_use=id,tool_use=tool_use_id,file=input.file_path,url=input.url"),
        ("attrs", "type,is_error,cwd,gitBranch,sessionId,tool_use_id,id,isSidechain,version"),
    ],
)];

/// Opciones efectivas: las del `preset` (si lo hay) con las de la fuente encima.
/// Error si el preset no existe.
pub fn effective_options(cfg: &SourceConfig) -> CoreResult<SourceConfig> {
    let Some(name) = option_trimmed(&cfg.options, "preset") else { return Ok(cfg.clone()) };
    let Some((_, opts)) = PRESETS.iter().find(|(n, _)| *n == name) else {
        let names: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
        return Err(CoreError::Other(format!("jsonl: preset desconocido {name:?}; hay: {}", names.join(", "))));
    };
    let mut options: std::collections::BTreeMap<String, String> = opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    options.extend(cfg.options.iter().filter(|(k, _)| *k != "preset").map(|(k, v)| (k.clone(), v.clone())));
    Ok(SourceConfig { options })
}

/// Expansión de un array de la línea: una línea → N eventos (ver
/// `SourceConfig.options["explode"]` / `["explode_where"]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplodeSpec {
    /// Ruta (ver `path`) al array a expandir, p.ej. `message.content`.
    pub path: String,
    /// Filtro opcional `(clave, valores)`: solo expanden los elementos objeto
    /// cuyo campo `clave` (ruta relativa al elemento) sea igual a uno de los
    /// `valores` (`type=tool_use|tool_result`).
    pub filter: Option<(String, Vec<String>)>,
}

impl ExplodeSpec {
    /// Elementos que expanden en `root`, como `(índice en el array original,
    /// elemento)`, en el orden natural del array. El índice es el del array
    /// SIN filtrar, para que el `id` de cada evento no dependa del filtro.
    /// Vacío si la ruta falta o no apunta a un array. Los elementos que no son
    /// objeto (escalares, null, arrays) se omiten: no hay de dónde leer campos.
    pub fn elements<'a>(&self, root: &'a Map<String, Value>) -> Vec<(usize, &'a Map<String, Value>)> {
        let Some(Value::Array(items)) = path::lookup(root, &self.path) else {
            return Vec::new();
        };
        items
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_object().map(|m| (i, m)))
            .filter(|(_, m)| self.filter.as_ref().is_none_or(|f| matches_filter(m, f)))
            .collect()
    }
}

/// ¿Casa `obj` con el filtro `(clave, valores)`? (`clave` es una ruta).
pub fn matches_filter(obj: &Map<String, Value>, filter: &(String, Vec<String>)) -> bool {
    path::lookup(obj, &filter.0).and_then(scalar_to_string).is_some_and(|got| filter.1.contains(&got))
}

/// Convierte un valor JSON escalar (string/number/bool) a `String`; `None`
/// para null, array u objeto.
pub fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Overrides globales de `SourceConfig.options`, uno por campo lógico.
/// `None` significa "sin override": cae a la autodetección por línea.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonlOverrides {
    pub time_field: Option<String>,
    pub message_field: Option<String>,
    pub level_field: Option<String>,
    pub entity_field: Option<String>,
    pub actor_field: Option<String>,
    pub explode: Option<ExplodeSpec>,
    /// `where`: `(clave, valores)`; las líneas cuya raíz no casa no dan eventos.
    pub line_filter: Option<(String, Vec<String>)>,
    /// `kind` fijo de los eventos (por defecto `log`): `ci.run`, `deploy`… Es lo
    /// que permite a `correlate --kind`/`events --kind` separar fuentes que no
    /// son logs (ejecuciones de CI, despliegues, incidentes).
    pub kind: Option<String>,
    /// Campo del que sale el `kind` de cada evento (gana a `kind` si resuelve).
    pub kind_field: Option<String>,
    /// Campo con el id estable del registro (`databaseId`, `run_id`…). Sin él,
    /// el id es el de siempre (`id`/`_id`/`uuid` o hash de línea+offset).
    pub id_field: Option<String>,
    /// `uid_field`: alternativas (`a|b`) de un id GLOBAL. Si resuelve, el id del
    /// evento es `<kind>:<valor>`, sin el fichero: repetido en otro fichero, el
    /// índice lo guarda una vez.
    pub uid_field: Vec<String>,
    /// Enlaces `rel=campo` separados por comas (`commit=headSha,url=url`): un
    /// `Link { rel, target: valor del campo }` por cada uno que resuelva. Con
    /// `rel=commit` el evento queda unido al commit (lo siguen `trace` y la
    /// resolución por prefijo de sha).
    pub links: Vec<(String, String)>,
    /// `level_map`: valor crudo del nivel → nivel (`true` → `ERROR`; un nivel
    /// vacío deja el evento sin nivel). Lo que no está en el mapa, tal cual.
    pub level_map: Vec<(String, String)>,
    /// `error_message_field`: alternativas (`a|b`) para el título de los eventos
    /// de nivel ERROR (primera línea, acotada).
    pub error_message_field: Vec<String>,
    /// `attrs`: si no está vacío, solo estas claves (rutas) van a `attrs`.
    pub attrs: Vec<String>,
    /// `entity_join`: `(campo, campo_previo)`. Un evento sin entidad hereda la
    /// del evento previo del mismo fichero cuyo `campo_previo` valga lo que su
    /// `campo` (`tool_use_id=id`: el resultado hereda la tool de su uso).
    pub entity_join: Option<(String, String)>,
}

/// Resolución de campos para una única línea u otra "muestra" representativa
/// (usada para `manifest()`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonlConfig {
    pub time_field: String,
    pub message_field: String,
    pub level_field: String,
    pub entity_field: String,
    pub actor_field: String,
    /// Ruta de `explode` ("" si no hay expansión).
    pub explode: String,
    /// Filtro de `explode_where` como `clave=valor` ("" si no hay).
    pub explode_where: String,
}

/// Primer candidato que resuelve en `scope` (como clave o como ruta), o ""
/// si ninguno.
fn first_present(scope: &Scope<'_>, candidates: &[&str]) -> String {
    candidates.iter().find(|c| scope.get(c).is_some()).map(|c| c.to_string()).unwrap_or_default()
}

/// Valor de override en `options`, ignorando cadenas vacías (se tratan como
/// "sin override": cae a la autodetección).
fn override_of(options: &std::collections::BTreeMap<String, String>, key: &str) -> Option<String> {
    options.get(key).map(String::as_str).filter(|v| !v.is_empty()).map(str::to_string)
}

/// Como `override_of` pero ignorando también las cadenas en blanco (opciones
/// de expansión, donde un valor de solo espacios es "sin valor").
fn option_trimmed(options: &std::collections::BTreeMap<String, String>, key: &str) -> Option<String> {
    options.get(key).map(|v| v.trim()).filter(|v| !v.is_empty()).map(str::to_string)
}

/// Parsea `clave=valor` o `clave=v1|v2` (separa en el primer `=`). `None` si
/// falta el `=` o la clave queda vacía.
fn parse_where(s: &str) -> Option<(String, Vec<String>)> {
    let (k, v) = s.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), v.split('|').map(|x| x.trim().to_string()).collect()))
}

/// `a=b,c=d` → pares con la clave sin espacios; el valor puede quedar vacío.
fn parse_pairs(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|part| {
            let (k, v) = part.split_once('=')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

fn parse_list(s: &str, sep: char) -> Vec<String> {
    s.split(sep).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect()
}

/// Valida las opciones de expansión: `explode_where` requiere `explode` y
/// debe tener la forma `clave=valor`. Se llama al abrir el cursor para que una
/// config errónea falle en alto en vez de no expandir nada en silencio.
pub fn validate_options(cfg: &SourceConfig) -> CoreResult<()> {
    let cfg = &effective_options(cfg)?;
    let explode = option_trimmed(&cfg.options, "explode");
    if let Some(w) = option_trimmed(&cfg.options, "where").filter(|w| parse_where(w).is_none()) {
        return Err(CoreError::Other(format!("jsonl: `where` debe ser `clave=valor` o `clave=v1|v2`, no {w:?}")));
    }
    if let Some(m) = option_trimmed(&cfg.options, "level_map") {
        if m.split(',').any(|p| !p.contains('=') || p.split_once('=').is_some_and(|(k, _)| k.trim().is_empty())) {
            return Err(CoreError::Other(format!("jsonl: `level_map` debe ser `valor=NIVEL,…`, no {m:?}")));
        }
    }
    if let Some(j) = option_trimmed(&cfg.options, "entity_join") {
        if parse_where(&j).is_none_or(|(_, v)| v.len() != 1 || v[0].is_empty()) {
            return Err(CoreError::Other(format!("jsonl: `entity_join` debe ser `campo=campo_previo`, no {j:?}")));
        }
    }
    if let Some(w) = option_trimmed(&cfg.options, "explode_where") {
        if explode.is_none() {
            return Err(CoreError::Other("jsonl: `explode_where` requiere `explode`".to_string()));
        }
        if parse_where(&w).is_none() {
            return Err(CoreError::Other(format!("jsonl: `explode_where` debe ser `clave=valor` o `clave=v1|v2`, no {w:?}")));
        }
    }
    Ok(())
}

pub fn overrides_from(cfg: &SourceConfig) -> JsonlOverrides {
    let explode = option_trimmed(&cfg.options, "explode").map(|p| ExplodeSpec {
        path: path::normalize_array_path(&p),
        filter: option_trimmed(&cfg.options, "explode_where").and_then(|w| parse_where(&w)),
    });
    JsonlOverrides {
        time_field: override_of(&cfg.options, "time_field"),
        message_field: override_of(&cfg.options, "message_field"),
        level_field: override_of(&cfg.options, "level_field"),
        entity_field: override_of(&cfg.options, "entity_field"),
        actor_field: override_of(&cfg.options, "actor_field"),
        explode,
        line_filter: option_trimmed(&cfg.options, "where").and_then(|w| parse_where(&w)),
        kind: override_of(&cfg.options, "kind"),
        kind_field: override_of(&cfg.options, "kind_field"),
        id_field: override_of(&cfg.options, "id_field"),
        uid_field: option_trimmed(&cfg.options, "uid_field").map(|s| parse_list(&s, '|')).unwrap_or_default(),
        links: option_trimmed(&cfg.options, "links").map(|s| parse_links(&s)).unwrap_or_default(),
        level_map: option_trimmed(&cfg.options, "level_map").map(|s| parse_pairs(&s)).unwrap_or_default(),
        error_message_field: option_trimmed(&cfg.options, "error_message_field").map(|s| parse_list(&s, '|')).unwrap_or_default(),
        attrs: option_trimmed(&cfg.options, "attrs").map(|s| parse_list(&s, ',')).unwrap_or_default(),
        entity_join: option_trimmed(&cfg.options, "entity_join")
            .and_then(|s| parse_where(&s))
            .and_then(|(k, mut v)| (v.len() == 1).then(|| (k, v.remove(0)))),
    }
}

/// `rel=campo,rel2=campo2` → pares; ignora los trozos sin `=` o vacíos.
fn parse_links(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|part| {
            let (rel, field) = part.split_once('=')?;
            let (rel, field) = (rel.trim(), field.trim());
            (!rel.is_empty() && !field.is_empty()).then(|| (rel.to_string(), field.to_string()))
        })
        .collect()
}

/// Resuelve el selector para UN ámbito concreto: el override si lo hay (se
/// usa tal cual, exista o no), o si no, el primer candidato que resuelve en
/// `scope` ("" si ninguno). Con `explode`, "resuelve" sigue la regla
/// elemento-luego-raíz de `Scope::get`.
pub fn resolve_field_in(override_value: Option<&str>, candidates: &[&str], scope: &Scope<'_>) -> String {
    match override_value {
        Some(v) => v.to_string(),
        None => first_present(scope, candidates),
    }
}

/// Como `resolve_field_in` sobre un único objeto (sin expansión).
#[cfg(test)]
pub fn resolve_field(override_value: Option<&str>, candidates: &[&str], obj: &Map<String, Value>) -> String {
    resolve_field_in(override_value, candidates, &Scope::root_only(obj))
}

/// Resuelve los 5 campos para `sample` (informativo, para `manifest()`). Con
/// `explode`, el ámbito es el primer elemento expandible de la muestra (y la
/// muestra como raíz); si no hay ninguno, solo la raíz.
pub fn resolve_config(cfg: &SourceConfig, sample: &Map<String, Value>) -> JsonlConfig {
    let ov = overrides_from(cfg);
    let elem = ov.explode.as_ref().and_then(|spec| spec.elements(sample).into_iter().next().map(|(_, m)| m));
    let scope = Scope { elem, root: sample };
    JsonlConfig {
        time_field: resolve_field_in(ov.time_field.as_deref(), TIME_FIELDS, &scope),
        message_field: resolve_field_in(ov.message_field.as_deref(), MESSAGE_FIELDS, &scope),
        level_field: resolve_field_in(ov.level_field.as_deref(), LEVEL_FIELDS, &scope),
        entity_field: resolve_field_in(ov.entity_field.as_deref(), ENTITY_FIELDS, &scope),
        actor_field: resolve_field_in(ov.actor_field.as_deref(), ACTOR_FIELDS, &scope),
        explode: ov.explode.as_ref().map(|e| e.path.clone()).unwrap_or_default(),
        explode_where: ov.explode.as_ref().and_then(|e| e.filter.as_ref()).map(|(k, v)| format!("{k}={}", v.join("|"))).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn autodetecta_primer_presente() {
        let sample = sample_obj(json!({"@timestamp": "x", "msg": "hola", "service": "api", "level": "info"}));
        let cfg = resolve_config(&SourceConfig::default(), &sample);
        assert_eq!(cfg.time_field, "@timestamp");
        assert_eq!(cfg.message_field, "msg");
        assert_eq!(cfg.level_field, "level");
        assert_eq!(cfg.entity_field, "service");
    }

    #[test]
    fn override_gana_a_la_autodeteccion() {
        let sample = sample_obj(json!({"ts": 1, "msg": "hola"}));
        let mut opts = std::collections::BTreeMap::new();
        opts.insert("time_field".to_string(), "customTime".to_string());
        let cfg = resolve_config(&SourceConfig { options: opts }, &sample);
        assert_eq!(cfg.time_field, "customTime");
    }

    #[test]
    fn sin_candidatos_presentes_queda_vacio() {
        let sample = sample_obj(json!({"foo": "bar"}));
        let cfg = resolve_config(&SourceConfig::default(), &sample);
        assert_eq!(cfg.time_field, "");
        assert_eq!(cfg.entity_field, "");
    }

    #[test]
    fn resolve_field_es_por_objeto_no_global() {
        let with_ts = sample_obj(json!({"ts": 1}));
        let with_at_timestamp = sample_obj(json!({"@timestamp": "x"}));
        assert_eq!(resolve_field(None, TIME_FIELDS, &with_ts), "ts");
        assert_eq!(resolve_field(None, TIME_FIELDS, &with_at_timestamp), "@timestamp");
    }
}
