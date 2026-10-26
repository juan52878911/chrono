//! Fuentes glob: `chrono add '~/.claude/projects/**/*.jsonl' --as jsonl` es UNA
//! fuente (una fila en `sources`) que agrupa todos los ficheros que casan.
//!
//! Cada fichero lleva su propio watermark, junto con el tamaño y el mtime con
//! que se leyó; `sync` vuelve a expandir el patrón y solo abre los ficheros
//! nuevos o cuyo tamaño o mtime cambió. Ese estado vive en `meta`
//! (`glob_state:<source_id>`) y no en `sources.watermark`: con miles de ficheros
//! serían cientos de KB en una columna que el MCP lee en cada llamada. En
//! `sources.watermark` queda un resumen (`glob:<ficheros>:<hash>`) que cambia
//! cuando cambia el estado.
//!
//! Los eventos llevan el `source_id` de la fuente glob y `attrs.file` = la ruta
//! del fichero relativa a la base del patrón (la parte sin comodines). Si un
//! fichero diverge (truncado o reescrito), se borran y reingieren solo sus
//! eventos. Un fichero que desaparece conserva sus eventos (son historia).
//!
//! Tamaño y mtime no bastan: un fichero reescrito con el mismo tamaño y el mismo
//! mtime (`touch -r`, `rsync -t`, una copia que conserva fechas) no se releía.
//! Cada fichero guarda además una huella barata (primeros y últimos 64 KB) y su
//! ctime, que el usuario no puede fijar: con tamaño y mtime iguales, la huella
//! solo se recalcula si el ctime cambió, así un `sync` sin cambios no lee nada.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono_core::{Source, Watermark};
use chrono_store::Store;
use serde_json::{json, Value};

use super::{fnv1a32, ingest_source, IngestCtx, IngestError, Result};
use crate::glob;
use crate::i18n::t;

/// Cada cuántos ficheros se guarda el estado (si se corta un `sync` largo, lo
/// ya leído no se relee: los ids son deterministas, pero cuesta tiempo).
const SAVE_EVERY: usize = 200;

/// Bytes del principio y del final que entran en la huella de un fichero.
const FINGERPRINT_SPAN: u64 = 64 * 1024;

/// Lo que se sabe de un fichero tras leerlo.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileState {
    size: u64,
    mtime_ns: i128,
    watermark: String,
    /// ctime (0 = desconocido: estado anterior a la huella o sin ctime en el SO).
    ctime_ns: i128,
    /// Huella de los primeros y últimos `FINGERPRINT_SPAN` bytes ("" = sin calcular).
    fingerprint: String,
}

/// mtime y ctime de un fichero en ns (0 si el SO no los da).
fn times_ns(md: &std::fs::Metadata) -> (i128, i128) {
    let mtime = md.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i128);
    #[cfg(unix)]
    let ctime = {
        use std::os::unix::fs::MetadataExt;
        i128::from(md.ctime()) * 1_000_000_000 + i128::from(md.ctime_nsec())
    };
    #[cfg(not(unix))]
    let ctime = 0;
    (mtime, ctime)
}

