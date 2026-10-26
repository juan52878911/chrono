//! Puerto Source: cada tipo de traza (git, jsonl, csv, textlog, changelog,
//! journald) implementa estos traits. El orquestador de ingesta solo conoce
//! los traits, nunca git ni un formato concreto.

use crate::domain::Event;
use crate::error::Result;
use std::collections::BTreeMap;
use std::path::Path;

/// Marca de progreso opaca y serializable. El adaptador decide su contenido:
/// git → "sha:<HEAD>"; fichero → "off:<bytes>|hash-4k"; journald → "cursor:<s>".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Watermark {
    pub kind: String,
    pub value: String,
}

/// Configuración por fuente (columnas, formato de tiempo, presets…),
/// cargada desde `.chrono/config.json`. Se mantiene abierta a propósito.
#[derive(Debug, Clone, Default)]
pub struct SourceConfig {
    pub options: BTreeMap<String, String>,
}

/// Un tipo de traza reconocible e ingeríble.
pub trait Source {
    /// "git" | "jsonl" | "csv" | "syslog" | "nginx" | "journald" | "changelog".
    fn kind(&self) -> &str;

    /// Confianza de que `path` es de este tipo: 0 = no reconocido; mayor = más seguro.
    /// `detect` DEBE devolver 0 ante algo que no entiende (no ingerir basura).
    fn detect(&self, path: &Path) -> i32;

    /// Abre un cursor desde el watermark dado (o desde el principio si es `None`).
    /// Si el watermark ya no casa, devuelve `CoreError::Diverged`.
    fn open(
        &self,
        path: &Path,
        watermark: Option<Watermark>,
        cfg: &SourceConfig,
    ) -> Result<Box<dyn Cursor>>;

    /// Opciones que el adaptador lee de `SourceConfig.options`, como
    /// `(clave, ayuda)`. Es lo que `chrono config check` usa para señalar claves
    /// desconocidas; las comunes a todas las fuentes (`adapter`, `redact`) no van aquí.
    fn options(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Comprueba los VALORES de las opciones (formas `clave=valor`, opciones que
    /// requieren otra…) sin abrir nada. `chrono config check` la llama por fuente.
    fn validate(&self, _cfg: &SourceConfig) -> Result<()> {
        Ok(())
    }

    /// Claves de `attrs` que el índice guarda como dims (`event_dims`): la opción
    /// común `dims`, o la de un preset del adaptador.
    fn dims(&self, cfg: &SourceConfig) -> Vec<String> {
        dims_option(cfg)
    }

    /// `entity_via`: el `rel` de link por el que un evento sin entidad toma la
    /// del evento con su misma clave (ver el CLI). La opción común, o la de un
    /// preset del adaptador.
    fn entity_via(&self, cfg: &SourceConfig) -> Option<String> {
        cfg.options.get("entity_via").map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// Opciones con que de verdad se ingiere (un preset del adaptador ya
    /// desplegado): el CLI guarda su firma con la fuente y, si cambian, la reindexa.
    fn ingest_options(&self, cfg: &SourceConfig) -> SourceConfig {
        cfg.clone()
    }
}

/// La opción `dims` (`"host,container"`) como lista ordenada y sin repetidos.
pub fn dims_option(cfg: &SourceConfig) -> Vec<String> {
    let mut dims: Vec<String> =
        cfg.options.get("dims").map(|s| s.split(',').map(str::trim).filter(|k| !k.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    dims.sort();
    dims.dedup();
    dims
}

/// Cursor de streaming sobre una fuente: entrega eventos uno a uno con RAM constante.
pub trait Cursor {
    /// Siguiente evento, o `None` al agotar la fuente.
    fn next(&mut self) -> Result<Option<Event>>;

    /// Watermark tras lo consumido hasta ahora (para el `sync` incremental).
    fn watermark(&self) -> Watermark;

    /// Claves deterministas de ESTA fuente para el manifiesto de la respuesta.
    fn manifest(&self) -> BTreeMap<String, String>;
}

/// Registro explícito de adaptadores (sin `init` mágico: determinista y legible).
/// `init` recorre estos, llama a `detect` y elige el de mayor score.
#[derive(Default)]
pub struct Registry {
    sources: Vec<Box<dyn Source>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, s: Box<dyn Source>) {
        self.sources.push(s);
    }

    /// Elige el adaptador con mayor `detect(path)` > 0, con desempate estable por
    /// orden de registro. `None` si ninguno reconoce la ruta.
    pub fn pick(&self, path: &Path) -> Option<&dyn Source> {
        let mut best: Option<(&dyn Source, i32)> = None;
        for s in &self.sources {
            let score = s.detect(path);
            if score > 0 && best.is_none_or(|(_, b)| score > b) {
                best = Some((s.as_ref(), score));
            }
        }
        best.map(|(s, _)| s)
    }

    /// Busca un adaptador por su `kind` (para forzar con `--as`).
    /// Todos los adaptadores registrados, en orden de registro.
    pub fn all(&self) -> impl Iterator<Item = &dyn Source> {
        self.sources.iter().map(|b| b.as_ref())
    }

    pub fn by_kind(&self, kind: &str) -> Option<&dyn Source> {
        self.sources.iter().map(|s| s.as_ref()).find(|s| s.kind() == kind)
    }
}
