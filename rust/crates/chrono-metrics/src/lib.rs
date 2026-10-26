//! `chrono-metrics` — consultas deterministas sobre el índice v2.
//!
//! Opera sobre un `&rusqlite::Connection` (de `chrono_store::Store::conn()`).
//! Paridad con el Go original en `internal/metrics/metrics.go`. Ver
//! `docs/DESIGN-GENERAL-CORE.md §2.3`.
//!
//! Todas las consultas aceptan una ventana temporal `since_epoch: i64`
//! (segundos UTC; 0 = sin límite) que filtra `events.at_epoch >= since_epoch`.

mod bugs;
mod checks;
mod churn;
mod correlate;
mod coupling;
mod cube;
mod events;
mod explain;
mod hotspots;
mod owners;
mod patterns;
mod phases;
mod precede;
mod relate;
mod search;
mod session_patterns;
mod similar;
mod sources;
mod stats;
mod ticket;
mod timeline;
mod top;
mod trace;

pub use bugs::{bugs, BugArea, BugCategory, Bugs};
pub use checks::{attr_key_known, attr_keys, event_relation_keys, other_timed_sources, sources_of_kind, unknown_levels, untimed_events};
pub use churn::{churn, ChurnRow};
pub use correlate::{Correlation, EventRef, correlate, correlate_kinds};
pub use coupling::{coupling, Coupled};
pub use cube::count_events;
pub use events::{events, events_after_rid, events_ordered, EventFilter, EventRow};
pub use explain::{co_changes, commit_facts, explain, parse_target, utc_date, Blamed, CoChange, CommitFacts, ExplainedCommit, Explanation, FixCertainty, TicketRef};
pub use hotspots::{hotspots, hotspots_of_type, Hotspot};
pub use owners::{owners, Owner};
pub use patterns::{patterns, patterns_by_level, patterns_filtered, patterns_scoped, Pattern};
pub use phases::{phases, Phase};
pub use precede::{precede, Common, Episode, PrecedeOpts, Preceded, Precursor};
pub use relate::{check_join, correlate_via, gaps, parse_side, relation_keys, valid_key, GapUnit, Gaps, GapsOpts, SideError, Via};
pub use search::{search, SearchHit};
pub use session_patterns::{
    session_patterns, Commits, Group, Incomplete, LoopInfo, Merge, Rework, SessionPatterns, SessionPatternsOpts, Summary,
    ToolCount, Tokens,
};
pub use similar::{similar, SimilarRow};
pub use sources::{source_aliases, sources, SourceSummary};
pub use ticket::{ticket, Ticket};
pub use stats::{stats, StatRow};
pub use timeline::{timeline, timeline_scoped, EventScope, TimelineBucket};
pub use top::{top, TopValue, top_filtered, TopFilter};
pub use trace::{trace, Trace, TraceNode};

/// Error de `chrono-metrics`: no exige un tipo concreto a los llamadores
/// (CLI, tests...).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Error de uso con el mensaje en los dos idiomas: `Display` da el español
/// (lo que ve el MCP); el CLI lo baja con `downcast_ref` y elige según `--lang`.
#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub en: String,
    pub es: String,
}

impl Msg {
    pub fn err(en: impl Into<String>, es: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(Msg { en: en.into(), es: es.into() })
    }
}

impl std::fmt::Display for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.es)
    }
}

impl std::error::Error for Msg {}

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
