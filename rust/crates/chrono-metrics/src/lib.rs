//! `chrono-metrics` — consultas deterministas sobre el índice v2.
//!
//! Opera sobre un `&rusqlite::Connection` (de `chrono_store::Store::conn()`).
//! Paridad con el Go original en `internal/metrics/metrics.go`. Ver
//! `docs/DESIGN-GENERAL-CORE.md §2.3`.
//!
//! Todas las consultas aceptan una ventana temporal `since_epoch: i64`
//! (segundos UTC; 0 = sin límite) que filtra `events.at_epoch >= since_epoch`.

mod churn;
mod coupling;
mod hotspots;
mod owners;
mod search;
mod similar;

pub use churn::{churn, ChurnRow};
pub use coupling::{coupling, Coupled};
pub use hotspots::{hotspots, Hotspot};
pub use owners::{owners, Owner};
pub use search::{search, SearchHit};
pub use similar::{similar, SimilarRow};

/// Error de `chrono-metrics`: no exige un tipo concreto a los llamadores
/// (CLI, tests...).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(test)]
mod tests;
