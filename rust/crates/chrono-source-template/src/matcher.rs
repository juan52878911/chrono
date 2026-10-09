//! Matcher de plantillas SIN regex: parsea una plantilla de una línea en
//! segmentos (literales y marcadores `<nombre>`) y casa líneas contra ella de
//! forma determinista.
//!
//! Reglas (decisiones documentadas):
//! - **Marcadores**: `<nombre>`; el nombre es no vacío y sin espacios, `<` ni `>`.
//! - **Escape**: un `<` literal se escribe `<<` (un `>` suelto ya es literal).
//! - **Dos marcadores seguidos** (`<a><b>`) se RECHAZAN al parsear: sin un
//!   literal entre medias no hay dónde cortar la captura de `a`.
//! - **Nombres duplicados** se rechazan (la captura sería ambigua).
//! - **Casado**: izquierda→derecha. Un marcador seguido de un literal captura
//!   hasta la PRÓXIMA aparición de ese literal (no voraz); el último marcador
//!   (sin literal detrás) captura el resto de la línea. Un literal inicial o
//!   intermedio debe aparecer justo en ese punto o la línea NO casa. Si la
//!   plantilla termina en un literal, lo que sobre de la línea tras él se
//!   ignora (los logs suelen traer colas variables).
//! - **Espacios flexibles**: una tira de espacios/tabs en un literal de la
//!   plantilla casa una tira de uno o más espacios/tabs en la línea; el resto
//!   de caracteres del literal se casan exactos.
//! - Cada captura se devuelve sin espacios/tabs en los extremos; puede ser vacía.

use std::collections::BTreeMap;

/// Un trozo de la plantilla.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segmento {
    Literal(String),
    Marcador(String),
}

/// Plantilla ya parseada.
#[derive(Debug, Clone)]
pub struct Plantilla {
    texto: String,
    segmentos: Vec<Segmento>,
}

fn es_blanco(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// Intenta casar el literal `lit` en `linea` a partir de `pos`; devuelve el
/// índice donde termina el casado. Una tira de blancos en `lit` casa una tira
/// (≥1) de blancos en la línea; el resto es exacto.
fn literal_en(lit: &[u8], linea: &[u8], pos: usize) -> Option<usize> {
    let (mut k, mut j) = (0usize, pos);
    while k < lit.len() {
        if es_blanco(lit[k]) {
            while k < lit.len() && es_blanco(lit[k]) {
                k += 1;
            }
            if !linea.get(j).copied().is_some_and(es_blanco) {
                return None;
            }
            while linea.get(j).copied().is_some_and(es_blanco) {
                j += 1;
            }
        } else {
            if linea.get(j) != Some(&lit[k]) {
                return None;
            }
            k += 1;
            j += 1;
        }
    }
    Some(j)
}

fn recorta(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t')
}

impl Plantilla {
    /// Parsea la plantilla. Error (texto legible) si es inválida.
    pub fn parse(texto: &str) -> Result<Self, String> {
        let mut segmentos: Vec<Segmento> = Vec::new();
        let mut lit = String::new();
        let mut it = texto.chars().peekable();
        while let Some(c) = it.next() {
            if c != '<' {
                lit.push(c);
                continue;
            }
            if it.peek() == Some(&'<') {
                it.next();
                lit.push('<'); // escape `<<` -> `<` literal
                continue;
            }
            let mut nombre = String::new();
            let mut cerrado = false;
            for d in it.by_ref() {
                if d == '>' {
                    cerrado = true;
                    break;
                }
                if d == '<' || d.is_whitespace() {
                    return Err(format!("marcador mal formado cerca de \"<{nombre}{d}\" (sin espacios ni '<'; usa '<<' para un '<' literal)"));
                }
                nombre.push(d);
            }
            if !cerrado {
                return Err(format!("marcador sin cerrar: \"<{nombre}\" (usa '<<' para un '<' literal)"));
            }
            if nombre.is_empty() {
                return Err("marcador vacío '<>'".to_string());
            }
            if !lit.is_empty() {
                segmentos.push(Segmento::Literal(std::mem::take(&mut lit)));
            }
            if matches!(segmentos.last(), Some(Segmento::Marcador(_))) {
                return Err(format!("dos marcadores seguidos sin literal entre ellos (antes de <{nombre}>)"));
            }
            if segmentos.iter().any(|s| matches!(s, Segmento::Marcador(n) if *n == nombre)) {
                return Err(format!("marcador duplicado <{nombre}>"));
            }
            segmentos.push(Segmento::Marcador(nombre));
        }
        if !lit.is_empty() {
            segmentos.push(Segmento::Literal(lit));
        }
        if !segmentos.iter().any(|s| matches!(s, Segmento::Marcador(_))) {
            return Err("la plantilla no tiene ningún marcador <nombre>".to_string());
        }
        Ok(Self { texto: texto.to_string(), segmentos })
    }

    /// Texto original de la plantilla (para el manifest).
    pub fn texto(&self) -> &str {
        &self.texto
    }

    /// Nombres de los marcadores, en orden de aparición.
    pub fn nombres(&self) -> Vec<&str> {
        self.segmentos
            .iter()
            .filter_map(|s| match s {
                Segmento::Marcador(n) => Some(n.as_str()),
                Segmento::Literal(_) => None,
            })
            .collect()
    }

    /// Casa `linea`; devuelve las capturas `(nombre, valor)` en orden de
    /// plantilla, o `None` si la línea no casa.
    pub fn casar<'a>(&'a self, linea: &'a str) -> Option<Vec<(&'a str, &'a str)>> {
        let bytes = linea.as_bytes();
        let mut pos = 0usize;
        let mut caps = Vec::new();
        let mut i = 0usize;
        while i < self.segmentos.len() {
            match &self.segmentos[i] {
                Segmento::Literal(l) => {
                    pos = literal_en(l.as_bytes(), bytes, pos)?;
                    i += 1;
                }
                Segmento::Marcador(n) => match self.segmentos.get(i + 1) {
                    None => {
                        caps.push((n.as_str(), recorta(&linea[pos..])));
                        pos = linea.len();
                        i += 1;
                    }
                    Some(Segmento::Literal(l)) => {
                        let lb = l.as_bytes();
                        let (corte, fin) = (pos..=linea.len())
                            .filter(|p| linea.is_char_boundary(*p))
                            .find_map(|p| literal_en(lb, bytes, p).map(|fin| (p, fin)))?;
                        caps.push((n.as_str(), recorta(&linea[pos..corte])));
                        pos = fin;
                        i += 2;
                    }
                    // Imposible: `parse` rechaza dos marcadores seguidos.
                    Some(Segmento::Marcador(_)) => return None,
                },
            }
        }
        let _ = pos; // lo que sobre tras un literal final se ignora.
        Some(caps)
    }
}

