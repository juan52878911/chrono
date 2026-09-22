//! Cursor de streaming sobre `git log`: RAM constante (un registro a la vez)
//! en la ruta secuencial, réplica de `ingestRange` en `internal/ingest/gitlog.go`.
//!
//! Para rangos grandes (init completo de un repo con muchos commits), `git
//! log --numstat` es el cuello de botella medido (~98% del tiempo de `init`).
//! Se paraleliza así: se listan los SHAs no-merge del rango con `git
//! rev-list` (rápido), se reparten en N trozos CONTIGUOS y cada trozo se
//! procesa con `git log --no-walk=unsorted --stdin` en su propio hilo. Los
//! resultados se reensamblan en orden de trozo (0, 1, 2, ...) y, dentro de
//! cada trozo, en el orden dado a `--stdin` (verificado: `--no-walk=unsorted`
//! respeta el orden de entrada, no reordena por fecha/topología). El
//! resultado es idéntico, evento a evento, al de la ruta secuencial de un
//! único `git log` — ver el test `paralelo_es_identico_a_secuencial`.
//!
//! Rangos pequeños (el caso típico de `sync` incremental) usan la ruta
//! secuencial de siempre: lanzar N procesos para unos pocos commits añade
//! overhead de spawn sin beneficio.

use crate::gitutil;
use crate::parse::{self, RECORD_SEP};
use crate::simhash;
use crate::timeutil::epoch_to_iso8601;
use chrono_core::{Actor, CoreError, Cursor, Event, Link, Result as CoreResult, Watermark};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

/// Umbral bulk (nº de touches) por encima del cual un evento se excluye del
/// coupling. Igual que `BulkThreshold` en Go.
pub const BULK_THRESHOLD: usize = 50;

/// Por debajo de este nº de commits no-merge en el rango, se usa la ruta
/// secuencial (un solo `git log`): el overhead de lanzar N procesos no
/// compensa. Mismo orden de magnitud que `PROGRESS_EVERY` en `chrono-cli`.
const PARALLEL_THRESHOLD: usize = 2000;

/// Tope de hilos/procesos `git log` concurrentes, aunque haya más CPUs.
const MAX_PARALLEL_THREADS: usize = 8;

/// Formato de `git log`: %x1e separa registros; %x1f separa campos.
/// Incluye `%at` (epoch UNIX) además de `%aI` (ISO con offset del autor).
const LOG_FORMAT: &str = "--pretty=format:%x1e%H%x1f%an%x1f%ae%x1f%at%x1f%aI%x1f%s%x1f%b%x1f";

/// `source_id` derivado de la ruta absoluta del repo, igual en ambas rutas
/// (secuencial y paralela) para que produzcan eventos idénticos.
fn source_id_for(repo: &Path) -> String {
    let abs = std::fs::canonicalize(repo).unwrap_or_else(|_| PathBuf::from(repo));
    format!("git:{}", abs.to_string_lossy())
}

/// Convierte un registro crudo de `git log` (ya sin el separador 0x1e) en un
/// `Event`, o `None` si el registro está corrupto/incompleto (se descarta,
/// como en Go). Única implementación del mapeo numstat/renames/reverts/
/// simhash/tickets: la comparten la ruta secuencial y la paralela.
fn parse_record_to_event(rec: &str, source_id: &str) -> Option<Event> {
    let raw = parse::split_record(rec)?;
    let at_epoch: i64 = raw.at_epoch.trim().parse().ok()?;
    let body = raw.body.trim().to_string();
    let title = raw.subject.to_string();

    let touches = raw.numstat.map(parse::parse_numstat).unwrap_or_default();

    let mut links: Vec<Link> = Vec::new();
    if let Some(l) = parse::find_revert_link(&body) {
        links.push(l);
    }
    let ticket_text = format!("{title}\n{body}");
    links.extend(parse::extract_tickets(&ticket_text));

    let toks = simhash::tokenize(&ticket_text);
    let hash = simhash::simhash(&toks);

    let mut ev = Event {
        id: raw.sha.to_string(),
        source_id: source_id.to_string(),
        kind: "commit".to_string(),
        at: epoch_to_iso8601(at_epoch),
        at_epoch,
        actor: Actor {
            name: raw.author_name.to_string(),
            key: raw.author_email.to_string(),
        },
        title,
        body,
        level: String::new(),
        attrs: BTreeMap::new(),
        touches,
        links,
        simhash: hash,
        is_bulk: false,
    };
    ev.mark_bulk(BULK_THRESHOLD);
    Some(ev)
}

