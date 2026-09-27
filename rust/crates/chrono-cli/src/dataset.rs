//! `chrono export-dataset`: commits etiquetados como JSONL en el formato
//! exacto que consume `kling ai chispa train` (kindling, `pkg/chispa`):
//!
//! ```text
//! {"text":"...","label":"fix","fields":{"files":3,"churn":42,"ext":[".go"],"dir":["internal"]},"id":"<sha>","repo":"<nombre>@<rama>","time":<epoch>,"label_source":"rules"}
//! ```
//!
//! Chispa es el único clasificador aprendido de chrono (docs/CONTRACTS.md).
//! Paridad con el Go `internal/dataset`: etiqueta = señal determinista del
//! índice, por prioridad forge (ticket `#N` enlazado a un issue/PR con label
//! de bug) > revert > prefijo conventional commit > `is_fix` por palabra
//! clave; sin señal, no se exporta. El prefijo se QUITA del texto (si no, el
//! modelo aprende a leer las cuatro primeras letras), y del cuerpo se quitan
//! trailers y líneas con prefijo. Orden temporal; textos repetidos, fuera.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;

use chrono_store::Store;

use crate::json::Json;

/// Bytes de cuerpo tras el asunto (como `tools/chispa-commits` de kindling).
pub const DEFAULT_BODY_BYTES: usize = 300;

const TYPES: [&str; 10] = [
    "feat", "fix", "docs", "refactor", "test", "chore", "perf", "ci", "build", "style",
];
const TRAILERS: [&str; 12] = [
    "signed-off-by",
    "co-authored-by",
    "reviewed-by",
    "acked-by",
    "tested-by",
    "reported-by",
    "cc",
    "change-id",
    "fixes",
    "closes",
    "refs",
    "ref",
];

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Longitud en bytes de un `tipo(ámbito)!:` al principio de `s` (tras
/// espacios opcionales), o `None`. Sin `regex`, como el resto del port.
fn conv_prefix_len(s: &str) -> Option<usize> {
    let lead = s.len() - s.trim_start().len();
    let rest = &s[lead..];
    let low = rest.to_ascii_lowercase();
    let ty = TYPES.iter().find(|t| low.starts_with(**t))?;
    let mut i = lead + ty.len();
    let b = s.as_bytes();
    if b.get(i) == Some(&b'(') {
        i += s[i..].find(')')? + 1;
    }
    if b.get(i) == Some(&b'!') {
        i += 1;
    }
    if b.get(i) != Some(&b':') {
        return None;
    }
    Some(i + 1)
}

fn is_bullet_prefixed(line: &str) -> bool {
    let l = line.trim_start();
    let l = l
        .strip_prefix('-')
        .or_else(|| l.strip_prefix('*'))
        .unwrap_or(l);
    conv_prefix_len(l).is_some()
}

fn is_trailer(line: &str) -> bool {
    let low = line.trim_start().to_ascii_lowercase();
    TRAILERS.iter().any(|t| {
        low.strip_prefix(t)
            .is_some_and(|r| r.trim_start().starts_with(':'))
    })
}

fn clean_body(body: &str, n: usize) -> String {
    let keep: Vec<&str> = body
        .split('\n')
        .filter(|l| !is_trailer(l) && !is_bullet_prefixed(l))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let s = keep.join("\n");
    if s.len() <= n {
        return s;
    }
    // Recorta a n bytes sin partir una runa.
    let mut cut = n;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].to_string()
}

/// Lo que ve el modelo: asunto sin prefijo y, en otra línea, hasta `n`
/// bytes de cuerpo limpio. Vacío si no queda asunto.
pub fn text(subject: &str, body: &str, n: usize) -> String {
    let mut s = subject.trim();
    if let Some(k) = conv_prefix_len(s) {
        s = s[k..].trim();
    }
    if s.is_empty() {
        return String::new();
    }
    let mut out = s.to_string();
    if n > 0 {
        let b = clean_body(body, n);
        if !b.is_empty() {
            out.push('\n');
            out.push_str(&b);
        }
    }
    out
}

fn top(m: &BTreeMap<String, i64>, n: usize) -> Vec<Json> {
    let mut keys: Vec<(&String, &i64)> = m.iter().collect();
    keys.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    keys.into_iter()
        .take(n)
        .map(|(k, _)| Json::str(k))
        .collect()
}

