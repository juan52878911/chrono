//! `relations` de la config al ingerir (P-REL-2): cada extremo que toca a una
//! fuente saca la clave de un campo del evento (con patrón opcional) y la
//! añade como `link` con `rel = nombre de la relación`. Así dos eventos con
//! la misma clave, de la fuente que sea, comparten `link_targets.id`.

use std::path::Path;

use chrono_classify_rules::{Config, RelationSide};
use chrono_core::{Event, Link, Pattern};

/// Un extremo ya compilado para una fuente concreta.
pub(super) struct Side {
    name: String,
    field: String,
    pattern: Option<Pattern>,
    len: usize,
    /// Para la firma: si cambia, la fuente se reindexa.
    spec: String,
}

/// ¿Nombra `selector` a esta fuente? Vacío = todas.
fn selects(selector: &str, names: &[&str]) -> bool {
    selector.is_empty() || names.contains(&selector)
}

/// Extremos de `cfg.relations` que tocan a la fuente. `names` son las formas
/// de nombrarla: id, tipo, ruta, nombre de fichero y alias/patrón de una glob.
/// Un patrón inválido se omite (lo dice `chrono config check`).
pub(super) fn sides_for(cfg: &Config, kind: &str, source_id: &str, path: &Path, keys: &[String]) -> Vec<Side> {
    let path_s = path.to_string_lossy();
    let base = path.file_name().map(|b| b.to_string_lossy()).unwrap_or_default();
    let mut names: Vec<&str> = vec![source_id, kind, &path_s, &base];
    names.extend(keys.iter().map(String::as_str));
    names.retain(|n| !n.is_empty());
    let mut out = Vec::new();
    for (name, sides) in &cfg.relations {
        for side in sides.iter().filter(|s| selects(&s.source, &names)) {
            let RelationSide { field, pattern, len, .. } = side;
            let compiled = if pattern.is_empty() {
                None
            } else {
                match Pattern::parse(pattern) {
                    Ok(p) => Some(p),
                    Err(_) => continue,
                }
            };
            let field = if field.is_empty() { name.clone() } else { field.clone() };
            let spec = format!("{field}\u{1f}{pattern}\u{1f}{len}");
            out.push(Side { name: name.clone(), field, pattern: compiled, len: *len, spec });
        }
    }
    out
}

/// Entradas para `source_dims`: `rel:<nombre>#<hash del extremo>`. Van junto
/// a las dims, así que cambiar una relación reindexa la fuente igual que
/// cambiar sus dims (las claves ya escritas no se pueden rehacer sin releer).
pub(super) fn signature(sides: &[Side]) -> Vec<String> {
    let mut sig: Vec<String> =
        sides.iter().map(|s| format!("rel:{}#{:08x}", s.name, super::fnv1a32(s.spec.as_bytes()))).collect();
    sig.sort();
    sig.dedup();
    sig
}

/// Valores crudos del campo en el evento (antes del patrón).
fn raw_values<'e>(ev: &'e Event, field: &str) -> Vec<&'e str> {
    match field {
        "title" => vec![&ev.title],
        // Un id declarado lleva delante su kind o su fichero (`tool_use:toolu_…`,
        // `app.jsonl:42`): la clave es lo de después del primer `:`.
        "id" => vec![ev.id.split_once(':').map_or(ev.id.as_str(), |(_, rest)| rest)],
        "kind" => vec![&ev.kind],
        "level" => vec![&ev.level],
        "actor" => vec![&ev.actor.key],
        "entity" => ev.touches.iter().map(|t| t.entity.as_str()).collect(),
        "file" => match ev.attrs.get("file").or_else(|| ev.attrs.get("path")) {
            Some(f) => vec![f],
            None => ev.touches.iter().map(|t| t.entity.as_str()).collect(),
        },
        f => match f.strip_prefix("link:") {
            Some(rel) => ev.links.iter().filter(|l| l.rel == rel).map(|l| l.target.as_str()).collect(),
            None => ev.attrs.get(f).map(String::as_str).into_iter().collect(),
        },
    }
}

/// Añade a `ev.links` las claves de cada extremo (sin repetir).
pub(super) fn apply(sides: &[Side], ev: &mut Event) {
    let mut found: Vec<Link> = Vec::new();
    for side in sides {
        for raw in raw_values(ev, &side.field) {
            let values = match &side.pattern {
                Some(p) => p.find_all(raw),
                None => vec![raw],
            };
            for v in values {
                let v = v.trim();
                let v = match side.len {
                    0 => v,
                    n => v.char_indices().nth(n).map_or(v, |(b, _)| &v[..b]),
                };
                if v.is_empty() {
                    continue;
                }
                let link = Link { rel: side.name.clone(), target: v.to_string() };
                if !found.contains(&link) && !ev.links.contains(&link) {
                    found.push(link);
                }
            }
        }
    }
    ev.links.extend(found);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_classify_rules::RelationSide;
    use chrono_core::Touch;

    fn cfg(json: &str) -> Config {
        serde_json::from_str(json).unwrap()
    }

    fn ev() -> Event {
        let mut e = Event { id: "file:abc".into(), kind: "file".into(), title: "lote/prueba-0042_ok.csv".into(), ..Default::default() };
        e.attrs.insert("path".into(), "lote/prueba-0042_ok.csv".into());
        e.touches.push(Touch { entity: "lote".into(), entity_type: "folder".into(), weight: 1, attrs: Default::default() });
        e
    }

    #[test]
    fn extremos_por_fuente_y_valores_con_patron() {
        let c = cfg(r#"{"relations":{"prueba":[{"source":"pruebas","field":"file","pattern":"prueba-(\\d+)"},
                                            {"source":"git","field":"title","pattern":"prueba-(\\d+)"}]}}"#);
        let sides = sides_for(&c, "dir", "dir:/x/pruebas", Path::new("/x/pruebas"), &[]);
        assert_eq!(sides.len(), 1);
        let mut e = ev();
        apply(&sides, &mut e);
        assert_eq!(e.links, vec![Link { rel: "prueba".into(), target: "0042".into() }]);
        assert!(sides_for(&c, "jsonl", "jsonl:/y", Path::new("/y"), &[]).is_empty());
        assert_eq!(signature(&sides).len(), 1);
        assert!(signature(&sides)[0].starts_with("rel:prueba#"));
    }

    #[test]
    fn id_sin_kind_link_y_recorte() {
        let side = |field: &str, len| RelationSide { field: field.into(), len, ..Default::default() };
        let c = Config { relations: [("sha".to_string(), vec![side("id", 7)]), ("u".to_string(), vec![side("link:tool_use", 0)])].into(), ..Config::default() };
        let sides = sides_for(&c, "jsonl", "jsonl:/a", Path::new("/a"), &[]);
        let mut e = Event { id: "commit:1a2b3c4d5e".into(), kind: "commit".into(), ..Default::default() };
        e.links.push(Link { rel: "tool_use".into(), target: "toolu_1".into() });
        apply(&sides, &mut e);
        assert!(e.links.contains(&Link { rel: "sha".into(), target: "1a2b3c4".into() }));
        assert!(e.links.contains(&Link { rel: "u".into(), target: "toolu_1".into() }));
        // Un patrón distinto cambia la firma (reindexa la fuente).
        let mut c2 = c.clone();
        c2.relations.get_mut("sha").unwrap()[0].len = 8;
        assert_ne!(signature(&sides), signature(&sides_for(&c2, "jsonl", "jsonl:/a", Path::new("/a"), &[])));
    }
}
