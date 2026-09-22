//! Parseo "a mano" (sin regex) del formato Keep a Changelog
//! (https://keepachangelog.com): un documento markdown con secciones de
//! versión encabezadas por `## [1.2.3] - 2024-06-01` o `## [Unreleased]`, y
//! dentro sub-secciones `### Added/Changed/Deprecated/Removed/Fixed/Security`
//! con viñetas `- ...`.
//!
//! No usamos un parser de markdown genérico: el formato es lo bastante
//! regular (línea por línea, prefijos fijos `## `/`### `/`- `) como para
//! bastar con `str::lines()` y comparaciones de prefijo, igual de barato que
//! `chrono-source-csv::parse` para su formato.

/// Una sección de versión ya extraída del documento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Texto tal cual entre corchetes: "1.2.3", "Unreleased"...
    pub version: String,
    /// Fecha cruda "YYYY-MM-DD" del encabezado, si la hay.
    pub date: Option<String>,
    /// Contenido de la sección (todo lo que hay hasta el siguiente `## `),
    /// recortado de espacios en los extremos.
    pub body: String,
    /// Sub-secciones `### <categoria>` presentes, en el orden en que
    /// aparecen, con el número de viñetas `- ...` que contiene cada una.
    pub categories: Vec<(String, u32)>,
}

/// Parsea el documento completo en sus secciones de versión. Cualquier
/// contenido antes de la primera cabecera `## [...]` (título, descripción)
/// se ignora, igual que cualquier `## ` que no case con el patrón de versión
/// (encabezados de otro tipo no rompen el parseo: simplemente no generan
/// sección).
pub fn parse(content: &str) -> Vec<Release> {
    let lines: Vec<&str> = content.lines().collect();
    let mut releases = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            if let Some((version, date)) = parse_version_header(rest) {
                let mut j = i + 1;
                while j < lines.len() && !lines[j].trim_start().starts_with("## ") {
                    j += 1;
                }
                let body_lines = &lines[i + 1..j];
                let categories = parse_categories(body_lines);
                let body = body_lines.join("\n").trim().to_string();
                releases.push(Release { version, date, body, categories });
                i = j;
                continue;
            }
        }
        i += 1;
    }
    releases
}

/// `true` si el documento contiene al menos una cabecera de versión
/// reconocible (`## [...]`). Usado por `detect` para exigir el formato
/// además del nombre de fichero.
pub fn has_version_heading(content: &str) -> bool {
    content
        .lines()
        .any(|l| l.trim_start().strip_prefix("## ").and_then(parse_version_header).is_some())
}

/// Parsea el resto de una línea `## <rest>` como cabecera de versión:
/// `[<version>]` seguido opcionalmente de `- <fecha>` (y cualquier otra cosa
/// tras la fecha, como `[YANKED]`, se ignora). `None` si no empieza por `[`
/// o el corchete no cierra (no es una cabecera de versión).
fn parse_version_header(rest: &str) -> Option<(String, Option<String>)> {
    let rest = rest.trim();
    let rest = rest.strip_prefix('[')?;
    let close = rest.find(']')?;
    let version = rest[..close].trim().to_string();
    if version.is_empty() {
        return None;
    }
    let after = rest[close + 1..].trim();
    let date = after.strip_prefix('-').map(str::trim).and_then(|s| {
        let candidate: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
        is_date_like(&candidate).then_some(candidate)
    });
    Some((version, date))
}

/// `true` si `s` tiene la forma exacta "YYYY-MM-DD" (10 caracteres ASCII).
fn is_date_like(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && s[0..4].bytes().all(|c| c.is_ascii_digit())
        && s[5..7].bytes().all(|c| c.is_ascii_digit())
        && s[8..10].bytes().all(|c| c.is_ascii_digit())
}

/// Extrae las sub-secciones `### <categoria>` de las líneas de una sección
/// de versión, contando las viñetas `- ...` que caen bajo cada una. Una
/// viñeta antes de la primera sub-sección (changelog plano sin categorías)
/// no se cuenta en ninguna parte: no hay `touch` sin categoría que lo
/// justifique (ver contrato del encargo).
fn parse_categories(body_lines: &[&str]) -> Vec<(String, u32)> {
    let mut cats: Vec<(String, u32)> = Vec::new();
    let mut current: Option<usize> = None;
    for line in body_lines {
        let t = line.trim_start();
        if let Some(name) = t.strip_prefix("### ") {
            let name = name.trim();
            current = if name.is_empty() {
                None
            } else {
                cats.push((name.to_string(), 0));
                Some(cats.len() - 1)
            };
            continue;
        }
        if t.starts_with("- ") || t.trim_end() == "-" {
            if let Some(idx) = current {
                cats[idx].1 += 1;
            }
        }
    }
    cats
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "# Changelog\n",
        "\n",
        "Todas las notas de cambios de este proyecto.\n",
        "\n",
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
        "\n",
        "### Security\n",
        "- actualiza dependencia vulnerable\n",
        "\n",
        "## [1.0.0] - 2024-01-01\n",
        "\n",
        "### Added\n",
        "- version inicial\n",
    );

    #[test]
    fn parsea_tres_releases_en_orden() {
        let releases = parse(SAMPLE);
        assert_eq!(releases.len(), 3);
        assert_eq!(releases[0].version, "Unreleased");
        assert_eq!(releases[0].date, None);
        assert_eq!(releases[1].version, "1.2.3");
        assert_eq!(releases[1].date.as_deref(), Some("2024-06-01"));
        assert_eq!(releases[2].version, "1.0.0");
        assert_eq!(releases[2].date.as_deref(), Some("2024-01-01"));
    }

    #[test]
    fn categorias_con_conteo_de_vinetas() {
        let releases = parse(SAMPLE);
        let r = &releases[1];
        assert_eq!(r.categories, vec![("Added".to_string(), 2), ("Fixed".to_string(), 1), ("Security".to_string(), 1)]);
    }

    #[test]
    fn body_contiene_las_subsecciones() {
        let releases = parse(SAMPLE);
        assert!(releases[1].body.contains("nueva funcionalidad de busqueda"));
        assert!(releases[1].body.contains("### Fixed"));
    }

    #[test]
    fn sin_cabeceras_de_version_no_hay_releases() {
        assert!(parse("# Changelog\n\nsolo texto, sin secciones.\n").is_empty());
    }

    #[test]
    fn has_version_heading_distingue_formato() {
        assert!(has_version_heading(SAMPLE));
        assert!(!has_version_heading("# Changelog\n\nsolo texto.\n"));
        assert!(!has_version_heading("## Unreleased\n\nsin corchetes.\n"));
    }

    #[test]
    fn fecha_invalida_se_descarta_pero_version_se_conserva() {
        let releases = parse("## [2.0.0] - no-es-fecha\n\n### Added\n- x\n");
        assert_eq!(releases[0].version, "2.0.0");
        assert_eq!(releases[0].date, None);
    }

    #[test]
    fn yanked_tras_la_fecha_no_rompe_el_parseo() {
        let releases = parse("## [3.0.0] - 2024-03-01 [YANKED]\n\n### Removed\n- x\n");
        assert_eq!(releases[0].date.as_deref(), Some("2024-03-01"));
    }
}
