//! `explain`: lo que el ÍNDICE sabe de los commits que escribieron un trozo de
//! código (los da `git blame`, ver `chrono_source_git::blame`): si eran
//! arreglos y con qué certeza, su categoría, sus tickets/PRs, y qué otras
//! entidades cambiaron en esos mismos commits.
//!
//! Solo hechos del índice, con orden total: misma entrada, misma salida.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension};

use crate::Result;

/// Certeza de que un commit es un arreglo, según la etiqueta `is_fix` del
/// clasificador: `Yes` = prefijo conventional `fix:` o revert (confianza 1.0),
/// `Maybe` = solo una palabra clave en el mensaje, `No`, o `Unknown` si el
/// commit no está en el índice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixCertainty {
    Yes,
    Maybe,
    No,
    Unknown,
}

impl FixCertainty {
    pub fn as_str(self) -> &'static str {
        match self {
            FixCertainty::Yes => "yes",
            FixCertainty::Maybe => "maybe",
            FixCertainty::No => "no",
            FixCertainty::Unknown => "-",
        }
    }
}

/// Un ticket/PR referenciado por un commit, con lo que se sepa del forge.
#[derive(Debug, Clone, PartialEq)]
pub struct TicketRef {
    pub id: String,
    /// `pr` | `issue` si está en la tabla `issues` (forge indexado), si no "".
    pub kind: String,
    pub title: String,
    pub state: String,
}

/// Lo que el índice sabe de UN commit.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitFacts {
    pub in_index: bool,
    pub fix: FixCertainty,
    pub category: String,
    pub is_bulk: bool,
    pub tickets: Vec<TicketRef>,
}

/// Una entidad que cambió en `commits` de los commits del trozo.
#[derive(Debug, Clone, PartialEq)]
pub struct CoChange {
    pub entity: String,
    pub commits: i64,
    /// Con un índice `--symbols`: las funciones de ese fichero que cambiaron
    /// en esos commits, `(nombre, commits)`, de más a menos (empate por nombre).
    pub symbols: Vec<(String, i64)>,
}