/// Lee registros separados por `RECORD_SEP` de `reader` hasta EOF y los
/// convierte en `Event` (los corruptos se descartan). Usada por los hilos de
/// la ruta paralela; la ruta secuencial sigue leyendo registro a registro
/// desde `Cursor::next` para mantener RAM constante.
fn drain_git_log_stream<R: BufRead>(mut reader: R, source_id: &str) -> std::io::Result<Vec<Event>> {
    let mut events = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(RECORD_SEP, &mut buf)?;
        if n == 0 {
            break;
        }
        if buf.last() == Some(&RECORD_SEP) {
            buf.pop();
        }
        if buf.is_empty() {
            continue;
        }
        let rec = String::from_utf8_lossy(&buf).into_owned();
        if let Some(ev) = parse_record_to_event(&rec, source_id) {
            events.push(ev);
        }
    }
    Ok(events)
}

/// Ejecuta `git log --no-walk=unsorted --numstat ... --stdin` sobre un trozo
/// de SHAs (en el orden dado) y devuelve sus eventos en ese mismo orden.
fn run_log_chunk(repo: &Path, source_id: &str, shas: &[String]) -> CoreResult<Vec<Event>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "log",
            "--no-walk=unsorted",
            "--use-mailmap",
            "--numstat",
            "-M",
            LOG_FORMAT,
            "--stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::Other(format!("no se pudo lanzar git log (paralelo): {e}")))?;

    // Igual que en `gitutil::blob_lines`: el hilo escritor evita el deadlock
    // (git podría bloquearse escribiendo numstat a stdout, con el pipe de
    // stdin lleno, si escribiéramos todos los SHAs antes de leer stdout).
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::Other("git log sin stdin".to_string()))?;
    let shas_owned: Vec<String> = shas.to_vec();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for sha in &shas_owned {
            writeln!(stdin, "{sha}")?;
        }
        Ok(())
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::Other("git log sin stdout".to_string()))?;
    let events = drain_git_log_stream(BufReader::new(stdout), source_id)
        .map_err(|e| CoreError::Other(format!("leyendo git log (paralelo): {e}")))?;

    let mut stderr_txt = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut stderr_txt);
    }

    match writer.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(CoreError::Other(format!("escribiendo SHAs a git log: {e}"))),
        Err(_) => {
            return Err(CoreError::Other(
                "el hilo que escribía SHAs a git log (paralelo) entró en panic".to_string(),
            ))
        }
    }

    let status = child
        .wait()
        .map_err(|e| CoreError::Other(format!("esperando a git log (paralelo): {e}")))?;
    if !status.success() {
        return Err(CoreError::Other(format!(
            "git log (paralelo) terminó con error: {}",
            stderr_txt.trim()
        )));
    }
    Ok(events)
}

/// Reparte `shas` (ya en el orden deseado) en `n_threads` trozos contiguos,
/// procesa cada uno en su propio hilo con `run_log_chunk`, y reensambla los
/// eventos en orden de trozo (determinista, no en orden de finalización).
pub(crate) fn ingest_parallel(
    repo: &Path,
    source_id: &str,
    shas: &[String],
    n_threads: usize,
) -> CoreResult<Vec<Event>> {
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let chunk_count = n_threads.max(1).min(shas.len());
    let chunk_size = shas.len().div_ceil(chunk_count).max(1);

    let mut handles = Vec::with_capacity(chunk_count);
    for chunk in shas.chunks(chunk_size) {
        let repo_owned = repo.to_path_buf();
        let source_id_owned = source_id.to_string();
        let chunk_owned: Vec<String> = chunk.to_vec();
        handles.push(std::thread::spawn(move || {
            run_log_chunk(&repo_owned, &source_id_owned, &chunk_owned)
        }));
    }

    // Reensamblado EN ORDEN DE TROZO (0, 1, 2, ...), no en orden de
    // finalización: eso es lo que garantiza el mismo orden que la ruta
    // secuencial, sea cual sea la velocidad relativa de cada hilo.
    let mut events = Vec::with_capacity(shas.len());
    for h in handles {
        let chunk_events = h.join().map_err(|_| {
            CoreError::Other("un hilo de git log (paralelo) entró en panic".to_string())
        })??;
        events.extend(chunk_events);
    }
    Ok(events)
}

/// Estado de la ruta secuencial (streaming, un registro a la vez).
struct SequentialState {
    child: Child,
    reader: BufReader<ChildStdout>,
    buf: Vec<u8>,
    done: bool,
}

