//! Parseo de registros `git log` (numstat, renames, reverts, tickets).
//! Sin regex: todo con operaciones de cadena, réplica manual de
//! `internal/ingest/gitlog.go` (splitRename/parseNumstat) e
//! `internal/classify/classify.go` (revert/tickets), simplificado para R1.

use chrono_core::{Link, Touch};
use std::collections::BTreeMap;

/// Separador de registro de `git log --pretty=format:...` (0x1e).
pub const RECORD_SEP: u8 = 0x1e;
/// Separador de campo (0x1f).
pub const FIELD_SEP: char = '\u{1f}';

/// Un registro ya partido en sus 7 campos + bloque numstat opcional.
pub struct RawRecord<'a> {
    pub sha: &'a str,
    pub author_name: &'a str,
    pub author_email: &'a str,
    pub at_epoch: &'a str,
    /// `%aI` (ISO con offset del autor): se conserva por paridad con el
    /// formato de log del Go, pero `at` se deriva de `at_epoch`, no de este
    /// campo (evita reimplementar un parser de fechas con offset).
    #[allow(dead_code)]
    pub author_iso: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
    pub numstat: Option<&'a str>,
}

/// Parte un registro (ya sin el separador de registro) en sus campos.
/// Devuelve `None` si no trae al menos los 7 campos obligatorios.
pub fn split_record(rec: &str) -> Option<RawRecord<'_>> {
    let mut parts = rec.splitn(8, FIELD_SEP);
    let sha = parts.next()?;
    let author_name = parts.next()?;
    let author_email = parts.next()?;
    let at_epoch = parts.next()?;
    let author_iso = parts.next()?;
    let subject = parts.next()?;
    let body = parts.next()?;
    let numstat = parts.next();
    Some(RawRecord {
        sha,
        author_name,
        author_email,
        at_epoch,
        author_iso,
        subject,
        body,
        numstat,
    })
}

/// Parsea el bloque numstat de un commit en una lista de `Touch`.
pub fn parse_numstat(block: &str) -> Vec<Touch> {
    let mut out = Vec::new();
    for raw_line in block.split('\n') {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let mut cols = line.splitn(3, '\t');
        let (Some(added_s), Some(deleted_s), Some(path_s)) =
            (cols.next(), cols.next(), cols.next())
        else {
            continue;
        };

        let mut attrs = BTreeMap::new();
        let weight;
        if added_s == "-" || deleted_s == "-" {
            weight = -1;
            attrs.insert("added".to_string(), "-1".to_string());
            attrs.insert("deleted".to_string(), "-1".to_string());
        } else {
            let added: i64 = added_s.parse().unwrap_or(0);
            let deleted: i64 = deleted_s.parse().unwrap_or(0);
            weight = added + deleted;
            attrs.insert("added".to_string(), added.to_string());
            attrs.insert("deleted".to_string(), deleted.to_string());
        }

        let (new_path, old_path, is_rename) = split_rename(path_s);
        attrs.insert(
            "change_type".to_string(),
            (if is_rename { "R" } else { "M" }).to_string(),
        );
        if is_rename {
            attrs.insert("old_path".to_string(), old_path);
        }

        out.push(Touch {
            entity: new_path,
            entity_type: "file".to_string(),
            weight,
            attrs,
        });
    }
    out
}

/// Interpreta las notaciones de rename de `numstat`: `old => new` y
/// `pre{old => new}post`. Réplica de `splitRename` en Go.
fn split_rename(p: &str) -> (String, String, bool) {
    if !p.contains(" => ") {
        return (p.to_string(), String::new(), false);
    }
    if let Some(i) = p.find('{') {
        if let Some(j_rel) = p[i..].find('}') {
            let j = i + j_rel;
            if j > i {
                let mid = &p[i + 1..j];
                let pre = &p[..i];
                let post = &p[j + 1..];
                if let Some(sep) = mid.find(" => ") {
                    let old_seg = &mid[..sep];
                    let new_seg = &mid[sep + 4..];
                    let old_p = clean_path(&format!("{pre}{old_seg}{post}"));
                    let new_p = clean_path(&format!("{pre}{new_seg}{post}"));
                    return (new_p, old_p, true);
                }
            }
        }
    }
    if let Some(sep) = p.find(" => ") {
        let old_p = p[..sep].trim().to_string();
        let new_p = p[sep + 4..].trim().to_string();
        return (new_p, old_p, true);
    }
    (p.to_string(), String::new(), false)
}

/// Réplica simplificada de `filepath.Clean` para rutas estilo POSIX (las que
/// usa git internamente, siempre con '/').
fn clean_path(p: &str) -> String {
    let is_abs = p.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                if matches!(stack.last(), Some(&last) if last != "..") {
                    stack.pop();
                } else if !is_abs {
                    stack.push("..");
                }
            }
            s => stack.push(s),
        }
    }
    let joined = stack.join("/");
    let result = if is_abs {
        format!("/{joined}")
    } else {
        joined
    };
    if result.is_empty() {
        ".".to_string()
    } else {
        result
    }
}

