//! `chrono-source-delega` — adaptador Source del directorio de estado de delega
//! (`~/.local/state/delega/` o el `state_dir` del config): una carpeta por
//! trabajo (`<YYYYMMDD-HHMMSS-…>/`) y el registro `audit.jsonl`.
//!
//! - Por trabajo: `delega.job.start`, `delega.job.end` (coste, duración, mensajes,
//!   herramientas, alertas, `rc`, tope, si dejó salida), `delega.job.result` (primera
//!   línea del resultado, redactada), `delega.notice` (avisos del trabajador),
//!   `delega.alert` y `delega.message` (del `progress.log`). Entidad = id del trabajo.
//! - `audit.jsonl`: `delega.audit.<event>` (launch, end, refused, integrate…).
//! - Lee los nombres nuevos y los legados en español (`resumen.json`, `progreso.log`…).
//! - Nunca abre `brief.md`; del texto libre (resultado, avisos, `reason`) solo entra la
//!   primera línea, recortada a 160 caracteres y con los términos de `forbidden.txt`
//!   (y del fichero de la opción `forbidden_terms`) sustituidos por `[redacted]`.
//! - delega escribe hora local sin zona: el offset sale de la opción `utc_offset`
//!   (`-05:00`) o se deduce de los trabajos que guardan las dos formas (resumen frente a
//!   estado). Se guarda en el watermark para que un `sync` no lo cambie a mitad.
//! - Incremental: watermark `mt:<ns>|a:<off>|p:<hash>|tz:<s>`. Se releen solo las
//!   carpetas con algún fichero más nuevo que `mt` (los eventos ya guardados se
//!   descartan por id) y `audit.jsonl` desde `a` (si encoge o cambia su prefijo, diverge).

mod job;
mod simhash;
mod util;

pub use job::{subtype, LAUNCH_RCS};
pub use util::{redact, ts_ms};

use chrono_core::{CoreError, Cursor, Event, Result as CoreResult, Source, SourceConfig, Watermark};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const PREFIX_CAP: u64 = 4096;

/// Adaptador `Source` del directorio de estado de delega.
#[derive(Default)]
pub struct DelegaSource;

impl DelegaSource {
    pub fn new() -> Self {
        Self
    }
}

fn is_job_dir(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.as_bytes().first().is_some_and(u8::is_ascii_digit)) && p.is_dir()
}

/// Carpetas de trabajo, ordenadas por nombre.
fn job_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| is_job_dir(p)).collect()).unwrap_or_default();
    v.sort();
    v
}

fn mtime_ns(p: &Path) -> u128 {
    fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
}

/// El fichero más nuevo de la carpeta (o la carpeta misma).
fn dir_mtime(dir: &Path) -> u128 {
    let own = mtime_ns(dir);
    fs::read_dir(dir).map(|rd| rd.flatten().map(|e| mtime_ns(&e.path())).max().unwrap_or(0)).unwrap_or(0).max(own)
}

