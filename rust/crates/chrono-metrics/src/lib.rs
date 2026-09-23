//! `chrono-metrics` — consultas deterministas sobre el índice v2.
//!
//! Opera sobre un `&rusqlite::Connection` (de `chrono_store::Store::conn()`).
//! Paridad con el Go original en `internal/metrics/metrics.go`. Ver
//! `docs/DESIGN-GENERAL-CORE.md §2.3`.
//!
//! Todas las consultas aceptan una ventana temporal `since_epoch: i64`
//! (segundos UTC; 0 = sin límite) que filtra `events.at_epoch >= since_epoch`.

mod bugs;
mod churn;
mod correlate;
mod coupling;
mod hotspots;
mod owners;
mod phases;
mod search;
mod similar;
mod ticket;
mod timeline;
mod top;

pub use bugs::{bugs, BugArea, BugCategory, Bugs};
pub use churn::{churn, ChurnRow};
pub use correlate::{correlate, Correlation, EventRef};
pub use coupling::{coupling, Coupled};
pub use hotspots::{hotspots, Hotspot};
pub use owners::{owners, Owner};
pub use phases::{phases, Phase};
pub use search::{search, SearchHit};
pub use similar::{similar, SimilarRow};
pub use ticket::{ticket, Ticket};
pub use timeline::{timeline, TimelineBucket};
pub use top::{top, TopValue};

/// Error de `chrono-metrics`: no exige un tipo concreto a los llamadores
/// (CLI, tests...).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Operador SQL de alcance de símbolos (S1). Por defecto las consultas de
/// entidades EXCLUYEN símbolos (`type != 'symbol'`), para no mezclar el touch
/// del fichero con el del símbolo; `--by symbol` (symbols_only=true) invierte
/// el filtro a SOLO símbolos (`type = 'symbol'`). Se compone como
/// `<alias>.type {op} 'symbol'`; literal fijo, no llega texto de usuario al SQL.
pub(crate) fn symbol_type_op(symbols_only: bool) -> &'static str {
    if symbols_only {
        "="
    } else {
        "!="
    }
}

#[cfg(test)]
mod tests;