impl SequentialState {
    fn spawn(repo: &Path, range: Option<&str>) -> CoreResult<Self> {
        let mut args: Vec<String> = vec![
            "-C".into(),
            repo.to_string_lossy().into_owned(),
            "log".into(),
            "--no-merges".into(),
            "--use-mailmap".into(),
            "--numstat".into(),
            "-M".into(),
            LOG_FORMAT.into(),
        ];
        if let Some(r) = range {
            args.push(r.to_string());
        }

        let mut child = Command::new("git")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CoreError::Other(format!("no se pudo lanzar git log: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CoreError::Other("git log sin stdout".to_string()))?;

        Ok(Self {
            child,
            reader: BufReader::new(stdout),
            buf: Vec::new(),
            done: false,
        })
    }

    /// Lee el siguiente registro crudo (ya sin el separador 0x1e), o `None`
    /// al agotar el stream. Streaming: solo mantiene un registro en memoria.
    fn read_record(&mut self) -> std::io::Result<Option<String>> {
        loop {
            self.buf.clear();
            let n = self.reader.read_until(RECORD_SEP, &mut self.buf)?;
            if n == 0 {
                return Ok(None);
            }
            if self.buf.last() == Some(&RECORD_SEP) {
                self.buf.pop();
            }
            if self.buf.is_empty() {
                continue; // separador inicial u otro registro vacío.
            }
            return Ok(Some(String::from_utf8_lossy(&self.buf).into_owned()));
        }
    }

    fn next_event(&mut self, source_id: &str) -> CoreResult<Option<Event>> {
        if self.done {
            return Ok(None);
        }
        loop {
            let rec = self
                .read_record()
                .map_err(|e| CoreError::Other(format!("leyendo git log: {e}")))?;
            let Some(rec) = rec else {
                self.finish()?;
                return Ok(None);
            };
            if let Some(ev) = parse_record_to_event(&rec, source_id) {
                return Ok(Some(ev));
            }
            // registro corrupto/incompleto: se descarta, como en Go.
        }
    }

    fn finish(&mut self) -> CoreResult<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        // Drena stderr para no bloquear si el pipe se llenó.
        let mut stderr_txt = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_txt);
        }
        let status = self
            .child
            .wait()
            .map_err(|e| CoreError::Other(format!("esperando a git log: {e}")))?;
        if !status.success() {
            return Err(CoreError::Other(format!(
                "git log terminó con error: {}",
                stderr_txt.trim()
            )));
        }
        Ok(())
    }
}

enum Inner {
    Sequential(SequentialState),
    /// Ruta paralela: ya materializada por completo (los N `git log` deben
    /// terminar para poder reensamblar en orden), se entrega desde aquí.
    Buffered(std::vec::IntoIter<Event>),
}

pub struct GitCursor {
    inner: Inner,
    head: String,
    source_id: String,
}

impl GitCursor {
    /// Arranca la ingesta sobre `repo`. `range` es, por ejemplo,
    /// `Some("<sha>..HEAD")` para una ingesta incremental, o `None` para todo.
    ///
    /// Decide automáticamente la ruta: si el rango tiene al menos
    /// `PARALLEL_THRESHOLD` commits no-merge y hay más de una CPU disponible,
    /// se paraleliza `git log --numstat` (el 98% del tiempo de un `init`
    /// grande); si no, se usa la ruta secuencial de siempre.
    pub fn spawn(repo: &std::path::Path, head: String, range: Option<String>) -> CoreResult<Self> {
        let source_id = source_id_for(repo);

        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(MAX_PARALLEL_THREADS);

        if n_threads > 1 {
            let rev_arg = range.as_deref().unwrap_or("HEAD");
            // Si `rev-list` fallara (rango inválido, etc.), no lo tratamos
            // como error aquí: dejamos que el `git log` de la ruta
            // secuencial, más abajo, sea quien reporte el error real.
            if let Ok(shas) = gitutil::rev_list_no_merges(repo, rev_arg) {
                if shas.len() >= PARALLEL_THRESHOLD {
                    let events = ingest_parallel(repo, &source_id, &shas, n_threads)?;
                    return Ok(Self {
                        inner: Inner::Buffered(events.into_iter()),
                        head,
                        source_id,
                    });
                }
            }
        }

        let seq = SequentialState::spawn(repo, range.as_deref())?;
        Ok(Self {
            inner: Inner::Sequential(seq),
            head,
            source_id,
        })
    }
}

impl Cursor for GitCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
        match &mut self.inner {
            Inner::Sequential(seq) => seq.next_event(&self.source_id),
            Inner::Buffered(iter) => Ok(iter.next()),
        }
    }

    fn watermark(&self) -> Watermark {
        Watermark {
            kind: "git".to_string(),
            value: format!("sha:{}", self.head),
        }
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert(
            "git_version".to_string(),
            gitutil::git_version().unwrap_or_default(),
        );
        m.insert("first_parent".to_string(), "false".to_string());
        m.insert("no_merges".to_string(), "true".to_string());
        m.insert("mailmap_used".to_string(), "true".to_string());
        m.insert("bulk_threshold".to_string(), BULK_THRESHOLD.to_string());
        m
    }
}