fn hash_prefix(path: &Path, limit: u64) -> String {
    let want = limit.min(PREFIX_CAP) as usize;
    let mut buf = vec![0u8; want];
    let mut total = 0;
    if let Ok(mut f) = fs::File::open(path) {
        while total < want {
            match f.read(&mut buf[total..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
    }
    format!("{:016x}", simhash::fnv1a64(&buf[..total]))
}

/// `±hh:mm` / `±hhmm` / `Z` → segundos.
fn parse_offset(s: &str) -> Option<i64> {
    let s = s.trim();
    if s == "Z" || s == "0" {
        return Some(0);
    }
    let sign = match s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let r = s[1..].replace(':', "");
    let hh: i64 = r.get(0..2)?.parse().ok()?;
    let mm: i64 = r.get(2..4).unwrap_or("00").parse().ok()?;
    Some(sign * (hh * 3600 + mm * 60))
}

#[derive(Debug, Default, PartialEq)]
struct Mark {
    mt: u128,
    audit_off: u64,
    audit_hash: Option<String>,
    tz: Option<i64>,
}

fn parse_mark(v: &str) -> Option<Mark> {
    let mut m = Mark::default();
    for part in v.split('|') {
        let (k, val) = part.split_once(':')?;
        match k {
            "mt" => m.mt = val.parse().ok()?,
            "a" => m.audit_off = val.parse().ok()?,
            "p" => m.audit_hash = Some(val.to_string()),
            "tz" => m.tz = Some(val.parse().ok()?),
            _ => {}
        }
    }
    Some(m)
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

/// Offset más repetido entre los trabajos que lo permiten deducir (empate: el menor).
fn observed_offset(dirs: &[PathBuf]) -> Option<i64> {
    let mut votes: BTreeMap<i64, usize> = BTreeMap::new();
    for d in dirs {
        if let Some(o) = job::observed_offset(d) {
            *votes.entry(o).or_default() += 1;
        }
    }
    votes.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map(|(o, _)| *o)
}

impl Source for DelegaSource {
    fn kind(&self) -> &str {
        "delega"
    }

    fn detect(&self, path: &Path) -> i32 {
        if !path.is_dir() {
            return 0;
        }
        if path.join("audit.jsonl").is_file() {
            return 85;
        }
        let looks_like_job = |d: &PathBuf| d.join("meta").is_file() && d.join("brief.md").is_file();
        if job_dirs(path).iter().any(looks_like_job) {
            85
        } else {
            0
        }
    }

    fn open(&self, path: &Path, watermark: Option<Watermark>, cfg: &SourceConfig) -> CoreResult<Box<dyn Cursor>> {
        if !path.is_dir() {
            return Err(CoreError::Other(format!("{} no es un directorio de estado de delega", path.display())));
        }
        let prev = match &watermark {
            None => None,
            Some(wm) => Some(parse_mark(&wm.value).ok_or_else(|| CoreError::Diverged(wm.value.clone()))?),
        };
        let dirs = job_dirs(path);
        let opt_offset = cfg.options.get("utc_offset").and_then(|s| parse_offset(s));
        let (offset_s, offset_from) = match (opt_offset, prev.as_ref().and_then(|p| p.tz)) {
            (Some(o), Some(t)) if o != t => return Err(CoreError::Diverged(format!("utc_offset {o} != {t}"))),
            (Some(o), _) => (o, "option"),
            (None, Some(t)) => (t, "watermark"),
            (None, None) => observed_offset(&dirs).map_or((0, "default-utc"), |o| (o, "observed")),
        };
        let terms = cfg.options.get("forbidden_terms").map(|p| util::load_terms(&expand_home(p))).unwrap_or_default();
        let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let source_id = format!("delega:{}", abs.to_string_lossy());
        let names: Vec<String> = dirs.iter().filter_map(|d| d.file_name().and_then(|n| n.to_str()).map(str::to_string)).collect();
        let ctx = job::Ctx { source_id: &source_id, offset_s, terms: &terms, jobs: &names };

        let mut events: Vec<Event> = Vec::new();
        let mut stats: BTreeMap<String, String> = BTreeMap::new();

        // audit.jsonl desde el offset, solo líneas completas.
        let audit = path.join("audit.jsonl");
        let mut audit_off = prev.as_ref().map_or(0, |p| p.audit_off);
        let (mut audit_lines, mut audit_invalid) = (0u64, 0u64);
        if let Ok(bytes) = fs::read(&audit) {
            if (bytes.len() as u64) < audit_off {
                return Err(CoreError::Diverged(format!("audit.jsonl encogió por debajo de {audit_off}")));
            }
            if let Some(h) = prev.as_ref().and_then(|p| p.audit_hash.as_ref()) {
                if *h != hash_prefix(&audit, audit_off) {
                    return Err(CoreError::Diverged("audit.jsonl reescrito".into()));
                }
            }
            let mut pos = audit_off as usize;
            while let Some(nl) = bytes[pos..].iter().position(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(&bytes[pos..pos + nl]);
                if !line.trim().is_empty() {
                    match serde_json::from_str::<Value>(&line).ok().and_then(|v| v.as_object().cloned()).and_then(|o| job::audit_event(&o, pos as u64, &ctx)) {
                        Some(ev) => {
                            audit_lines += 1;
                            events.push(ev);
                        }
                        None => audit_invalid += 1,
                    }
                }
                pos += nl + 1;
            }
            audit_off = pos as u64;
        } else if audit_off > 0 {
            return Err(CoreError::Diverged("audit.jsonl desapareció".into()));
        }

        // Carpetas de trabajo con algo más nuevo que el watermark.
        let since = prev.as_ref().map_or(0, |p| p.mt);
        let mut mt_max = since;
        let mut scanned = 0usize;
        for d in &dirs {
            let mt = dir_mtime(d);
            mt_max = mt_max.max(mt);
            if prev.is_some() && mt <= since {
                continue;
            }
            scanned += 1;
            let name = d.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            events.extend(job::job_events(d, name, &ctx));
        }
        events.sort_by(|a, b| a.at_epoch.cmp(&b.at_epoch).then_with(|| a.id.cmp(&b.id)));

        let hash = if audit_off > 0 { format!("|p:{}", hash_prefix(&audit, audit_off)) } else { String::new() };
        let next = format!("mt:{mt_max}|a:{audit_off}{hash}|tz:{offset_s}");
        let prev_value = watermark.map_or_else(|| format!("mt:0|a:0|tz:{offset_s}"), |w| w.value);
        stats.insert("format".into(), "delega-state".into());
        stats.insert("delega_jobs".into(), dirs.len().to_string());
        stats.insert("delega_jobs_scanned".into(), scanned.to_string());
        stats.insert("delega_audit_lines".into(), audit_lines.to_string());
        stats.insert("delega_audit_invalid".into(), audit_invalid.to_string());
        stats.insert("utc_offset_s".into(), offset_s.to_string());
        stats.insert("utc_offset_from".into(), offset_from.into());
        Ok(Box::new(DelegaCursor { pending: events.into(), next, prev: prev_value, stats }))
    }
}

/// Cursor sobre los eventos ya leídos (el estado de delega cabe en memoria: cientos de trabajos).
pub struct DelegaCursor {
    pending: VecDeque<Event>,
    next: String,
    prev: String,
    stats: BTreeMap<String, String>,
}

impl Cursor for DelegaCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        Ok(self.pending.pop_front())
    }

    fn watermark(&self) -> Watermark {
        // Cortado a medias (cap de eventos): se vuelve a leer todo; los ids repetidos se ignoran.
        let value = if self.pending.is_empty() { self.next.clone() } else { self.prev.clone() };
        Watermark { kind: "delega".into(), value }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        self.stats.clone()
    }
}

#[cfg(test)]
mod tests;
