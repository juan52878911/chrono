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
    checkpoint_truncate, count_events_any_version, replace_in_place, snapshot_into, count_source_events_any_version, is_corrupt, read_meta_any_version, read_sources_any_version, reindex_marker, reindex_pending, NoIndex, SchemaMismatch,
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

/// `attr_rollups`: las dims de las fuentes como cubo, para `top attr:<k>` con
/// filtros de nivel/kind/entidad/fuente/ventana sin unir `event_dims` con
/// `events`. Lo mismo que `dim_rollups` sin plantilla y con el valor de la dim
/// (`dim_values.id`): un evento con la dim cuenta una vez por valor, como en
/// `event_dims`. Mismo watermark (`rollup_max_rowid`) y mismo bucket. Solo
/// cubre las claves de `meta.attr_rollup_keys`, que fija el build completo (un
/// índice anterior no la tiene hasta su próximo build completo: `reindex`, o
/// `add` de una fuente ya indexada). No va en `schema.sql`: la crea el build,
/// también en los índices v7 que ya existían.
pub(crate) const ATTR_ROLLUPS_SQL: &str = "CREATE TABLE IF NOT EXISTS attr_rollups (
    dim_id       INTEGER NOT NULL,
    source_ref   INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    level        TEXT NOT NULL,
    level_inf    TEXT NOT NULL,
    entity_id    INTEGER NOT NULL,
    bucket_epoch INTEGER NOT NULL,
    count        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (dim_id, source_ref, kind, level, level_inf, entity_id, bucket_epoch)
) WITHOUT ROWID;";

/// Una clave entra en `attr_rollups` si tiene a lo sumo tantos valores: las de
/// pocos valores (host, contenedor, rama, cwd) son las que se agrupan; una de
/// ids (traza, sesión a millones) daría una fila por evento y no ahorra nada.
pub const ATTR_ROLLUP_MAX_VALUES: i64 = 10_000;

/// Error de `chrono-store`: envuelve I/O y errores de `rusqlite` sin exigir un
/// tipo concreto a los llamadores (CLI, `chrono-metrics`...).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(test)]
mod tests;
