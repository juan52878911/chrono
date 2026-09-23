//! `chrono-source-oslog` — adaptador Source del LOG DEL SISTEMA, adaptable al
//! SO: en macOS via `log show --style json`, en Linux via `journalctl -o
//! json`. No es un fichero: se invoca con un token mágico (`chrono add
//! oslog`). Ver `docs/DESIGN-GENERAL-CORE.md §3` (journald) y
//! `chrono-source-jsonl` como referencia de estilo.
//!
//! Decisiones no obvias:
//! - `open()` es BUFFERED, no streaming: se shellea el comando del SO una
//!   vez, se recolecta TODA su salida en memoria y se parsea de una tirada.
//!   Es aceptable porque el volumen está acotado por la ventana temporal
//!   (`--last`/`--since`), no por el tamaño de un fichero potencialmente
//!   enorme.
//! - `log show --start` (macOS) no admite un epoch UTC directo: hay que
//!   darle "YYYY-MM-DD HH:MM:SS". Esa cadena se construye aquí desde el
//!   epoch UTC del watermark SIN conversión de zona horaria (no hay
//!   dependencias de calendario/TZ en este crate): en un sistema configurado
//!   en UTC es exacto; en otro, el `--start` puede desplazarse por el offset
//!   local. Limitación conocida, documentada en `macos_start_str`.
//! - `journalctl --since` sí admite epoch exacto con el prefijo `@`, así que
//!   en Linux no hay ese problema de zona horaria al reanudar.
//! - El `id` de cada evento es determinista: FNV-1a 64 de (timestamp crudo +
//!   mensaje + índice del registro dentro de la salida de ESTA ejecución),
//!   nunca del reloj de pared. Mismo rango + misma config -> misma salida
//!   del comando -> mismos ids, en el mismo orden (no se reordena).

mod cursor;
mod linux;
mod macos;
mod simhash;
mod timeutil;
mod util;

use chrono_core::{CoreError, Result as CoreResult, Source, SourceConfig, Watermark};
use cursor::OsLogCursor;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Tokens de ruta que activan este adaptador (no hay fichero: es un daemon).
pub const TOKENS: &[&str] = &["oslog", "journald", "syslogd"];

/// Ventana por defecto cuando no hay watermark previo. El volumen de
/// `log show`/`journalctl` es enorme (el encargo cita ~24k registros en 2
/// minutos en macOS): una ventana grande por defecto sería impracticable.
const DEFAULT_WINDOW: &str = "5m";

/// `true` en macOS (usa `log show`); cualquier otro SO objetivo usa
/// `journalctl` (Linux, y por extensión cualquier unix con systemd).
fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

/// Disponibilidad de la herramienta del SO actual. En macOS, `/usr/bin/log`
/// es una ruta fija del sistema (no hace falta ejecutar nada para
/// comprobarlo). En el resto, `journalctl` debe estar en el PATH: se intenta
/// ejecutar `journalctl --version` y solo importa si el proceso arrancó
/// (`.is_ok()`), no su código de salida.
fn tool_available() -> bool {
    if is_macos() {
        Path::new("/usr/bin/log").exists()
    } else {
        Command::new("journalctl").arg("--version").output().is_ok()
    }
}

/// Adaptador `Source` del log del sistema.
pub struct OsLogSource;

impl OsLogSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OsLogSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for OsLogSource {
    fn kind(&self) -> &str {
        "oslog"
    }

    fn detect(&self, path: &Path) -> i32 {
        let Some(s) = path.to_str() else { return 0 };
        if !TOKENS.contains(&s) {
            return 0; // no es uno de los tokens mágicos: no reconocido.
        }
        if tool_available() {
            60
        } else {
            0 // token reconocido pero sin herramienta del SO: no ingerible aquí.
        }
    }

    fn open(&self, path: &Path, watermark: Option<Watermark>, cfg: &SourceConfig) -> CoreResult<Box<dyn chrono_core::Cursor>> {
        let Some(token) = path.to_str() else {
            return Err(CoreError::Other("chrono-source-oslog: ruta no es UTF-8 válida".to_string()));
        };
        if !TOKENS.contains(&token) {
            return Err(CoreError::Unrecognized(format!("chrono-source-oslog: token no reconocido: {token}")));
        }

        // Watermark: "since:<epoch>" (o vacío si nunca hubo eventos previos).
        let since_epoch = watermark.as_ref().and_then(|wm| wm.value.strip_prefix("since:")).and_then(|v| v.parse::<i64>().ok());

        if is_macos() {
            open_macos(since_epoch, cfg)
        } else {
            open_linux(since_epoch, cfg)
        }
    }
}

fn window_from_cfg(cfg: &SourceConfig) -> String {
    cfg.options.get("window").cloned().unwrap_or_else(|| DEFAULT_WINDOW.to_string())
}

