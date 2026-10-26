//! `chrono-source-dir` — inventario de una carpeta: un `Event` por fichero.
//!
//! Cada evento lleva la ruta relativa como título, la carpeta que lo contiene
//! como entidad (`folder`) y en `attrs` el tipo (por extensión y número mágico,
//! ver `magic`), el tamaño, las fechas y una huella de cambio. No lee el
//! contenido: como mucho los primeros `magic::HEAD_BYTES` de los ficheros
//! nuevos o cambiados.
//!
//! Incremental por tamaño + mtime: el id del evento es un hash de
//! (ruta, tamaño, mtime), así que un fichero sin cambios vuelve a dar el mismo
//! id y no entra dos veces, y uno modificado da una versión nueva. El watermark
//! es el instante del último recorrido (`scan:<ms>`): en el siguiente solo se
//! abren los ficheros con mtime o ctime posteriores (el ctime cambia también al
//! copiar o mover un fichero a la carpeta, aunque conserve su mtime). Un
//! fichero borrado no genera evento: sus versiones anteriores siguen en el índice.
//!
//! Opciones: `hidden` (`true` incluye ficheros y carpetas que empiezan por `.`,
//! que por defecto se saltan) y `exclude` (nombres o rutas relativas, con `*`,
//! separados por comas). No sigue enlaces simbólicos.

mod magic;

use chrono_core::{Actor, CoreError, Cursor, Event, Result as CoreResult, Source, SourceConfig, Touch, Watermark};
use std::collections::{BTreeMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Margen al comparar con el último recorrido: un fichero escrito durante el
/// recorrido anterior (o con un reloj algo desfasado) se vuelve a mirar.
const SLACK_MS: i64 = 2000;

pub struct DirSource;

impl DirSource {
    pub fn new() -> Self {
        DirSource
    }
}

impl Default for DirSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for DirSource {
    fn kind(&self) -> &str {
        "dir"
    }

    /// Cualquier carpeta, con la confianza más baja: un repo git (o lo que
    /// otro adaptador reconozca) gana; `--as dir` la fuerza.
    fn detect(&self, path: &Path) -> i32 {
        i32::from(path.is_dir())
    }

    fn options(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("hidden", "\"true\": incluye ficheros y carpetas que empiezan por ."),
            ("exclude", "nombres o rutas relativas (con *) a saltar, separados por comas"),
        ]
    }

    fn open(&self, path: &Path, watermark: Option<Watermark>, cfg: &SourceConfig) -> CoreResult<Box<dyn Cursor>> {
        if !path.is_dir() {
            return Err(CoreError::Other(format!("{} no es una carpeta", path.display())));
        }
        let since_ms = match watermark.as_ref().map(|w| w.value.as_str()) {
            None | Some("") => None,
            Some(v) => Some(
                v.strip_prefix("scan:")
                    .and_then(|n| n.parse::<i64>().ok())
                    .ok_or_else(|| CoreError::Diverged(v.to_string()))?,
            ),
        };
        let opts = Walk {
            hidden: cfg.options.get("hidden").is_some_and(|v| v == "true"),
            exclude: cfg.options.get("exclude").map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default(),
        };
        let root = path.to_path_buf();
        let mut pending = VecDeque::new();
        pending.push_back(root.clone());
        Ok(Box::new(DirCursor {
            source_id: format!("dir:{}", root.display()),
            root,
            opts,
            since_ms,
            scan_start_ms: now_ms(),
            dirs: pending,
            files: VecDeque::new(),
            seen: 0,
            emitted: 0,
        }))
    }
}

struct Walk {
    hidden: bool,
    exclude: Vec<String>,
}

impl Walk {
    fn skip(&self, name: &str, rel: &str) -> bool {
        (!self.hidden && name.starts_with('.')) || self.exclude.iter().any(|g| wild(g, name) || wild(g, rel))
    }
}

/// `*` casa con cualquier tramo (también `/`); el resto, literal.
fn wild(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == b'*')
}

struct DirCursor {
    source_id: String,
    root: PathBuf,
    opts: Walk,
    /// Último recorrido (ms); `None` = primera vez, se emite todo.
    since_ms: Option<i64>,
    scan_start_ms: i64,
    /// Carpetas por recorrer y ficheros de la carpeta en curso, en orden de nombre.
    dirs: VecDeque<PathBuf>,
    files: VecDeque<(PathBuf, std::fs::Metadata)>,
    seen: u64,
    emitted: u64,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn ms_of(t: std::io::Result<SystemTime>) -> Option<i64> {
    let t = t.ok()?;
    Some(match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    })
}

#[cfg(unix)]
fn ctime_ms(md: &std::fs::Metadata) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    Some(md.ctime() * 1000 + md.ctime_nsec() / 1_000_000)
}

#[cfg(not(unix))]
fn ctime_ms(_md: &std::fs::Metadata) -> Option<i64> {
    None
}

/// FNV-1a de 64 bits.
fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in *p {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
        h = (h ^ 0xff).wrapping_mul(0x0100_0000_01b3); // separador entre partes
    }
    h
}