/// Busca "This reverts commit <sha>" (insensible a mayúsculas, por línea,
/// con espacio inicial opcional) y devuelve el `Link` si lo encuentra.
pub fn find_revert_link(body: &str) -> Option<Link> {
    const NEEDLE: &str = "this reverts commit";
    for line in body.lines() {
        let trimmed = line.trim_start();
        // Corte por bytes seguro: `get` devuelve None si el índice cae dentro
        // de un carácter multibyte (p.ej. "−" U+2212 en un cuerpo real), en
        // vez de hacer panic como `&trimmed[..n]`.
        let Some(prefix) = trimmed.get(..NEEDLE.len()) else {
            continue;
        };
        if !prefix.eq_ignore_ascii_case(NEEDLE) {
            continue;
        }
        let rest = trimmed[NEEDLE.len()..].trim_start();
        let sha: String = rest
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if (7..=40).contains(&sha.len()) {
            return Some(Link {
                rel: "reverts".to_string(),
                target: sha.to_lowercase(),
            });
        }
    }
    None
}

/// Extrae tickets `#<dígitos>` o `<MAYÚSCULAS>-<dígitos>` (parseo manual, sin
/// regex). Preserva el orden de aparición y deduplica.
pub fn extract_tickets(text: &str) -> Vec<Link> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '#'));
        if token.is_empty() {
            continue;
        }
        if let Some(id) = classify_ticket(token) {
            if seen.insert(id.clone()) {
                out.push(Link {
                    rel: "ticket".to_string(),
                    target: id,
                });
            }
        }
    }
    out
}

fn classify_ticket(token: &str) -> Option<String> {
    if let Some(rest) = token.strip_prefix('#') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return Some(token.to_string());
        }
        return None;
    }
    if let Some(dash) = token.find('-') {
        let (left, right) = (&token[..dash], &token[dash + 1..]);
        if !left.is_empty()
            && left.chars().all(|c| c.is_ascii_uppercase())
            && !right.is_empty()
            && right.chars().all(|c| c.is_ascii_digit())
        {
            return Some(token.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_record_basico() {
        let rec = "abc\x1fAna\x1fana@x.com\x1f100\x1f2024-01-01T00:00:00Z\x1fSubj\x1fBody\x1f5\t2\tfoo.rs\n";
        let r = split_record(rec).unwrap();
        assert_eq!(r.sha, "abc");
        assert_eq!(r.author_name, "Ana");
        assert_eq!(r.subject, "Subj");
        assert_eq!(r.body, "Body");
        assert_eq!(r.numstat, Some("5\t2\tfoo.rs\n"));
    }

    #[test]
    fn numstat_calcula_weight() {
        let touches = parse_numstat("5\t2\tfoo.rs\n0\t0\tbar.rs\n");
        assert_eq!(touches[0].entity, "foo.rs");
        assert_eq!(touches[0].weight, 7);
        assert_eq!(touches[1].weight, 0);
    }

    #[test]
    fn numstat_binario_da_weight_menos_uno() {
        let touches = parse_numstat("-\t-\timg.png\n");
        assert_eq!(touches[0].weight, -1);
        assert_eq!(touches[0].attrs.get("change_type").unwrap(), "M");
    }

    #[test]
    fn numstat_rename_simple() {
        let touches = parse_numstat("3\t1\told.rs => new.rs\n");
        assert_eq!(touches[0].entity, "new.rs");
        assert_eq!(touches[0].attrs.get("change_type").unwrap(), "R");
        assert_eq!(touches[0].attrs.get("old_path").unwrap(), "old.rs");
    }

    #[test]
    fn numstat_rename_llaves() {
        let touches = parse_numstat("1\t1\tsrc/{a => b}/main.rs\n");
        assert_eq!(touches[0].entity, "src/b/main.rs");
        assert_eq!(touches[0].attrs.get("old_path").unwrap(), "src/a/main.rs");
    }

    #[test]
    fn revert_detectado() {
        let link = find_revert_link("Revert stuff\n\nThis reverts commit abc1234def.\n").unwrap();
        assert_eq!(link.rel, "reverts");
        assert_eq!(link.target, "abc1234def");
    }

    #[test]
    fn revert_no_hace_panic_con_multibyte() {
        // Cuerpo real de rustworkx: el corte de 19 bytes caía dentro de "−" (U+2212).
        assert!(find_revert_link("2d(d−1)+(d+1)(d−1)\n").is_none());
        assert!(find_revert_link("ñandúes al vuelo\n").is_none());
    }

    #[test]
    fn tickets_hash_y_guion() {
        let links = extract_tickets("Fix #123 for JIRA-456 and also #123 again");
        let targets: Vec<_> = links.iter().map(|l| l.target.clone()).collect();
        assert_eq!(targets, vec!["#123".to_string(), "JIRA-456".to_string()]);
    }
}
