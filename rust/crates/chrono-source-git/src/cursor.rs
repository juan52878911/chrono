//! Cursor de streaming sobre `git log`: RAM constante (un registro a la vez),
//! réplica de `ingestRange` en `internal/ingest/gitlog.go`.

use crate::gitutil;
use crate::parse::{self, RECORD_SEP};
use crate::simhash;
use crate::timeutil::epoch_to_iso8601;
use chrono_core::{Actor, CoreError, Cursor, Event, Link, Result as CoreResult, Watermark};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};

/// Umbral bulk (nº de touches) por encima del cual un evento se excluye del
/// coupling. Igual que `BulkThreshold` en Go.
pub const BULK_THRESHOLD: usize = 50;

/// Formato de `git log`: %x1e separa registros; %x1f separa campos.
/// Incluye `%at` (epoch UNIX) además de `%aI` (ISO con offset del autor).
const LOG_FORMAT: &str = "--pretty=format:%x1e%H%x1f%an%x1f%ae%x1f%at%x1f%aI%x1f%s%x1f%b%x1f";

pub struct GitCursor {
    child: Child,
    reader: BufReader<ChildStdout>,
    buf: Vec<u8>,
    head: String,
    source_id: String,
    done: bool,
}

impl GitCursor {
    /// Arranca `git log` en streaming sobre `repo`. `range` es, por ejemplo,
    /// `Some("<sha>..HEAD")` para una ingesta incremental, o `None` para todo.
    pub fn spawn(repo: &std::path::Path, head: String, range: Option<String>) -> CoreResult<Self> {
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
            args.push(r);
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

        let abs = std::fs::canonicalize(repo).unwrap_or_else(|_| PathBuf::from(repo));
        let source_id = format!("git:{}", abs.to_string_lossy());

        Ok(Self {
            child,
            reader: BufReader::new(stdout),
            buf: Vec::new(),
            head,
            source_id,
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

impl Cursor for GitCursor {
    fn next(&mut self) -> CoreResult<Option<Event>> {
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

            let Some(raw) = parse::split_record(&rec) else {
                continue; // registro corrupto/incompleto: se descarta, como en Go.
            };

            let at_epoch: i64 = match raw.at_epoch.trim().parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
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
                source_id: self.source_id.clone(),
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
            return Ok(Some(ev));
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
