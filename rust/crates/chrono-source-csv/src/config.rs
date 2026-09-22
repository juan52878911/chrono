//! Resolución de columnas del CSV: qué columna de la cabecera es el tiempo,
//! el mensaje, el nivel, la entidad, el actor y el id explícito.
//!
//! A diferencia de `chrono-source-jsonl` (que resuelve el campo POR LÍNEA,
//! porque un log JSONL puede mezclar formas de una línea a otra), un CSV
//! tiene una única cabecera para todo el fichero: la resolución se hace UNA
//! VEZ contra esa cabecera (requisito del encargo, ver `cursor::CsvCursor`)
//! y se reutiliza, ya como índices, para cada fila.

use chrono_core::SourceConfig;
use std::collections::BTreeMap;

pub const TIME_COLUMNS: &[&str] =
    &["ts", "time", "timestamp", "date", "@timestamp", "datetime", "occurred_at", "event_time"];
pub const MESSAGE_COLUMNS: &[&str] = &["msg", "message", "text", "event", "description", "summary", "detail"];
pub const LEVEL_COLUMNS: &[&str] = &["level", "severity", "lvl", "status", "priority"];
pub const ENTITY_COLUMNS: &[&str] =
    &["service", "endpoint", "path", "resource", "name", "component", "host", "entity", "table"];
pub const ACTOR_COLUMNS: &[&str] = &["host", "user", "author", "service", "actor", "owner"];
pub const ID_COLUMNS: &[&str] = &["id", "_id", "uuid"];

/// Overrides explícitos de `SourceConfig.options`: NOMBRE de columna (no
/// índice), case-insensitive contra la cabecera. `None` = sin override: cae
/// a la autodetección por nombre de columna.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CsvOverrides {
    pub time_column: Option<String>,
    pub message_column: Option<String>,
    pub level_column: Option<String>,
    pub entity_column: Option<String>,
    pub actor_column: Option<String>,
    pub id_column: Option<String>,
}

/// Valor de override en `options`, ignorando cadenas vacías (se tratan como
/// "sin override": cae a la autodetección).
fn override_of(options: &BTreeMap<String, String>, key: &str) -> Option<String> {
    options.get(key).map(String::as_str).filter(|v| !v.is_empty()).map(str::to_string)
}

pub fn overrides_from(cfg: &SourceConfig) -> CsvOverrides {
    CsvOverrides {
        time_column: override_of(&cfg.options, "time_column"),
        message_column: override_of(&cfg.options, "message_column"),
        level_column: override_of(&cfg.options, "level_column"),
        entity_column: override_of(&cfg.options, "entity_column"),
        actor_column: override_of(&cfg.options, "actor_column"),
        id_column: override_of(&cfg.options, "id_column"),
    }
}

/// Delimitador configurado: coma por defecto. Acepta un único carácter
/// literal (p.ej. un tab real pegado en el JSON de config) o la cadena de
/// escape de dos caracteres `"\t"` (la forma habitual de escribir un tab en
/// un fichero de configuración de texto).
pub fn resolve_delimiter(cfg: &SourceConfig) -> char {
    match cfg.options.get("delimiter").map(String::as_str) {
        None | Some("") => ',',
        Some("\\t") => '\t',
        Some(s) => s.chars().next().unwrap_or(','),
    }
}

/// Índice de la primera columna de `candidates` presente en `header`
/// (comparación case-insensitive), o `None` si ninguna lo está.
fn first_present(header: &[String], candidates: &[&str]) -> Option<usize> {
    candidates.iter().find_map(|c| header.iter().position(|h| h.eq_ignore_ascii_case(c)))
}

/// Índice de la columna `name` en `header` (comparación case-insensitive), o
/// `None` si no existe (override apuntando a una columna inexistente: se
/// ignora, no se aborta la ingesta).
fn index_of(header: &[String], name: &str) -> Option<usize> {
    header.iter().position(|h| h.eq_ignore_ascii_case(name))
}

/// Columnas resueltas contra la cabecera: un índice por dimensión lógica.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedColumns {
    pub time: Option<usize>,
    pub message: Option<usize>,
    pub level: Option<usize>,
    pub entity: Option<usize>,
    pub actor: Option<usize>,
    pub id: Option<usize>,
}

impl ResolvedColumns {
    /// Nombre de cabecera de una columna resuelta, o "" si no se resolvió
    /// ninguna (para `manifest()`).
    pub fn name(&self, header: &[String], idx: Option<usize>) -> String {
        idx.and_then(|i| header.get(i)).cloned().unwrap_or_default()
    }
}

/// Resuelve las 6 columnas UNA VEZ contra `header` (nombres de la cabecera,
/// en el orden del fichero): override si lo hay, si no autodetección por
/// nombre de columna.
pub fn resolve_columns(header: &[String], ov: &CsvOverrides) -> ResolvedColumns {
    let resolve = |over: &Option<String>, candidates: &[&str]| -> Option<usize> {
        match over {
            Some(name) => index_of(header, name),
            None => first_present(header, candidates),
        }
    };
    ResolvedColumns {
        time: resolve(&ov.time_column, TIME_COLUMNS),
        message: resolve(&ov.message_column, MESSAGE_COLUMNS),
        level: resolve(&ov.level_column, LEVEL_COLUMNS),
        entity: resolve(&ov.entity_column, ENTITY_COLUMNS),
        actor: resolve(&ov.actor_column, ACTOR_COLUMNS),
        id: resolve(&ov.id_column, ID_COLUMNS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(cols: &[&str]) -> Vec<String> {
        cols.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn autodetecta_por_nombre_de_cabecera_case_insensitive() {
        let h = header(&["Timestamp", "Message", "Level", "Service"]);
        let cols = resolve_columns(&h, &CsvOverrides::default());
        assert_eq!(cols.time, Some(0));
        assert_eq!(cols.message, Some(1));
        assert_eq!(cols.level, Some(2));
        assert_eq!(cols.entity, Some(3));
    }

    #[test]
    fn override_gana_a_la_autodeteccion() {
        let h = header(&["ts", "msg", "customTime"]);
        let ov = CsvOverrides { time_column: Some("customTime".to_string()), ..Default::default() };
        let cols = resolve_columns(&h, &ov);
        assert_eq!(cols.time, Some(2));
    }

    #[test]
    fn override_a_columna_inexistente_queda_sin_resolver() {
        let h = header(&["a", "b"]);
        let ov = CsvOverrides { time_column: Some("no-existe".to_string()), ..Default::default() };
        let cols = resolve_columns(&h, &ov);
        assert_eq!(cols.time, None);
    }

    #[test]
    fn sin_candidatos_presentes_queda_sin_resolver() {
        let h = header(&["foo", "bar"]);
        let cols = resolve_columns(&h, &CsvOverrides::default());
        assert_eq!(cols.time, None);
        assert_eq!(cols.entity, None);
    }

    #[test]
    fn delimitador_por_defecto_coma() {
        assert_eq!(resolve_delimiter(&SourceConfig::default()), ',');
    }

    #[test]
    fn delimitador_tab_por_escape_o_literal() {
        let mut opts = BTreeMap::new();
        opts.insert("delimiter".to_string(), "\\t".to_string());
        assert_eq!(resolve_delimiter(&SourceConfig { options: opts }), '\t');

        let mut opts2 = BTreeMap::new();
        opts2.insert("delimiter".to_string(), "\t".to_string());
        assert_eq!(resolve_delimiter(&SourceConfig { options: opts2 }), '\t');
    }
}
