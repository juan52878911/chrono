//! S0 · `chrono show <id>`: enseña el diff de un commit acotado por un tope de
//! bytes (token budget), leyendo git EN VIVO. NO indexa nada (respeta el §2 del
//! diseño: no se guardan diffs). Réplica del criterio de `branches` (git live).

use std::path::Path;
use std::process::Command;

use crate::Result;

/// Resultado de `show`: el diff (posiblemente recortado) y metadatos.
#[derive(Debug, Clone, PartialEq)]
pub struct Show {
    /// Id resuelto (sha completo) del objeto mostrado.
    pub id: String,
    /// Texto del `git show` (recortado a `max_bytes` en frontera de línea).
    pub text: String,
    /// `true` si se recortó por el tope de bytes.
    pub truncated: bool,
    /// Bytes totales que git produjo (antes de recortar).
    pub total_bytes: usize,
}

/// `git show <id>` acotado a `max_bytes` (recorte en frontera de línea). Si
/// `entity` es `Some(ruta)`, se limita a esa ruta (`git show <id> -- <ruta>`).
/// Devuelve error si el id no existe.
pub fn show(repo: &Path, id: &str, entity: Option<&str>, max_bytes: usize) -> Result<Show> {
    // Formato estable e independiente del `git config` del usuario.
    let mut args: Vec<String> = vec![
        "-C".into(),
        repo.to_string_lossy().into_owned(),
        "show".into(),
        "--no-color".into(),
        "--format=fuller".into(),
        id.to_string(),
    ];
    if let Some(path) = entity {
        args.push("--".into());
        args.push(path.to_string());
    }
    let out = Command::new("git").args(&args).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git show {id}: {}", err.trim()).into());
    }

    // Sha completo resuelto (para el envelope), independiente del recorte.
    let resolved = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", id])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| id.to_string());

    let total_bytes = out.stdout.len();
    let (text, truncated) = truncate_on_line_boundary(&out.stdout, max_bytes);
    Ok(Show { id: resolved, text, truncated, total_bytes })
}

/// Recorta `bytes` a lo sumo `max_bytes`, en la última frontera de línea `\n`
/// dentro del tope (para no cortar una línea a la mitad). Devuelve el texto
/// (UTF-8 lossy) y si se recortó.
fn truncate_on_line_boundary(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    if bytes.len() <= max_bytes {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let mut cut = max_bytes;
    while cut > 0 && bytes[cut - 1] != b'\n' {
        cut -= 1;
    }
    if cut == 0 {
        cut = max_bytes; // una sola línea larguísima: corta duro en el tope.
    }
    (String::from_utf8_lossy(&bytes[..cut]).into_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorte_en_frontera_de_linea() {
        let data = b"linea1\nlinea2\nlinea3\n";
        let (t, trunc) = truncate_on_line_boundary(data, 10);
        assert_eq!(t, "linea1\n"); // no parte "linea2" a la mitad
        assert!(trunc);
    }

    #[test]
    fn sin_recorte_si_cabe() {
        let data = b"corto\n";
        let (t, trunc) = truncate_on_line_boundary(data, 100);
        assert_eq!(t, "corto\n");
        assert!(!trunc);
    }

    #[test]
    fn linea_unica_mas_larga_que_el_tope_corta_duro() {
        let data = b"unalineamuylargasinsaltos";
        let (t, trunc) = truncate_on_line_boundary(data, 5);
        assert_eq!(t.len(), 5);
        assert!(trunc);
    }
}