/// Atajo puro: parsea `plantilla` y casa `linea`. `None` si la plantilla es
/// inválida o la línea no casa. Útil en tests y para depurar plantillas.
#[allow(dead_code)]
pub fn casar_linea(plantilla: &str, linea: &str) -> Option<BTreeMap<String, String>> {
    let p = Plantilla::parse(plantilla).ok()?;
    let caps = p.casar(linea)?;
    Some(caps.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn casa_basico() {
        let r = casar_linea("<a> <b>: <c>", "2024 INFO: hola mundo").unwrap();
        assert_eq!(r, m(&[("a", "2024"), ("b", "INFO"), ("c", "hola mundo")]));
    }

    #[test]
    fn espacios_flexibles() {
        let r = casar_linea("<a> <b>: <c>", "2024   INFO:\t\thola  mundo").unwrap();
        assert_eq!(r, m(&[("a", "2024"), ("b", "INFO"), ("c", "hola  mundo")]));
    }

    #[test]
    fn literal_exacto_no_flexible() {
        // ":" no es blanco: no puede faltar ni cambiar.
        assert!(casar_linea("<a>: <b>", "x; y").is_none());
    }

    #[test]
    fn literal_intermedio_ausente_no_casa() {
        assert!(casar_linea("<a> <b>: <c>", "2024 INFO hola").is_none());
        assert!(casar_linea("[<a>] <b>", "sin corchetes").is_none());
    }

    #[test]
    fn literal_inicial_debe_estar() {
        assert!(casar_linea("[<a>] <b>", "[x] y").is_some());
        assert!(casar_linea("[<a>] <b>", "x] y").is_none());
    }

    #[test]
    fn escape_doble_menor() {
        let r = casar_linea("<< <a> >> <b>", "< x >> y").unwrap();
        assert_eq!(r, m(&[("a", "x"), ("b", "y")]));
        let r = casar_linea("<<<a>>", "<hola>").unwrap();
        assert_eq!(r, m(&[("a", "hola")]));
    }

    #[test]
    fn no_voraz_corta_en_la_primera_aparicion() {
        let r = casar_linea("<a>|<b>", "x|y|z").unwrap();
        assert_eq!(r, m(&[("a", "x"), ("b", "y|z")]));
    }

    #[test]
    fn cola_tras_literal_final_se_ignora() {
        let r = casar_linea("<a> end", "x end y z").unwrap();
        assert_eq!(r, m(&[("a", "x")]));
    }

    #[test]
    fn plantillas_invalidas() {
        assert!(Plantilla::parse("sin marcadores").is_err());
        assert!(Plantilla::parse("<a><b>").is_err());
        assert!(Plantilla::parse("<a> <a>").is_err());
        assert!(Plantilla::parse("<a").is_err());
        assert!(Plantilla::parse("<> x").is_err());
        assert!(Plantilla::parse("<a b>").is_err());
    }

    #[test]
    fn utf8_seguro() {
        let r = casar_linea("<a> » <b>", "ñandú » café").unwrap();
        assert_eq!(r, m(&[("a", "ñandú"), ("b", "café")]));
    }
}
