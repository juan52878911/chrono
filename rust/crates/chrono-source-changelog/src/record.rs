//! Mapeo de una `parser::Release` ya extraída a un `chrono_core::Event` con
//! `kind="release"`. Única implementación del mapeo; la usan tanto el cursor
//! como los tests.

use crate::parser::Release;
use crate::simhash::{simhash, tokenize};
use crate::timeutil::{epoch_to_iso8601, parse_time_str};
use chrono_core::{Actor, Event, Touch};
use std::collections::BTreeMap;

/// Tope de caracteres del `body` (contenido de la sección): un changelog es
/// pequeño, pero una sección desbocada (parseo de un fichero raro) no debe
/// inflar el evento sin límite.
const MAX_BODY_CHARS: usize = 4096;

/// Recorta `s` a lo sumo `max_chars` caracteres (respetando fronteras UTF-8).
fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Convierte todas las `releases` de un fichero en sus `Event`, en el mismo
/// orden en que aparecen en el documento. Si una misma versión aparece más
/// de una vez (raro, pero el contrato lo contempla), las apariciones tras la
/// primera reciben un sufijo `#<n>` determinista por orden de aparición, para
/// que el `id` siga siendo único dentro de la fuente.
pub fn releases_to_events(releases: &[Release], source_id: &str) -> Vec<Event> {
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    releases
        .iter()
        .map(|r| {
            let occurrence = seen.entry(r.version.clone()).or_insert(0);
            *occurrence += 1;
            let id = if *occurrence == 1 { r.version.clone() } else { format!("{}#{}", r.version, occurrence) };
            release_to_event(r, id, source_id)
        })
        .collect()
}

/// Convierte una `Release` (con su `id` ya desambiguado) en un `Event`.
fn release_to_event(r: &Release, id: String, source_id: &str) -> Event {
    // `r.date` es solo "YYYY-MM-DD" (sin hora): `parse_time_str`/`parse_rfc3339`
    // exigen al menos una hora completa, así que se completa a medianoche UTC
    // antes de parsear (contrato: la fecha del encabezado es medianoche UTC).
    let time_value = r.date.as_deref().and_then(|d| parse_time_str(&format!("{d}T00:00:00Z")));
    let at_epoch = time_value.unwrap_or(0);
    let at = time_value.map(epoch_to_iso8601).unwrap_or_default();

    let title = format!("release {}", r.version);
    let body = truncate_chars(&r.body, MAX_BODY_CHARS);

    let touches: Vec<Touch> = r
        .categories
        .iter()
        .map(|(cat, count)| Touch { entity: cat.clone(), entity_type: "change-type".to_string(), weight: *count as i64, attrs: BTreeMap::new() })
        .collect();

    let mut attrs = BTreeMap::new();
    attrs.insert("version".to_string(), r.version.clone());
    if let Some(date) = &r.date {
        attrs.insert("date".to_string(), date.clone());
    }
    for (cat, count) in &r.categories {
        attrs.insert(format!("count:{cat}"), count.to_string());
    }

    let toks = tokenize(&format!("{title} {body}"));
    let hash = simhash(&toks);

    Event {
        id,
        source_id: source_id.to_string(),
        kind: "release".to_string(),
        at,
        at_epoch,
        actor: Actor::default(),
        title,
        body,
        level: String::new(),
        attrs,
        touches,
        links: Vec::new(),
        simhash: hash,
        is_bulk: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    const SAMPLE: &str = concat!(
        "## [Unreleased]\n",
        "\n",
        "### Added\n",
        "- soporte para exportar a csv\n",
        "\n",
        "## [1.2.3] - 2024-06-01\n",
        "\n",
        "### Added\n",
        "- nueva funcionalidad de busqueda\n",
        "- soporte para filtros avanzados\n",
        "\n",
        "### Fixed\n",
        "- corrige fuga de memoria en el indexador\n",
    );

    fn events() -> Vec<Event> {
        releases_to_events(&parser::parse(SAMPLE), "changelog:/tmp/CHANGELOG.md")
    }

    #[test]
    fn una_release_es_un_evento_kind_release() {
        let evs = events();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].kind, "release");
        assert_eq!(evs[1].id, "1.2.3");
        assert_eq!(evs[1].title, "release 1.2.3");
    }

    #[test]
    fn touches_una_por_categoria_con_su_conteo() {
        let evs = events();
        let touches = &evs[1].touches;
        assert_eq!(touches.len(), 2);
        assert_eq!(touches[0].entity, "Added");
        assert_eq!(touches[0].entity_type, "change-type");
        assert_eq!(touches[0].weight, 2);
        assert_eq!(touches[1].entity, "Fixed");
        assert_eq!(touches[1].weight, 1);
    }

    #[test]
    fn fecha_produce_at_epoch_medianoche_utc() {
        let evs = events();
        assert_eq!(evs[1].at_epoch, 1_717_200_000); // 2024-06-01T00:00:00Z
        assert_eq!(evs[1].at, "2024-06-01T00:00:00Z");
    }

    #[test]
    fn unreleased_sin_fecha_at_epoch_cero_y_at_vacio() {
        let evs = events();
        assert_eq!(evs[0].id, "Unreleased");
        assert_eq!(evs[0].at_epoch, 0);
        assert_eq!(evs[0].at, "");
    }

    #[test]
    fn versiones_duplicadas_reciben_sufijo_determinista() {
        let content = "## [1.0.0]\n- a\n\n## [1.0.0]\n- b\n";
        let evs = releases_to_events(&parser::parse(content), "changelog:/tmp/x.md");
        assert_eq!(evs[0].id, "1.0.0");
        assert_eq!(evs[1].id, "1.0.0#2");
    }

    #[test]
    fn attrs_incluye_version_fecha_y_conteos() {
        let evs = events();
        let a = &evs[1].attrs;
        assert_eq!(a["version"], "1.2.3");
        assert_eq!(a["date"], "2024-06-01");
        assert_eq!(a["count:Added"], "2");
        assert_eq!(a["count:Fixed"], "1");
    }
}
