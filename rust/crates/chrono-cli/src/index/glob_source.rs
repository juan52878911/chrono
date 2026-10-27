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
//! mtime (`touch -r`, `rsync -t`, una copia que conserva fechas) no se releía,
//! ni uno editado en medio y crecido (solo entraba lo añadido). Cada fichero
//! guarda además su ctime, que el usuario no puede fijar, y una huella COMPLETA
//! de lo ya leído (`[0, tamaño)`), sea cual sea el tamaño. Si tamaño, mtime y
//! ctime siguen igual, no se lee nada (un `sync` sin cambios no abre ningún
//! fichero). Si algo cambió, se lee el fichero una vez de principio a fin: la
//! huella de lo ya leído sale por el camino y la de lo que queda leído, al
//! final (el hash va en streaming: la segunda continúa la primera). Distinta,
//! el fichero se relee entero (se borran sus eventos y se reingiere); igual, con
//! más tamaño es un append (se ingiere desde el watermark) y con el mismo, nada
//! (un `chmod` o un `touch` no releen eventos). El coste es leer de disco los
//! ficheros tocados, no reingerirlos: unos 0,1 s por un jsonl de 200 MB.
//!
//! Hasta 0.7.0, por encima de 64 MB la huella era una muestra (primeros y
//! últimos 64 KB y 16 bloques por el medio) y una edición entre bloques no se
//! veía. Un estado con esa huella (`v3:`) se compara una última vez con la
//! muestra y pasa a la completa.

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

/// Prefijo de la huella completa (el contenido entero de `[0, tamaño)`). Una
/// sin prefijo es de antes (solo primeros/últimos 64 KB) y no se puede
/// comparar.
const FP_VERSION: &str = "v2:";

/// Huella de muestra de los estados de hasta 0.7.0 para ficheros de más de
/// `LEGACY_FULL_MAX`: solo sirve para comparar una vez y migrar.
const FP_SAMPLED: &str = "v3:";
const LEGACY_FULL_MAX: u64 = 64 * 1024 * 1024;
const LEGACY_SPAN: u64 = 64 * 1024;
const LEGACY_SAMPLES: u64 = 16;

/// Lo que se sabe de un fichero tras leerlo.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileState {
    size: u64,
    mtime_ns: i128,
    watermark: String,
    /// ctime (0 = desconocido: estado anterior a la huella o sin ctime en el SO).
    ctime_ns: i128,
    /// Huella completa de `[0, size)` ("" = sin calcular).
    fingerprint: String,
    /// Sus ids de hash llevan la sal de la fuente y el fichero; `false` = se
    /// empezó a leer antes (solo el fichero) y sigue así hasta releerse entero.
    ids_v2: bool,
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

/// Hash de 64 bits por palabras: no es criptográfico, solo distingue un
/// contenido cambiado. Acepta trozos de cualquier tamaño.
struct Hasher {
    h: u64,
    carry: Vec<u8>,
}

impl Hasher {
    fn new() -> Hasher {
        Hasher { h: 0xcbf2_9ce4_8422_2325, carry: Vec::with_capacity(8) }
    }

    fn word(&mut self, w: [u8; 8]) {
        self.h = (self.h ^ u64::from_le_bytes(w)).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(29);
    }

    fn update(&mut self, mut bytes: &[u8]) {
        if !self.carry.is_empty() {
            let take = (8 - self.carry.len()).min(bytes.len());
            self.carry.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.carry.len() < 8 {
                return;
            }
            let w: [u8; 8] = self.carry[..].try_into().unwrap_or_default();
            self.carry.clear();
            self.word(w);
        }
        let mut words = bytes.chunks_exact(8);
        for w in &mut words {
            self.word(w.try_into().unwrap_or_default());
        }
        self.carry.extend_from_slice(words.remainder());
    }

    /// La huella de lo visto (con el tamaño del fichero), sin consumir el estado.
    fn finish(&self, size: u64) -> String {
        let mut h = self.h ^ size;
        for b in &self.carry {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{FP_VERSION}{h:016x}")
    }
}

/// Huellas completas de los `sizes` primeros bytes de `path` (crecientes), en
/// una sola lectura: cada prefijo continúa el hash del anterior.
fn fingerprints(path: &Path, sizes: &[u64]) -> std::io::Result<Vec<String>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut out = Vec::with_capacity(sizes.len());
    let mut full = Hasher::new();
    let mut pos = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    for &size in sizes {
        while pos < size {
            let want = (size - pos).min(buf.len() as u64) as usize;
            f.read_exact(&mut buf[..want])?;
            full.update(&buf[..want]);
            pos += want as u64;
        }
        out.push(full.finish(size));
    }
    Ok(out)
}