#[cfg(test)]
mod parallel_tests {
    use super::*;
    use std::process::Command as Cmd;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "chrono-source-git-test-{tag}-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn run(dir: &Path, args: &[&str]) {
        let out = Cmd::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed in {dir:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn commit(dir: &Path, file: &str, contents: &str, msg: &str) -> String {
        let full = dir.join(file);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, contents).unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-q", "-m", msg]);
        String::from_utf8(
            Cmd::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    /// Repo temporal con ~N commits, incluyendo un rename y un revert, para
    /// el test de determinismo secuencial-vs-paralelo.
    struct DeterminismRepo {
        dir: PathBuf,
    }

    impl DeterminismRepo {
        fn build(n_commits: usize) -> Self {
            let dir = unique_dir("determinism");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            run(&dir, &["init", "-q"]);
            run(&dir, &["config", "user.email", "test@example.com"]);
            run(&dir, &["config", "user.name", "Test"]);
            run(&dir, &["config", "commit.gpgsign", "false"]);

            for i in 0..n_commits {
                let file = format!("src/file{}.txt", i % 7);
                let contents = format!("línea {i}\n");
                commit(&dir, &file, &contents, &format!("commit {i}"));
            }

            // Rename a mitad de camino.
            run(&dir, &["mv", "src/file0.txt", "src/renamed0.txt"]);
            run(&dir, &["commit", "-q", "-m", "rename: file0 -> renamed0"]);

            // Un commit a revertir, y su revert.
            let sha = commit(&dir, "src/file1.txt", "cambio a revertir\n", "cambio temporal");
            run(&dir, &["revert", "--no-edit", &sha]);

            Self { dir }
        }

        fn path(&self) -> &Path {
            &self.dir
        }
    }

    impl Drop for DeterminismRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rev_list_count_and_order(dir: &Path) -> (usize, Vec<String>) {
        let shas = gitutil::rev_list_no_merges(dir, "HEAD").unwrap();
        (shas.len(), shas)
    }

    fn drain_sequential(repo: &Path) -> Vec<Event> {
        let source_id = source_id_for(repo);
        let mut seq = SequentialState::spawn(repo, None).unwrap();
        let mut out = Vec::new();
        while let Some(ev) = seq.next_event(&source_id).unwrap() {
            out.push(ev);
        }
        out
    }

    #[test]
    fn paralelo_es_identico_a_secuencial() {
        // ~90 commits no-merge + 1 rename + 1 cambio + 1 revert.
        let repo = DeterminismRepo::build(90);
        let (count, rev_list_order) = rev_list_count_and_order(repo.path());

        let secuencial = drain_sequential(repo.path());

        let source_id = source_id_for(repo.path());
        // Fuerza varios trozos aunque la máquina de CI tenga pocos núcleos:
        // con `count` bajo, N=8 hilos ya reparte en trozos pequeños.
        let paralelo = ingest_parallel(repo.path(), &source_id, &rev_list_order, 8).unwrap();

        assert_eq!(secuencial.len(), count, "nº de eventos != rev-list --count");
        assert_eq!(paralelo.len(), count, "nº de eventos (paralelo) != rev-list --count");

        // Mismo orden que `git rev-list` (más reciente primero) en ambas rutas.
        let ids_seq: Vec<&str> = secuencial.iter().map(|e| e.id.as_str()).collect();
        let ids_par: Vec<&str> = paralelo.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids_seq, rev_list_order);
        assert_eq!(ids_par, rev_list_order);

        // Identidad evento a evento: mismo id/at_epoch/touches/simhash/links/...
        assert_eq!(secuencial, paralelo);

        // Verifica que el caso de interés (rename, revert) sí se ejerció.
        assert!(secuencial.iter().any(|e| e
            .touches
            .iter()
            .any(|t| t.attrs.get("change_type").map(String::as_str) == Some("R"))));
        assert!(secuencial
            .iter()
            .any(|e| e.links.iter().any(|l| l.rel == "reverts")));
    }

    #[test]
    fn paralelo_con_un_solo_hilo_tambien_es_identico() {
        let repo = DeterminismRepo::build(60);
        let (_, rev_list_order) = rev_list_count_and_order(repo.path());
        let secuencial = drain_sequential(repo.path());
        let source_id = source_id_for(repo.path());
        let paralelo = ingest_parallel(repo.path(), &source_id, &rev_list_order, 1).unwrap();
        assert_eq!(secuencial, paralelo);
    }
}
