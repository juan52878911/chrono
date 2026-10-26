//! `chrono-store` — índice SQLite de chrono (esquema, migración, writer).
//!
//! El esquema compartido con `chrono-metrics` está en `schema.sql` (contrato
//! fijo: no se cambian nombres de tabla/columna aquí). Ver
//! `docs/DESIGN-GENERAL-CORE.md §4`.

mod json;
mod legacy;
mod store;
mod writer;

pub use legacy::{
    count_events_any_version, is_corrupt, read_meta_any_version, read_sources_any_version, reindex_marker, reindex_pending, SchemaMismatch,
};
pub use store::{SourceRow, Store};
pub use writer::{Writer, WriterCaches};

/// SQL del esquema, embebido en el binario.
pub fn schema_sql() -> &'static str {
    include_str!("../schema.sql")
}

/// Versión de esquema que produce este store. Reindex si el índice tiene otra.
/// v3 añadió `templates`/`rollups` (Drain-light, ver `patterns`); v4 cambia la
/// unidad de `events.at_epoch` a MILISEGUNDOS (antes segundos); v5 cambia
/// `events.source_id` (TEXT) por `source_ref` (INTEGER → `sources.ref`) y añade
/// `touches.added`/`deleted` (antes `json_extract` en churn); v6 da a `events`
/// una clave entera (`rid`) que usan touches/labels/links, deriva `at` de
/// `at_epoch`, saca las `dims` de cada fuente de `attrs` a `event_dims`, hace
/// parciales los índices de actor y SimHash y añade `dim_rollups` — reindex avisado;
/// v7 interna los `links` (`link_targets` + `event_links`, con la vista `links`):
/// un índice v6 se migra al abrirlo, sin reindexar.
pub const SCHEMA_VERSION: i64 = 7;

/// Ancho del bucket de `dim_rollups` (10 min): `informe` pide picos de 10 min y
/// a 1M eventos de logs deja ~10^4-10^5 filas. Va también a `meta.dim_rollup_bucket_ms`.
pub const DIM_ROLLUP_BUCKET_MS: i64 = 600_000;

/// Error de `chrono-store`: envuelve I/O y errores de `rusqlite` sin exigir un
/// tipo concreto a los llamadores (CLI, `chrono-metrics`...).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(test)]
mod tests;