/// Huella de muestra de los estados de hasta 0.7.0 (`v3:`, solo para ficheros
/// de más de 64 MB): primer y último bloque de 64 KB y 16 entre medias.
fn legacy_sampled_fingerprint(path: &Path, size: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let mut h = Hasher::new();
    let mut part = Vec::new();
    let last = size.saturating_sub(LEGACY_SPAN);
    let offsets = std::iter::once(0).chain((1..=LEGACY_SAMPLES).map(|k| last / (LEGACY_SAMPLES + 1) * k)).chain(std::iter::once(last));
    for off in offsets {
        part.clear();
        f.seek(SeekFrom::Start(off))?;
        (&mut f).take(LEGACY_SPAN).read_to_end(&mut part)?;
        h.update(&part);
    }
    Ok(h.finish(size).replacen(FP_VERSION, FP_SAMPLED, 1))
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
    /// De `events`, los que sustituyen a eventos borrados de ficheros que se
    /// releyeron enteros (no son nuevos).
    pub reread: usize,
    /// Ficheros ya indexados que no se pudieron leer (`fichero: error`).
    pub errors: Vec<String>,
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
            // La versión de los ids, también detrás (sin ella, los de antes).
            let ids_v2 = st.get(5).and_then(Value::as_u64) == Some(2);
            out.insert(file.clone(), FileState { size, mtime_ns: mtime.parse().unwrap_or(0), watermark: wm.to_string(), ctime_ns, fingerprint, ids_v2 });
        }
        Ok(out)
    }

    /// Guarda el estado y devuelve su resumen para `sources.watermark`.
    fn save_state(&self, store: &Store, state: &BTreeMap<String, FileState>) -> Result<String> {
        let row = |st: &FileState| {
            let mut v = json!([st.size, st.mtime_ns.to_string(), st.watermark, st.ctime_ns.to_string(), st.fingerprint]);
            if st.ids_v2 {
                v.as_array_mut().expect("es un array").push(json!(2));
            }
            v
        };
        let obj: serde_json::Map<String, Value> = state.iter().map(|(f, st)| (f.clone(), row(st))).collect();
        let raw = Value::Object(obj).to_string();
        store.set_meta(&self.state_key(), &raw)?;
        Ok(format!("glob:{}:{:08x}", state.len(), fnv1a32(raw.as_bytes())))
    }

    /// Eventos de la fuente que una relectura entera no recuperaría: los de
    /// ficheros que ya no casan con el patrón o no se pueden abrir, y los que no
    /// dicen de qué fichero son. Antes solo se miraban los ficheros del estado:
    /// los demás no contaban y la fuente se reindexaba borrándolos.
    pub fn events_not_rereadable(&self, store: &Store) -> Result<usize> {
        let base = self.base();
        let present: std::collections::HashSet<String> = glob::expand(&self.pattern)
            .into_iter()
            .filter(|p| std::fs::File::open(p).is_ok())
            .map(|p| p.strip_prefix(&base).unwrap_or(&p).to_string_lossy().into_owned())
            .collect();
        Ok(store
            .source_file_counts(&self.id)?
            .into_iter()
            .filter(|(file, _)| file.as_ref().is_none_or(|f| !present.contains(f)))
            .map(|(_, n)| n)
            .sum())
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
        let mut out = GlobSync { events: 0, reread: 0, errors: Vec::new(), files_read: 0, files_total: files.len(), diverged: false, watermark: String::new() };
        let mut stderr = std::io::stderr();
        let (files_word, events_word) = (t("files", "ficheros"), t("events", "eventos"));
        for (i, path) in files.iter().enumerate() {
            let Ok(md) = std::fs::metadata(path) else { continue };
            let (mtime_ns, ctime_ns) = times_ns(&md);
            let rel = path.strip_prefix(&base).unwrap_or(path).to_string_lossy().into_owned();
            let prev = state.get(&rel).cloned();
            let len = md.len();
            let mut rewritten = false;
            // Huella de lo que va a quedar leído (`[0, len)`), de paso con la de lo ya leído.
            let mut fp_new = None;
            if let Some(p) = &prev {
                let untouched = p.size == len && p.mtime_ns == mtime_ns && p.ctime_ns == ctime_ns && ctime_ns != 0;
                if untouched {
                    continue;
                }
                // Más corto: truncado o rotado, lo resuelve el adaptador (divergencia).
                if len >= p.size {
                    let fps = match fingerprints(path, &[p.size, len]) {
                        Ok(fps) => fps,
                        Err(e) => {
                            // Antes se callaba: el fichero dejaba de seguirse sin que nadie lo supiera.
                            eprintln!("{}", t(
                                format!("  warning: {rel}: {e} — its indexed events are kept"),
                                format!("  aviso: {rel}: {e} — se conservan sus eventos indexados"),
                            ));
                            out.errors.push(format!("{rel}: {e}"));
                            continue;
                        }
                    };
                    let (fp_old, fp_len) = (&fps[0], &fps[1]);
                    if p.fingerprint.starts_with(FP_VERSION) {
                        // Lo ya leído cambió: no es un append, se relee entero.
                        rewritten = fp_old != &p.fingerprint;
                    } else if p.fingerprint.starts_with(FP_SAMPLED) && p.size > LEGACY_FULL_MAX {
                        // Estado de antes con muestra: se compara con ella esta
                        // vez; desde aquí queda la huella completa.
                        rewritten = legacy_sampled_fingerprint(path, p.size).map_or(true, |fp| fp != p.fingerprint);
                    } else if p.size == len && p.mtime_ns != mtime_ns {
                        // Huella de antes (o ninguna), mismo tamaño y otro mtime:
                        // no hay con qué comparar y un append no es; se relee.
                        rewritten = true;
                    }
                    if !rewritten && p.size == len {
                        // Tocado sin cambiar (o sin huella con que comparar y mismo mtime): se adopta.
                        state.insert(rel, FileState { mtime_ns, ctime_ns, fingerprint: fp_len.clone(), ..p.clone() });
                        continue;
                    }
                    fp_new = Some(fp_len.clone());
                }
            }
            // Lo que no se relee entero sigue con los ids con que se empezó.
            let legacy_ids = prev.as_ref().is_some_and(|p| !p.ids_v2) && !rewritten;
            let mut ctx = IngestCtx { options_keys: options_keys.clone(), preset: self.preset.as_deref(), group: Some((&self.id, &rel)), legacy_ids };
            let wm = prev
                .filter(|p| !p.watermark.is_empty() && !rewritten)
                .map(|p| Watermark { kind: adapter.kind().to_string(), value: p.watermark });
            let mut deleted = 0;
            if rewritten {
                deleted = store.count_source_events(&self.id, Some(&rel))?;
                store.delete_source_file_events(&self.id, &rel)?;
                out.diverged = true;
            }
            let (n, new_wm) = match ingest_source(adapter, path, wm, store, index_root, &ctx) {
                Ok((n, w, _)) => (n, w),
                Err(IngestError::Diverged) => {
                    deleted = store.count_source_events(&self.id, Some(&rel))?;
                    store.delete_source_file_events(&self.id, &rel)?;
                    out.diverged = true;
                    ctx.legacy_ids = false;
                    let (n, w, _) = ingest_source(adapter, path, None, store, index_root, &ctx).map_err(|e| e.into_boxed())?;
                    (n, w)
                }
                Err(IngestError::Other(e)) => {
                    // Un fichero ilegible no para al resto; se reintenta en el próximo sync.
                    eprintln!("{}", t(format!("  warning: {rel}: {e} — skipped"), format!("  aviso: {rel}: {e} — se omite")));
                    out.errors.push(format!("{rel}: {e}"));
                    continue;
                }
            };
            let fingerprint = match fp_new {
                Some(fp) => fp,
                None => fingerprints(path, &[len]).ok().and_then(|v| v.into_iter().next()).unwrap_or_default(),
            };
            let ids_v2 = !ctx.legacy_ids;
            state.insert(rel, FileState { size: len, mtime_ns, watermark: new_wm.value, ctime_ns, fingerprint, ids_v2 });
            out.events += n;
            out.reread += n.min(deleted);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_huella_no_depende_de_como_se_parta_la_lectura() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut whole = Hasher::new();
        whole.update(&data);
        for split in [1, 3, 7, 8, 9, 500, 999] {
            let mut h = Hasher::new();
            for c in data.chunks(split) {
                h.update(c);
            }
            assert_eq!(h.finish(1000), whole.finish(1000), "trozos de {split}");
        }
        let dir = std::env::temp_dir().join(format!("chrono-huella-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f");
        std::fs::write(&p, &data).unwrap();
        let fps = fingerprints(&p, &[100, 1000]).unwrap();
        let mut h = Hasher::new();
        h.update(&data[..100]);
        assert_eq!(fps, [h.finish(100), whole.finish(1000)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn la_huella_de_un_fichero_grande_lo_cubre_entero() {
        use std::io::{Seek, SeekFrom, Write};
        let dir = std::env::temp_dir().join(format!("chrono-huella-grande-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f");
        let size = LEGACY_FULL_MAX + 3 * LEGACY_SPAN;
        std::fs::File::create(&p).unwrap().set_len(size).unwrap();
        let fp = || fingerprints(&p, &[size]).unwrap().remove(0);
        let poke = |off: u64| {
            let mut f = std::fs::File::options().write(true).open(&p).unwrap();
            f.seek(SeekFrom::Start(off)).unwrap();
            f.write_all(b"z").unwrap();
        };
        let a = fp();
        assert!(a.starts_with(FP_VERSION), "{a}");
        let legacy = legacy_sampled_fingerprint(&p, size).unwrap();
        assert!(legacy.starts_with(FP_SAMPLED), "{legacy}");
        // Entre dos bloques de la muestra de antes: la muestra no lo ve, la
        // huella completa sí.
        let block = (size - LEGACY_SPAN) / (LEGACY_SAMPLES + 1) * 8;
        poke(block + LEGACY_SPAN + 100);
        assert_eq!(legacy_sampled_fingerprint(&p, size).unwrap(), legacy);
        assert_ne!(fp(), a);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