/// Hechos del índice para cada sha (en el orden dado).
pub fn commit_facts(conn: &Connection, shas: &[String]) -> Result<Vec<CommitFacts>> {
    let mut out = Vec::with_capacity(shas.len());
    for sha in shas {
        let ev: Option<i64> = conn
            .query_row("SELECT is_bulk FROM events WHERE id = ?1 AND kind = 'commit'", params![sha], |r| r.get(0))
            .optional()?;
        let Some(is_bulk) = ev else {
            out.push(CommitFacts { in_index: false, fix: FixCertainty::Unknown, category: String::new(), is_bulk: false, tickets: Vec::new() });
            continue;
        };
        let fix: Option<(String, f64)> = conn
            .query_row(
                "SELECT label, confidence FROM labels WHERE event_id = ?1 AND task = 'is_fix'",
                params![sha],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let fix = match fix {
            Some((l, c)) if l == "true" && c >= 1.0 => FixCertainty::Yes,
            Some((l, _)) if l == "true" => FixCertainty::Maybe,
            Some(_) => FixCertainty::No,
            None => FixCertainty::No,
        };
        let category: String = conn
            .query_row("SELECT label FROM labels WHERE event_id = ?1 AND task = 'bug_category'", params![sha], |r| r.get(0))
            .optional()?
            .unwrap_or_default();
        let mut stmt = conn.prepare("SELECT target FROM links WHERE event_id = ?1 AND rel IN ('ticket', 'pr') ORDER BY target")?;
        let targets: Vec<String> = stmt.query_map(params![sha], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        let mut tickets = Vec::new();
        for target in targets {
            let number: Option<i64> = target.trim_start_matches('#').parse().ok();
            let issue: Option<(String, String, String)> = match number {
                Some(n) => conn
                    .query_row(
                        "SELECT kind, title, state FROM issues WHERE number = ?1 ORDER BY kind DESC LIMIT 1",
                        params![n],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?,
                None => None,
            };
            let (kind, title, state) = issue.unwrap_or_default();
            tickets.push(TicketRef { id: target, kind, title, state });
        }
        out.push(CommitFacts { in_index: true, fix, category, is_bulk: is_bulk != 0, tickets });
    }
    Ok(out)
}

/// Entidades que cambiaron en ≥ `min_commits` de los commits del trozo
/// (descontando los masivos), sin el propio fichero ni sus símbolos
/// (`fichero#func`). Es el acoplamiento DEL TROZO, no del fichero entero: un
/// fichero grande cambia con medio repo; las líneas que estás leyendo, no.
/// Orden: nº de commits DESC, entidad ASC.
pub fn co_changes(conn: &Connection, shas: &[String], file: &str, min_commits: i64, limit: usize) -> Result<Vec<CoChange>> {
    let mut counts: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT en.key FROM touches t
         JOIN entities en ON en.id = t.entity_id
         JOIN events e ON e.id = t.event_id
         WHERE e.id = ?1 AND e.kind = 'commit' AND e.is_bulk = 0",
    )?;
    let own_symbols = format!("{file}#");
    for sha in shas {
        let keys: Vec<String> = stmt.query_map(params![sha], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        for k in keys {
            if k == file || k.starts_with(&own_symbols) {
                continue;
            }
            counts.entry(k).or_default().insert(sha.as_str());
        }
    }
    // Los símbolos (`fichero#func`) no son filas propias: van con su fichero,
    // que ya cuenta como entidad. Así una misma co-ocurrencia no sale dos veces.
    let mut symbols: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    let mut files: Vec<CoChange> = Vec::new();
    for (entity, s) in counts {
        let n = s.len() as i64;
        match entity.split_once('#') {
            Some((f, sym)) => symbols.entry(f.to_string()).or_default().push((sym.to_string(), n)),
            None => files.push(CoChange { entity, commits: n, symbols: Vec::new() }),
        }
    }
    let mut out: Vec<CoChange> = files.into_iter().filter(|c| c.commits >= min_commits).collect();
    out.sort_by(|a, b| b.commits.cmp(&a.commits).then_with(|| a.entity.cmp(&b.entity)));
    out.truncate(limit);
    for c in &mut out {
        if let Some(mut syms) = symbols.remove(&c.entity) {
            syms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            syms.truncate(3);
            c.symbols = syms;
        }
    }
    Ok(out)
}

/// Un commit tal como lo da `git blame` (sin depender de `chrono-source-git`).
#[derive(Debug, Clone, PartialEq)]
pub struct Blamed {
    pub sha: String,
    pub author: String,
    /// Segundos UTC.
    pub author_time: i64,
    pub summary: String,
    pub lines: u32,
}

/// Una fila de la explicación: el commit y lo que el índice sabe de él.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplainedCommit {
    pub blamed: Blamed,
    pub facts: CommitFacts,
}

/// La explicación completa de un trozo, ya ordenada. Es la ÚNICA definición de
/// las reglas (orden, recuentos, acoplamiento): CLI y MCP solo la serializan.
#[derive(Debug, Clone, PartialEq)]
pub struct Explanation {
    /// Más reciente primero; empate por sha. Ya recortada a `limit`.
    pub commits: Vec<ExplainedCommit>,
    pub total_commits: usize,
    pub fixes_yes: i64,
    pub fixes_maybe: i64,
    pub not_indexed: i64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
    /// `(autor, líneas)`: más líneas primero; empate por nombre.
    pub authors: Vec<(String, i64)>,
    pub co_changes: Vec<CoChange>,
    /// Commits del trozo que cuentan para el acoplamiento (indexados, no masivos).
    pub co_base: i64,
}

/// Mínimo de commits del trozo en que debe aparecer una entidad para decir que
/// «cambia con él», y cuántas se listan.
pub const CO_MIN_COMMITS: i64 = 2;
pub const CO_LIMIT: usize = 8;

pub fn explain(conn: &Connection, file: &str, blamed: &[Blamed], limit: usize) -> Result<Explanation> {
    let shas: Vec<String> = blamed.iter().map(|b| b.sha.clone()).collect();
    let facts = commit_facts(conn, &shas)?;
    let co = co_changes(conn, &shas, file, CO_MIN_COMMITS, CO_LIMIT)?;
    let count = |k: FixCertainty| facts.iter().filter(|f| f.fix == k).count() as i64;
    let (fixes_yes, fixes_maybe, not_indexed) = (count(FixCertainty::Yes), count(FixCertainty::Maybe), count(FixCertainty::Unknown));
    let co_base = facts.iter().filter(|f| f.in_index && !f.is_bulk).count() as i64;

    let mut by_author: BTreeMap<String, i64> = BTreeMap::new();
    for b in blamed {
        *by_author.entry(b.author.clone()).or_default() += b.lines as i64;
    }
    let mut authors: Vec<(String, i64)> = by_author.into_iter().collect();
    authors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut commits: Vec<ExplainedCommit> =
        blamed.iter().cloned().zip(facts).map(|(blamed, facts)| ExplainedCommit { blamed, facts }).collect();
    commits.sort_by(|a, b| b.blamed.author_time.cmp(&a.blamed.author_time).then_with(|| a.blamed.sha.cmp(&b.blamed.sha)));
    let total_commits = commits.len();
    commits.truncate(limit);
    Ok(Explanation {
        commits,
        total_commits,
        fixes_yes,
        fixes_maybe,
        not_indexed,
        oldest: blamed.iter().map(|b| b.author_time).min(),
        newest: blamed.iter().map(|b| b.author_time).max(),
        authors,
        co_changes: co,
        co_base,
    })
}

/// `AAAA-MM-DD` en UTC de unos segundos epoch. Una sola definición para CLI y
/// MCP (nunca la zona horaria local: la respuesta no puede depender de dónde
/// se ejecute). Algoritmo «days from civil» de H. Hinnant, inverso.
pub fn utc_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

/// `fichero`, `fichero:N` o `fichero:desde-hasta` (1-based, inclusivo). El
/// error es un texto en inglés estable; quien llama lo localiza.
pub fn parse_target(target: &str) -> std::result::Result<(String, Option<(u32, u32)>), String> {
    let bad = || format!("invalid range in {target:?} (use file:FROM-TO)");
    match target.rsplit_once(':') {
        Some((f, r)) if !f.is_empty() && r.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            let (a, b) = match r.split_once('-') {
                Some((a, b)) => (a.parse::<u32>().map_err(|_| bad())?, b.parse::<u32>().map_err(|_| bad())?),
                None => {
                    let n = r.parse::<u32>().map_err(|_| bad())?;
                    (n, n)
                }
            };
            if a == 0 || b < a {
                return Err(bad());
            }
            Ok((f.trim_start_matches("./").to_string(), Some((a, b))))
        }
        _ => Ok((target.trim_start_matches("./").to_string(), None)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono_core::{Actor, Event, Label, Link, Touch};
    use chrono_store::Store;

    use super::*;

    fn commit(id: &str, files: &[&str]) -> Event {
        Event {
            id: id.into(),
            source_id: "git:/r".into(),
            kind: "commit".into(),
            at: "2026-01-01T00:00:00Z".into(),
            at_epoch: 1,
            actor: Actor::default(),
            title: id.into(),
            touches: files.iter().map(|f| Touch { entity: f.to_string(), entity_type: "file".into(), weight: 1, attrs: BTreeMap::new() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn hechos_y_acoplamiento_del_trozo() {
        let path = std::env::temp_dir().join(format!("chrono-explain-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path).unwrap();
        {
            let mut w = store.writer().unwrap();
            let mut a = commit("a", &["main.py", "config.py", "main.py#proxy"]);
            a.links = vec![Link { rel: "ticket".into(), target: "#7".into() }];
            w.add_event(&a).unwrap();
            w.add_event(&commit("b", &["main.py", "config.py", "otro.py"])).unwrap();
            w.add_event(&commit("c", &["main.py", "otro.py"])).unwrap();
            let lab = |task: &str, label: &str, conf: f64| Label { task: task.into(), label: label.into(), confidence: conf, source: "rules".into(), evidence: vec![] };
            w.add_labels("a", &[lab("is_fix", "true", 1.0), lab("bug_category", "network", 0.7)]).unwrap();
            w.add_labels("b", &[lab("is_fix", "true", 0.8)]).unwrap();
            w.add_labels("c", &[lab("is_fix", "false", 1.0)]).unwrap();
            w.commit().unwrap();
        }
        let conn = store.conn();
        let shas: Vec<String> = ["a", "b", "c", "zzz"].iter().map(|s| s.to_string()).collect();
        let f = commit_facts(conn, &shas).unwrap();
        assert_eq!(f.iter().map(|x| x.fix).collect::<Vec<_>>(), vec![FixCertainty::Yes, FixCertainty::Maybe, FixCertainty::No, FixCertainty::Unknown]);
        assert_eq!(f[0].category, "network");
        assert_eq!(f[0].tickets[0].id, "#7");
        assert!(!f[3].in_index);
        let co = co_changes(conn, &shas, "main.py", 2, 10).unwrap();
        assert_eq!(
            co,
            vec![
                CoChange { entity: "config.py".into(), commits: 2, symbols: vec![] },
                CoChange { entity: "otro.py".into(), commits: 2, symbols: vec![] }
            ]
        );
        assert!(co.iter().all(|c| !c.entity.starts_with("main.py#")), "sin los símbolos del propio fichero");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fecha_utc_y_rangos() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_755_475_200), "2025-08-18");
        assert_eq!(utc_date(1_709_164_800), "2024-02-29");
        assert_eq!(utc_date(1_710_000_000 - 1), "2024-03-09");
        assert_eq!(parse_target("src/a.py:10-20").unwrap(), ("src/a.py".into(), Some((10, 20))));
        assert_eq!(parse_target("./src/a.py:7").unwrap(), ("src/a.py".into(), Some((7, 7))));
        assert_eq!(parse_target("src/a.py").unwrap(), ("src/a.py".into(), None));
        assert!(parse_target("src/a.py:20-10").is_err());
        assert!(parse_target("src/a.py:0-3").is_err());
    }
}