/// Campos de Chispa a partir de (ruta, peso=añadidas+borradas; -1 binario).
pub fn fields(touches: &[(String, i64)]) -> Option<Json> {
    if touches.is_empty() {
        return None;
    }
    let mut churn = 0i64;
    let (mut exts, mut dirs) = (BTreeMap::new(), BTreeMap::new());
    for (path, w) in touches {
        churn += (*w).max(0);
        let base = path.rsplit('/').next().unwrap_or(path);
        let ext = match base.rfind('.') {
            Some(i) => base[i..].to_lowercase(),
            None => "(none)".to_string(),
        };
        *exts.entry(ext).or_insert(0) += 1;
        let dir = match path.find('/') {
            Some(i) if i > 0 => path[..i].to_string(),
            _ => "(root)".to_string(),
        };
        *dirs.entry(dir).or_insert(0) += 1;
    }
    Some(
        Json::obj()
            .set("files", Json::Int(touches.len() as i64))
            .set("churn", Json::Int(churn))
            .set("ext", Json::Arr(top(&exts, 8)))
            .set("dir", Json::Arr(top(&dirs, 8))),
    )
}

/// Escribe el JSONL y devuelve cuántas filas. `repo` es la etiqueta
/// `<nombre>@<rama>` de cada fila; `since_epoch` acota por fecha (0 = todo).
pub fn export(
    store: &Store,
    w: &mut dyn Write,
    since_epoch: i64,
    body_bytes: usize,
    repo: Option<&str>,
) -> Result<usize> {
    let conn = store.conn();

    // Etiquetas deterministas por evento.
    let mut kind: HashMap<String, String> = HashMap::new();
    let mut is_fix: HashSet<String> = HashSet::new();
    {
        let mut st = conn
            .prepare("SELECT event_id, task, label FROM labels WHERE task IN ('kind','is_fix')")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, task, label) = row?;
            if task == "kind" {
                kind.insert(id, label);
            } else if label == "true" {
                is_fix.insert(id);
            }
        }
    }
    let mut reverts: HashSet<String> = HashSet::new();
    let mut forge_bug: HashSet<String> = HashSet::new();
    {
        let mut st = conn.prepare(
            "SELECT l.event_id, l.rel, EXISTS(SELECT 1 FROM issues i WHERE i.is_bug = 1 AND ('#' || i.number) = l.target)
             FROM links l WHERE l.rel IN ('reverts','ticket')",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, rel, bug) = row?;
            if rel == "reverts" {
                reverts.insert(id);
            } else if bug == 1 {
                forge_bug.insert(id);
            }
        }
    }
    // Ficheros tocados por evento (solo entidades de tipo fichero).
    let mut touches: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    {
        let mut st = conn.prepare(
            "SELECT t.event_id, e.key, t.weight FROM touches t JOIN entities e ON e.id = t.entity_id
             JOIN events ev ON ev.id = t.event_id
             WHERE e.type = 'file' AND ev.kind = 'commit' AND ev.at_epoch >= ?1",
        )?;
        let rows = st.query_map([since_epoch], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, key, weight) = row?;
            touches.entry(id).or_default().push((key, weight));
        }
    }

    let mut st = conn.prepare(
        "SELECT id, at_epoch, title, body FROM events WHERE kind = 'commit' AND at_epoch >= ?1 ORDER BY at_epoch, id",
    )?;
    let rows = st.query_map([since_epoch], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut n = 0;
    for row in rows {
        let (id, at, title, body) = row?;
        let (label, source) = if forge_bug.contains(&id) {
            ("fix".to_string(), "forge")
        } else if reverts.contains(&id) {
            ("fix".to_string(), "rules")
        } else if let Some(k) = kind.get(&id) {
            (k.clone(), "rules")
        } else if is_fix.contains(&id) {
            ("fix".to_string(), "rules")
        } else {
            continue;
        };
        let txt = text(&title, &body, body_bytes);
        if txt.is_empty() {
            continue;
        }
        let key = txt
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let mut obj = Json::obj()
            .set("text", Json::str(&txt))
            .set("label", Json::str(&label));
        if let Some(f) = touches.get(&id).and_then(|t| fields(t)) {
            obj = obj.set("fields", f);
        }
        obj = obj.set("id", Json::str(&id));
        if let Some(r) = repo {
            obj = obj.set("repo", Json::str(r));
        }
        obj = obj
            .set("time", Json::Int(at))
            .set("label_source", Json::str(source));
        w.write_all(obj.compact().as_bytes())?;
        w.write_all(b"\n")?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefijo_conventional() {
        assert_eq!(text("feat(api)!: add endpoint", "", 300), "add endpoint");
        assert_eq!(text("  Fix: crash", "", 300), "crash");
        assert_eq!(text("fixes the thing", "", 300), "fixes the thing");
        assert_eq!(text("fix: ", "cuerpo", 300), "");
        assert_eq!(
            text(
                "docs: x",
                "Body\nSigned-off-by: A <a@x>\n- fix: bullet\nRefs : #3",
                300
            ),
            "x\nBody"
        );
    }

    #[test]
    fn recorta_sin_partir_runas() {
        assert_eq!(text("docs: ñandú", "ááá", 3), "ñandú\ná");
        assert_eq!(text("docs: x", "ab—", 5), "x\nab—");
        assert_eq!(text("docs: x", "ab—c", 4), "x\nab");
    }

    #[test]
    fn campos_como_kindling() {
        let f = fields(&[
            ("api/h.go".into(), 7),
            ("api/h_test.go".into(), 7),
            ("README.md".into(), 7),
            ("img.png".into(), -1),
        ])
        .unwrap();
        assert_eq!(
            f.compact(),
            r#"{"files":4,"churn":21,"ext":[".go",".md",".png"],"dir":["(root)","api"]}"#
        );
    }

    /// Índice real: forge > revert > conventional > palabra clave; sin
    /// señal no sale; duplicados fuera; orden temporal; formato de Chispa.
    #[test]
    fn export_formato_chispa() {
        use chrono_core::{Actor, Event, Label, Link, Touch};
        let path = std::env::temp_dir().join(format!("chrono-dataset-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path).unwrap();
        let ev =
            |id: &str, at: i64, title: &str, body: &str, files: &[&str], links: Vec<Link>| Event {
                id: id.into(),
                source_id: "git:/r".into(),
                kind: "commit".into(),
                at: String::new(),
                at_epoch: at,
                actor: Actor {
                    name: "A".into(),
                    key: "a@x".into(),
                },
                title: title.into(),
                body: body.into(),
                level: String::new(),
                attrs: Default::default(),
                touches: files
                    .iter()
                    .map(|f| Touch {
                        entity: f.to_string(),
                        entity_type: "file".into(),
                        weight: 7,
                        attrs: Default::default(),
                    })
                    .collect(),
                links,
                simhash: 0,
                is_bulk: false,
            };
        let lbl = |task: &str, label: &str| Label {
            task: task.into(),
            label: label.into(),
            confidence: 1.0,
            source: "rules".into(),
            evidence: vec![],
        };
        let ticket = |t: &str| {
            vec![Link {
                rel: "ticket".into(),
                target: t.into(),
            }]
        };
        {
            let mut w = store.writer().unwrap();
            let rows = [
                (
                    ev(
                        "c2",
                        20,
                        "feat(api): add endpoint",
                        "Body\nSigned-off-by: A",
                        &["api/h.go", "README.md"],
                        vec![],
                    ),
                    vec![lbl("kind", "feat"), lbl("is_fix", "false")],
                ),
                (
                    ev(
                        "c1",
                        10,
                        "cache goes wrong #7",
                        "",
                        &["cache/c.go"],
                        ticket("#7"),
                    ),
                    vec![lbl("is_fix", "false")],
                ),
                (
                    ev("c3", 30, "tidy things up", "", &["x.go"], vec![]),
                    vec![lbl("is_fix", "false")],
                ),
                (
                    ev(
                        "c4",
                        40,
                        "fix: Cache goes  wrong #7",
                        "",
                        &["cache/c.go"],
                        vec![],
                    ),
                    vec![lbl("kind", "fix"), lbl("is_fix", "true")],
                ),
                (
                    ev(
                        "c5",
                        50,
                        "Revert \"x\"",
                        "",
                        &["x.go"],
                        vec![Link {
                            rel: "reverts".into(),
                            target: "c3".into(),
                        }],
                    ),
                    vec![lbl("is_fix", "true")],
                ),
                (
                    ev("c6", 60, "arreglo del parser", "", &["p.go"], vec![]),
                    vec![lbl("is_fix", "true")],
                ),
            ];
            for (e, l) in &rows {
                w.add_event(e).unwrap();
                w.add_labels(&e.id, l).unwrap();
            }
            w.commit().unwrap();
        }
        store
            .conn()
            .execute(
                "INSERT INTO issues(number, kind, title, is_bug) VALUES (7, 'issue', 'cache', 1)",
                [],
            )
            .unwrap();

        let mut out = Vec::new();
        let n = export(&store, &mut out, 0, DEFAULT_BODY_BYTES, Some("demo@main")).unwrap();
        let _ = std::fs::remove_file(&path);
        let got = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(n, 4, "{got}");
        assert_eq!(
            lines[0],
            r#"{"text":"cache goes wrong #7","label":"fix","fields":{"files":1,"churn":7,"ext":[".go"],"dir":["cache"]},"id":"c1","repo":"demo@main","time":10,"label_source":"forge"}"#
        );
        assert_eq!(
            lines[1],
            r#"{"text":"add endpoint\nBody","label":"feat","fields":{"files":2,"churn":14,"ext":[".go",".md"],"dir":["(root)","api"]},"id":"c2","repo":"demo@main","time":20,"label_source":"rules"}"#
        );
        assert!(lines[2].contains(r#""id":"c5""#) && lines[2].contains(r#""label":"fix""#));
        assert!(lines[3].contains(r#""id":"c6""#) && lines[3].contains(r#""label":"fix""#));
    }
}
