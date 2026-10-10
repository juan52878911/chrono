//! `chrono-store` — índice SQLite v2 de chrono (esquema, migración, writer).
//!
//! El esquema compartido con `chrono-metrics` está en `schema.sql` (contrato
//! fijo: no se cambian nombres de tabla/columna aquí). Ver
//! `docs/DESIGN-GENERAL-CORE.md §4`.

mod json;
mod store;
mod writer;

pub use store::{SourceRow, Store};
pub use writer::{Writer, WriterCaches};

/// SQL del esquema v2, embebido en el binario.
pub fn schema_sql() -> &'static str {
    include_str!("../schema.sql")
}

/// Versión de esquema que produce este store. Reindex si el índice tiene otra.
/// v3 añadió `templates`/`rollups` (Drain-light, ver `patterns`); v4 cambia la
/// unidad de `events.at_epoch` a MILISEGUNDOS (antes segundos); v5 cambia
/// `events.source_id` (TEXT) por `source_ref` (INTEGER → `sources.ref`) y añade
/// `touches.added`/`deleted` (antes `json_extract` en churn) — reindex avisado.
pub const SCHEMA_VERSION: i64 = 5;

/// Error de `chrono-store`: envuelve I/O y errores de `rusqlite` sin exigir un
/// tipo concreto a los llamadores (CLI, `chrono-metrics`...).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(test)]
mod tests;
