//! Cursor BUFFERED: la salida del comando del SO ya se recolectó entera en
//! `open` (el volumen está acotado por la ventana temporal, ver `lib.rs`),
//! así que aquí solo se itera el `Vec<Event>` ya parseado, en el mismo orden
//! en que lo devolvió el comando (no se reordena: eso rompería el
//! determinismo del `id` de respaldo, que usa el índice del registro).

use chrono_core::{Cursor, Event, Result as CoreResult, Watermark};
use std::collections::BTreeMap;
use std::vec::IntoIter;

pub(crate) struct OsLogCursor {
    events: IntoIter<Event>,
    /// Epoch máximo entre TODOS los eventos recolectados en esta apertura
    /// (no solo los ya devueltos por `next()`): el watermark debe reflejar
    /// hasta dónde llegó esta pasada de ingesta, sin depender de que el
    /// llamador consuma el cursor entero.
    max_epoch: Option<i64>,
    manifest: BTreeMap<String, String>,
}

impl OsLogCursor {
    pub(crate) fn new(events: Vec<Event>, manifest: BTreeMap<String, String>) -> Self {
        // Solo eventos con tiempo válido (`at` no vacío) cuentan para el
        // watermark: un timestamp sin parsear (at_epoch=0) no debe hacer
        // retroceder ni contaminar el "último evento visto".
        let max_epoch = events.iter().filter(|e| !e.at.is_empty()).map(|e| e.at_epoch).max();
        Self { events: events.into_iter(), max_epoch, manifest }
    }
}

impl Cursor for OsLogCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        Ok(self.events.next())
    }

    fn watermark(&self) -> Watermark {
        let value = match self.max_epoch {
            Some(epoch) => format!("since:{epoch}"),
            None => String::new(),
        };
        Watermark { kind: "oslog".to_string(), value }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        self.manifest.clone()
    }
}