/// Epoch en ms → ISO-8601 UTC (`2026-10-26T10:00:00.000Z`).
fn iso(ms: i64) -> String {
    let (days, rem) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Días desde 1970-01-01 → fecha civil (algoritmo de Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, mi, s, ms) = (rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

impl DirCursor {
    /// Llena `files` con la siguiente carpeta que tenga ficheros; `false` al acabar.
    fn refill(&mut self) -> bool {
        while self.files.is_empty() {
            let Some(dir) = self.dirs.pop_front() else { return false };
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut entries: Vec<(String, PathBuf)> =
                rd.filter_map(|e| e.ok()).map(|e| (e.file_name().to_string_lossy().into_owned(), e.path())).collect();
            entries.sort();
            let mut subdirs = Vec::new();
            for (name, path) in entries {
                let rel = self.rel(&path);
                if self.opts.skip(&name, &rel) {
                    continue;
                }
                // symlink_metadata: un enlace no se sigue (ni carpetas en bucle
                // ni ficheros de fuera de la carpeta).
                let Ok(md) = std::fs::symlink_metadata(&path) else { continue };
                if md.is_dir() {
                    subdirs.push(path);
                } else if md.is_file() {
                    self.files.push_back((path, md));
                }
            }
            // Primero los ficheros de la carpeta y luego sus subcarpetas, en
            // profundidad y en orden de nombre: determinista.
            for d in subdirs.into_iter().rev() {
                self.dirs.push_front(d);
            }
        }
        true
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().into_owned()
    }

    fn event(&self, path: &Path, md: &std::fs::Metadata) -> Event {
        let rel = self.rel(path);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let folder = Path::new(&rel).parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty()).unwrap_or_else(|| ".".into());
        let size = md.len();
        let mtime = ms_of(md.modified()).unwrap_or(0);
        let mut head = Vec::with_capacity(magic::HEAD_BYTES);
        if let Ok(f) = std::fs::File::open(path) {
            let _ = f.take(magic::HEAD_BYTES as u64).read_to_end(&mut head);
        }
        let by_ext = magic::category_of_ext(&ext);
        let sniffed = magic::sniff(&head, &ext);
        let (format, category) = match (sniffed, by_ext) {
            (Some((f, c)), _) => (f.to_string(), c),
            (None, Some(c)) => (ext.clone(), c),
            (None, None) if size == 0 => ("empty".to_string(), "other"),
            (None, None) => (if ext.is_empty() { "unknown".into() } else { ext.clone() }, "other"),
        };
        let hash = fnv1a64(&[&size.to_le_bytes(), &mtime.to_le_bytes()]);
        let mut attrs = BTreeMap::from([
            ("path".to_string(), rel.clone()),
            ("name".to_string(), name),
            ("ext".to_string(), ext),
            ("type".to_string(), category.to_string()),
            ("format".to_string(), format),
            ("size".to_string(), size.to_string()),
            ("modified".to_string(), iso(mtime)),
            ("hash".to_string(), format!("{hash:016x}")),
        ]);
        if let Some(created) = ms_of(md.created()) {
            attrs.insert("created".to_string(), iso(created));
        }
        // La extensión promete una familia y la cabecera dice otra (un .pdf que es HTML).
        if let (Some((f, c)), Some(e)) = (sniffed, by_ext) {
            let text_in_binary = f == "text" && !magic::TEXT_EXTS.contains(&attrs["ext"].as_str());
            if text_in_binary || (c != e && f != "text") {
                attrs.insert("ext_mismatch".to_string(), "true".to_string());
            }
        }
        Event {
            id: format!("{:016x}", fnv1a64(&[rel.as_bytes(), &size.to_le_bytes(), &mtime.to_le_bytes()])),
            source_id: self.source_id.clone(),
            kind: "file".to_string(),
            at: iso(mtime),
            at_epoch: mtime,
            actor: Actor::default(),
            title: rel,
            body: String::new(),
            level: String::new(),
            attrs,
            touches: vec![Touch { entity: folder, entity_type: "folder".to_string(), weight: 1, attrs: BTreeMap::new() }],
            links: Vec::new(),
            simhash: 0,
            is_bulk: false,
        }
    }
}

impl Cursor for DirCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        loop {
            if self.files.is_empty() && !self.refill() {
                return Ok(None);
            }
            let Some((path, md)) = self.files.pop_front() else { continue };
            self.seen += 1;
            if let Some(since) = self.since_ms {
                let changed = [ms_of(md.modified()), ctime_ms(&md)].into_iter().flatten().max().unwrap_or(i64::MAX);
                if changed < since - SLACK_MS {
                    continue;
                }
            }
            self.emitted += 1;
            return Ok(Some(self.event(&path, &md)));
        }
    }

    fn watermark(&self) -> Watermark {
        Watermark { kind: "dir".to_string(), value: format!("scan:{}", self.scan_start_ms) }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        BTreeMap::from([("files_seen".to_string(), self.seen.to_string()), ("files_emitted".to_string(), self.emitted.to_string())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("chrono-dir-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn drain(c: &mut Box<dyn Cursor>) -> Vec<Event> {
        std::iter::from_fn(|| c.next().unwrap()).collect()
    }

    #[test]
    fn un_evento_por_fichero_con_tipo_y_carpeta() {
        let d = tmp("inventario");
        std::fs::create_dir_all(d.join("fotos/2024")).unwrap();
        std::fs::create_dir_all(d.join(".oculta")).unwrap();
        std::fs::write(d.join("fotos/2024/a.png"), b"\x89PNG\r\n\x1a\nresto").unwrap();
        std::fs::write(d.join("informe.pdf"), b"<html>no soy un pdf</html>").unwrap();
        std::fs::write(d.join("notas.md"), "# hola").unwrap();
        std::fs::write(d.join(".oculta/x.txt"), "x").unwrap();
        std::fs::write(d.join("vacio.dat"), "").unwrap();
        let src = DirSource::new();
        let mut c = src.open(&d, None, &SourceConfig::default()).unwrap();
        let evs = drain(&mut c);
        let got: Vec<(&str, &str, &str)> =
            evs.iter().map(|e| (e.title.as_str(), e.touches[0].entity.as_str(), e.attrs["type"].as_str())).collect();
        assert_eq!(
            got,
            [
                ("informe.pdf", ".", "document"),
                ("notas.md", ".", "document"),
                ("vacio.dat", ".", "other"),
                ("fotos/2024/a.png", "fotos/2024", "image"),
            ]
        );
        assert_eq!(evs[3].attrs["format"], "png");
        assert_eq!(evs[3].attrs["size"], "13");
        // Un .pdf que por dentro es texto: formato del contenido, y se marca.
        assert_eq!((evs[0].attrs["format"].as_str(), evs[0].attrs.get("ext_mismatch").map(String::as_str)), ("text", Some("true")));
        assert!(!evs[1].attrs.contains_key("ext_mismatch"));
        assert_eq!(evs[2].attrs["format"], "empty");
        assert!(evs.iter().all(|e| e.kind == "file" && e.at_epoch > 0 && e.at.ends_with('Z')));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn la_extension_que_miente_se_marca() {
        let d = tmp("mentira");
        std::fs::write(d.join("foto.jpg"), b"%PDF-1.4 ...").unwrap();
        let src = DirSource::new();
        let evs = drain(&mut src.open(&d, None, &SourceConfig::default()).unwrap());
        assert_eq!((evs[0].attrs["format"].as_str(), evs[0].attrs.get("ext_mismatch").map(String::as_str)), ("pdf", Some("true")));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn incremental_por_mtime_y_ids_estables() {
        let d = tmp("incremental");
        std::fs::write(d.join("a.txt"), "uno").unwrap();
        let src = DirSource::new();
        let mut c = src.open(&d, None, &SourceConfig::default()).unwrap();
        let first = drain(&mut c);
        let wm = c.watermark();
        // Mismo contenido, mismo id: recorrer de nuevo desde cero no duplica.
        let again = drain(&mut src.open(&d, None, &SourceConfig::default()).unwrap());
        assert_eq!(first[0].id, again[0].id);
        // Con el watermark del recorrido (y el margen ya pasado), nada que leer.
        let later = Watermark { kind: "dir".into(), value: format!("scan:{}", now_ms() + 10 * SLACK_MS) };
        assert!(drain(&mut src.open(&d, Some(later), &SourceConfig::default()).unwrap()).is_empty());
        // Un fichero nuevo tras el recorrido sí entra.
        std::fs::write(d.join("b.txt"), "dos").unwrap();
        let evs = drain(&mut src.open(&d, Some(wm), &SourceConfig::default()).unwrap());
        assert!(evs.iter().any(|e| e.title == "b.txt"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn opciones_hidden_y_exclude() {
        let d = tmp("opciones");
        std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        std::fs::write(d.join("node_modules/x/i.js"), "1").unwrap();
        std::fs::write(d.join(".env"), "A=1").unwrap();
        std::fs::write(d.join("a.log"), "l").unwrap();
        std::fs::write(d.join("b.tmp"), "t").unwrap();
        let mut options = BTreeMap::new();
        options.insert("hidden".to_string(), "true".to_string());
        options.insert("exclude".to_string(), "node_modules, *.tmp".to_string());
        let evs = drain(&mut DirSource::new().open(&d, None, &SourceConfig { options }).unwrap());
        let titles: Vec<&str> = evs.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, [".env", "a.log"]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn watermark_ajeno_diverge_y_fecha_iso() {
        let d = tmp("wm");
        let bad = Watermark { kind: "dir".into(), value: "off:12".into() };
        assert!(matches!(DirSource::new().open(&d, Some(bad), &SourceConfig::default()), Err(CoreError::Diverged(_))));
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_761_472_800_123), "2025-10-26T10:00:00.123Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00.000Z");
        std::fs::remove_dir_all(&d).ok();
    }
}
