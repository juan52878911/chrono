//! `chrono-core` — núcleo de dominio de chrono (port a Rust).
//!
//! Dominio puro, sin I/O ni dependencias: el modelo de evento temporal general
//! (`Event`) y los puertos (`Source`/`Cursor`, `Classifier`). Los adaptadores
//! (git, jsonl, sqlite, jev…) viven en crates aparte y dependen de este.
//!
//! Ver `docs/DESIGN-GENERAL-CORE.md` en la raíz del repo.

pub mod classify;
pub mod domain;
pub mod error;
pub mod source;
pub mod template;

pub use classify::{Chain, Classifier};
pub use template::templatize;
pub use domain::{Actor, Event, Label, Link, Touch};
pub use error::{CoreError, Result};
pub use source::{Cursor, Registry, Source, SourceConfig, Watermark};