/// Convierte un epoch UTC al formato de fecha-hora que acepta `log show
/// --start` ("YYYY-MM-DD HH:MM:SS"). Ver limitación de zona horaria en el
/// comentario de cabecera del módulo.
fn macos_start_str(epoch: i64) -> String {
    let iso = timeutil::epoch_to_iso8601(epoch); // "YYYY-MM-DDTHH:MM:SSZ"
    iso.replace('T', " ").trim_end_matches('Z').to_string()
}

fn open_macos(since_epoch: Option<i64>, cfg: &SourceConfig) -> CoreResult<Box<dyn chrono_core::Cursor>> {
    let mut cmd = Command::new("log");
    cmd.arg("show").arg("--style").arg("json");

    let window_report = match since_epoch {
        Some(epoch) => {
            cmd.arg("--start").arg(macos_start_str(epoch));
            format!("since:{epoch}")
        }
        None => {
            let window = window_from_cfg(cfg);
            cmd.arg("--last").arg(&window);
            window
        }
    };

    let out = cmd.output().map_err(|e| CoreError::Other(format!("el log del sistema requiere permisos / no disponible (log show: {e})")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(CoreError::Other(format!("el log del sistema requiere permisos / no disponible (log show: {stderr})")));
    }

    let json_text = String::from_utf8_lossy(&out.stdout);
    let events = macos::parse_macos(&json_text, "oslog:macos")?;

    let mut manifest = BTreeMap::new();
    manifest.insert("format".to_string(), "oslog".to_string());
    manifest.insert("os".to_string(), "macos".to_string());
    manifest.insert("window".to_string(), window_report);

    Ok(Box::new(OsLogCursor::new(events, manifest)))
}

fn open_linux(since_epoch: Option<i64>, cfg: &SourceConfig) -> CoreResult<Box<dyn chrono_core::Cursor>> {
    let mut cmd = Command::new("journalctl");
    cmd.arg("-o").arg("json");

    let window_report = match since_epoch {
        Some(epoch) => {
            // journalctl sí admite epoch exacto con el prefijo "@" (systemd.time(7)),
            // sin la ambigüedad de zona horaria que tiene `log show --start`.
            cmd.arg("--since").arg(format!("@{epoch}"));
            format!("since:{epoch}")
        }
        None => {
            let window = window_from_cfg(cfg);
            // "-<window>" es una duración relativa systemd.time(7) (p.ej. "-5m"):
            // mismo formato de duración que el `--last` de macOS.
            cmd.arg("--since").arg(format!("-{window}"));
            window
        }
    };

    let out = cmd.output().map_err(|e| CoreError::Other(format!("el log del sistema requiere permisos / no disponible (journalctl: {e})")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(CoreError::Other(format!("el log del sistema requiere permisos / no disponible (journalctl: {stderr})")));
    }

    let jsonl_text = String::from_utf8_lossy(&out.stdout);
    let events = linux::parse_journald(&jsonl_text, "oslog:linux");

    let mut manifest = BTreeMap::new();
    manifest.insert("format".to_string(), "oslog".to_string());
    manifest.insert("os".to_string(), "linux".to_string());
    manifest.insert("window".to_string(), window_report);

    Ok(Box::new(OsLogCursor::new(events, manifest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_da_0_para_ruta_que_no_es_token() {
        let src = OsLogSource::new();
        assert_eq!(src.detect(Path::new("cualquier-fichero.jsonl")), 0);
        assert_eq!(src.detect(Path::new("/var/log/system.log")), 0);
    }

    #[test]
    fn detect_da_60_para_token_valido_si_hay_herramienta_del_so() {
        let src = OsLogSource::new();
        let score = src.detect(Path::new("oslog"));
        // Tolerante al entorno de CI: si la herramienta del SO no está
        // disponible (permisos, contenedor sin journalctl...), detect debe
        // dar 0, no reventar. Si SÍ está, debe dar exactamente 60.
        if tool_available() {
            assert_eq!(score, 60);
        } else {
            assert_eq!(score, 0);
        }
        // Los otros dos tokens mágicos se comportan igual.
        assert_eq!(src.detect(Path::new("journald")), score);
        assert_eq!(src.detect(Path::new("syslogd")), score);
    }

    #[test]
    fn open_con_token_no_reconocido_es_unrecognized() {
        let src = OsLogSource::new();
        let cfg = SourceConfig::default();
        match src.open(Path::new("no-es-un-token"), None, &cfg) {
            Err(CoreError::Unrecognized(_)) => {}
            other => panic!("esperaba CoreError::Unrecognized, obtuve otra cosa (ok={})", other.is_ok()),
        }
    }

    #[test]
    fn macos_start_str_da_formato_esperado() {
        let epoch = crate::timeutil::parse_rfc3339("2026-09-23T04:12:11Z").unwrap();
        assert_eq!(macos_start_str(epoch), "2026-09-23 04:12:11");
    }
}