/// Huella de los primeros y últimos `FINGERPRINT_SPAN` bytes de los `size`
/// primeros de `path` (todo el fichero si es pequeño). Hash de 64 bits por
/// palabras: no es criptográfico, solo distingue un contenido reescrito.
fn fingerprint(path: &Path, size: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity((2 * FINGERPRINT_SPAN).min(size) as usize);
    (&mut f).take(FINGERPRINT_SPAN.min(size)).read_to_end(&mut buf)?;
    let tail_from = size.saturating_sub(FINGERPRINT_SPAN).max(FINGERPRINT_SPAN);
    if tail_from < size {
        f.seek(SeekFrom::Start(tail_from))?;
        f.take(size - tail_from).read_to_end(&mut buf)?;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ size;
    let mut words = buf.chunks_exact(8);
    for w in &mut words {
        h = (h ^ u64::from_le_bytes(w.try_into().unwrap_or_default())).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(29);
    }
    for b in words.remainder() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(format!("{h:016x}"))
}

/// Datos fijos de una fuente glob (en su `manifest_json`).
#[derive(Debug, Clone, Default)]
pub(crate) struct GlobSource {
    pub id: String,
    /// Patrón absoluto (`/Users/x/.claude/projects/**/*.jsonl`).
    pub pattern: String,
    pub alias: Option<String>,
    pub preset: Option<String>,
}

/// Resultado de un paso de ingesta de una fuente glob.
pub(crate) struct GlobSync {
    pub events: usize,
    /// Ficheros abiertos en esta pasada (nuevos o cambiados).
    pub files_read: usize,
    /// Ficheros que casan con el patrón.
    pub files_total: usize,
    /// Algún fichero divergió y se reingirió (FTS y rollups, completos).
    pub diverged: bool,
    /// Resumen para `sources.watermark`.
    pub watermark: String,
}

impl GlobSource {
    /// Lee una fuente glob del `manifest_json` de su fila (`None` si no lo es).
    pub fn from_row(id: &str, manifest_json: &str) -> Option<GlobSource> {
        let v: Value = serde_json::from_str(manifest_json).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(GlobSource { id: id.to_string(), pattern: s("glob")?, alias: s("alias"), preset: s("preset") })
    }

    pub fn manifest(&self, files: usize) -> BTreeMap<String, String> {
        let mut m = BTreeMap::from([("glob".to_string(), self.pattern.clone()), ("files".to_string(), files.to_string())]);
        if let Some(a) = &self.alias {
            m.insert("alias".to_string(), a.clone());
        }
        if let Some(p) = &self.preset {
            m.insert("preset".to_string(), p.clone());
        }
        m
    }

    /// Claves de `source_options` de la fuente: su alias y su patrón.
    pub fn options_keys(&self) -> Vec<String> {
        self.alias.iter().cloned().chain([self.pattern.clone()]).collect()
    }

    /// Directorio base: la parte del patrón antes del primer comodín.
    fn base(&self) -> PathBuf {
        let segs: Vec<&str> = self.pattern.split('/').collect();
        let first = segs.iter().position(|s| glob::is_pattern(s)).unwrap_or(segs.len().saturating_sub(1));
        PathBuf::from(segs[..first].join("/"))
    }

    fn state_key(&self) -> String {
        format!("glob_state:{}", self.id)
    }

    fn load_state(&self, store: &Store) -> Result<BTreeMap<String, FileState>> {
        let Some(raw) = store.meta(&self.state_key())? else { return Ok(BTreeMap::new()) };
        let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let mut out = BTreeMap::new();
        for (file, st) in v.as_object().into_iter().flatten() {
            let (Some(size), Some(mtime), Some(wm)) = (st.get(0).and_then(Value::as_u64), st.get(1).and_then(Value::as_str), st.get(2).and_then(Value::as_str))
            else {
                continue;
            };
            // Ctime y huella van detrás: un estado de antes no los tiene.
            let ctime_ns = st.get(3).and_then(Value::as_str).and_then(|c| c.parse().ok()).unwrap_or(0);
            let fingerprint = st.get(4).and_then(Value::as_str).unwrap_or("").to_string();
            out.insert(file.clone(), FileState { size, mtime_ns: mtime.parse().unwrap_or(0), watermark: wm.to_string(), ctime_ns, fingerprint });
        }
        Ok(out)
    }

    /// Guarda el estado y devuelve su resumen para `sources.watermark`.
    fn save_state(&self, store: &Store, state: &BTreeMap<String, FileState>) -> Result<String> {
        let obj: serde_json::Map<String, Value> =
            state.iter().map(|(f, st)| (f.clone(), json!([st.size, st.mtime_ns.to_string(), st.watermark, st.ctime_ns.to_string(), st.fingerprint]))).collect();
        let raw = Value::Object(obj).to_string();
        store.set_meta(&self.state_key(), &raw)?;
        Ok(format!("glob:{}:{:08x}", state.len(), fnv1a32(raw.as_bytes())))
    }

    /// Olvida el estado (re-ingesta completa en `add`).
    pub fn clear_state(&self, store: &Store) -> Result<()> {
        store.conn().execute("DELETE FROM meta WHERE key = ?1", [self.state_key()])?;
        Ok(())
    }

    /// Expande el patrón e ingiere lo nuevo de cada fichero.
    pub fn sync(&self, adapter: &dyn Source, store: &mut Store, index_root: &Path) -> Result<GlobSync> {
        let files = glob::expand(&self.pattern);
        let base = self.base();
        let mut state = self.load_state(store)?;
        let options_keys = self.options_keys();
        let mut out = GlobSync { events: 0, files_read: 0, files_total: files.len(), diverged: false, watermark: String::new() };
        let mut stderr = std::io::stderr();
        let (files_word, events_word) = (t("files", "ficheros"), t("events", "eventos"));
        for (i, path) in files.iter().enumerate() {
            let Ok(md) = std::fs::metadata(path) else { continue };
            let (mtime_ns, ctime_ns) = times_ns(&md);
            let rel = path.strip_prefix(&base).unwrap_or(path).to_string_lossy().into_owned();
            let mut prev = state.get(&rel).cloned();
            let mut rewritten = false;
            if let Some(p) = prev.as_mut().filter(|p| p.size == md.len() && p.mtime_ns == mtime_ns) {
                if p.ctime_ns == ctime_ns && ctime_ns != 0 {
                    continue;
                }
                // Mismo tamaño y mtime, pero algo tocó el fichero: la huella decide.
                let Ok(fp) = fingerprint(path, md.len()) else { continue };
                if p.fingerprint.is_empty() || p.fingerprint == fp {
                    // Sin huella previa (estado de antes) no hay con qué comparar: se adopta.
                    p.ctime_ns = ctime_ns;
                    p.fingerprint = fp;
                    state.insert(rel, p.clone());
                    continue;
                }
                // Reescrito: se relee entero, como una divergencia.
                rewritten = true;
            }
            let ctx = IngestCtx { options_keys: options_keys.clone(), preset: self.preset.as_deref(), group: Some((&self.id, &rel)) };
            let wm = prev
                .filter(|p| !p.watermark.is_empty() && !rewritten)
                .map(|p| Watermark { kind: adapter.kind().to_string(), value: p.watermark });
            if rewritten {
                store.delete_source_file_events(&self.id, &rel)?;
                out.diverged = true;
            }
            let (n, new_wm) = match ingest_source(adapter, path, wm, store, index_root, &ctx) {
                Ok((n, w, _)) => (n, w),
                Err(IngestError::Diverged) => {
                    store.delete_source_file_events(&self.id, &rel)?;
                    out.diverged = true;
                    let (n, w, _) = ingest_source(adapter, path, None, store, index_root, &ctx).map_err(|e| e.into_boxed())?;
                    (n, w)
                }
                Err(IngestError::Other(e)) => {
                    // Un fichero ilegible no para al resto; se reintenta en el próximo sync.
                    eprintln!("{}", t(format!("  warning: {rel}: {e} — skipped"), format!("  aviso: {rel}: {e} — se omite")));
                    continue;
                }
            };
            let fingerprint = fingerprint(path, md.len()).unwrap_or_default();
            state.insert(rel, FileState { size: md.len(), mtime_ns, watermark: new_wm.value, ctime_ns, fingerprint });
            out.events += n;
            out.files_read += 1;
            if out.files_read.is_multiple_of(SAVE_EVERY) {
                self.save_state(store, &state)?;
            }
            if files.len() >= 50 {
                let _ = write!(stderr, "\r  {}/{} {files_word} · {} {events_word}", i + 1, files.len(), out.events);
                let _ = stderr.flush();
            }
        }
        if files.len() >= 50 {
            let _ = writeln!(stderr);
        }
        out.watermark = self.save_state(store, &state)?;
        Ok(out)
    }
}

/// `~` al principio → `$HOME`; relativo → absoluto respecto al cwd.
pub(crate) fn absolutize(pattern: &str) -> String {
    let expanded = match pattern.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME").map(|h| format!("{h}/{rest}")).unwrap_or_else(|_| pattern.to_string()),
        None => pattern.to_string(),
    };
    if Path::new(&expanded).is_absolute() {
        expanded
    } else {
        std::env::current_dir().map(|d| format!("{}/{expanded}", d.display())).unwrap_or(expanded)
    }
}
