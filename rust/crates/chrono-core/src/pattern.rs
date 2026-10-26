//! Patrones de extracción sin `regex` (P-REL-2): sacan una clave de una ruta o
//! un título, p. ej. `prueba-(\d+)` del nombre de un fichero. Un subconjunto
//! pequeño y predecible:
//!
//! - literales; `.` cualquier carácter; `\d \w \s` y sus negaciones `\D \W \S`;
//!   `\x` es `x` literal (`\.`, `\(`, `\[`…);
//! - clases `[a-z0-9_]` y negadas `[^/]`, con `\d \w \s` dentro;
//! - cuantificadores voraces `* + ?` `{n}` `{n,}` `{n,m}` sobre un átomo;
//! - anclas `^` (al principio) y `$` (al final);
//! - como mucho un grupo `( … )`, sin anidar ni cuantificar: si lo hay, el
//!   valor es lo que captura; si no, la coincidencia entera.
//!
//! Sin alternancia ni referencias hacia atrás. Busca la coincidencia más a la
//! izquierda, con vuelta atrás acotada por el tamaño del texto.

#[derive(Debug, Clone, PartialEq)]
enum Atom {
    Lit(char),
    Any,
    /// Rangos inclusivos y si la clase va negada.
    Class(Vec<(char, char)>, bool),
    Open,
    Close,
}

#[derive(Debug, Clone, PartialEq)]
struct Node {
    atom: Atom,
    min: usize,
    max: usize,
}

/// Un patrón compilado. [`Pattern::parse`] da el error con la posición.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    nodes: Vec<Node>,
    start: bool,
    end: bool,
}

/// Tope de repeticiones de `*`/`+`/`{n,}`: ninguna clave real es más larga.
const MAX_REPEAT: usize = 4096;

fn shorthand(c: char) -> Option<(Vec<(char, char)>, bool)> {
    let digit = vec![('0', '9')];
    let word = vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
    let space = vec![(' ', ' '), ('\t', '\r')];
    Some(match c {
        'd' => (digit, false),
        'D' => (digit, true),
        'w' => (word, false),
        'W' => (word, true),
        's' => (space, false),
        'S' => (space, true),
        _ => return None,
    })
}

impl Pattern {
    pub fn parse(src: &str) -> Result<Pattern, String> {
        let chars: Vec<char> = src.chars().collect();
        let mut nodes: Vec<Node> = Vec::new();
        let (mut start, mut end) = (false, false);
        let (mut groups, mut open) = (0usize, false);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let at = i;
            i += 1;
            let atom = match c {
                '^' if at == 0 => {
                    start = true;
                    continue;
                }
                '$' if i == chars.len() => {
                    end = true;
                    continue;
                }
                '(' => {
                    if open || groups == 1 {
                        return Err(format!("posición {at}: solo se admite un grupo ( … ), sin anidar"));
                    }
                    open = true;
                    groups += 1;
                    Atom::Open
                }
                ')' => {
                    if !open {
                        return Err(format!("posición {at}: ')' sin '('"));
                    }
                    open = false;
                    Atom::Close
                }
                '.' => Atom::Any,
                '\\' => {
                    let Some(&e) = chars.get(i) else { return Err(format!("posición {at}: '\\' al final")) };
                    i += 1;
                    match shorthand(e) {
                        Some((ranges, neg)) => Atom::Class(ranges, neg),
                        None => Atom::Lit(e),
                    }
                }
                '[' => {
                    let mut ranges = Vec::new();
                    let neg = chars.get(i) == Some(&'^');
                    if neg {
                        i += 1;
                    }
                    let mut closed = false;
                    while i < chars.len() {
                        let mut lo = chars[i];
                        i += 1;
                        if lo == ']' && !ranges.is_empty() {
                            closed = true;
                            break;
                        }
                        if lo == '\\' {
                            let Some(&e) = chars.get(i) else { break };
                            i += 1;
                            if let Some((r, false)) = shorthand(e) {
                                ranges.extend(r);
                                continue;
                            }
                            if shorthand(e).is_some() {
                                return Err(format!("posición {}: \\{e} negado dentro de [ ]", i - 2));
                            }
                            lo = e;
                        }
                        if chars.get(i) == Some(&'-') && chars.get(i + 1).is_some_and(|&h| h != ']') {
                            let hi = chars[i + 1];
                            i += 2;
                            if hi < lo {
                                return Err(format!("posición {at}: rango {lo}-{hi} al revés"));
                            }
                            ranges.push((lo, hi));
                        } else {
                            ranges.push((lo, lo));
                        }
                    }
                    if !closed {
                        return Err(format!("posición {at}: '[' sin ']'"));
                    }
                    Atom::Class(ranges, neg)
                }
                '*' | '+' | '?' | '{' => return Err(format!("posición {at}: '{c}' sin nada que repetir")),
                _ => Atom::Lit(c),
            };
            let (min, max) = match chars.get(i) {
                Some('*') => (0, MAX_REPEAT),
                Some('+') => (1, MAX_REPEAT),
                Some('?') => (0, 1),
                Some('{') => {
                    let close = chars[i..].iter().position(|&c| c == '}').map(|p| i + p);
                    let Some(close) = close else { return Err(format!("posición {i}: '{{' sin '}}'")) };
                    let body: String = chars[i + 1..close].iter().collect();
                    let num = |s: &str| s.trim().parse::<usize>().map_err(|_| format!("posición {i}: repetición {{{body}}} no válida"));
                    let r = match body.split_once(',') {
                        None => (num(&body)?, num(&body)?),
                        Some((a, b)) if b.trim().is_empty() => (num(a)?, MAX_REPEAT),
                        Some((a, b)) => (num(a)?, num(b)?),
                    };
                    if r.0 > r.1 || r.1 > MAX_REPEAT {
                        return Err(format!("posición {i}: repetición {{{body}}} no válida"));
                    }
                    i = close;
                    r
                }
                _ => (1, 1),
            };
            if (min, max) != (1, 1) {
                if matches!(atom, Atom::Open | Atom::Close) {
                    return Err(format!("posición {i}: un grupo no se puede repetir"));
                }
                i += 1;
            }
            nodes.push(Node { atom, min, max });
        }
        if open {
            return Err("'(' sin ')'".to_string());
        }
        Ok(Pattern { nodes, start, end })
    }

    /// Valor de la primera coincidencia (el grupo si lo hay), o `None`.
    pub fn find<'t>(&self, text: &'t str) -> Option<&'t str> {
        self.find_all(text).into_iter().next()
    }

    /// Valores de las coincidencias sin solaparse, de izquierda a derecha.
    /// Las vacías no cuentan.
    pub fn find_all<'t>(&self, text: &'t str) -> Vec<&'t str> {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let byte = |p: usize| chars.get(p).map_or(text.len(), |c| c.0);
        let mut out = Vec::new();
        let mut from = 0;
        while from <= chars.len() {
            let mut hit = None;
            for s in from..=chars.len() {
                let mut caps = (None, None);
                if let Some(e) = self.at(&chars, 0, s, &mut caps) {
                    hit = Some((s, e, caps));
                    break;
                }
                if self.start {
                    break;
                }
            }
            let Some((s, e, caps)) = hit else { break };
            let (a, b) = match caps {
                (Some(a), Some(b)) => (a, b),
                _ => (s, e),
            };
            if b > a {
                out.push(&text[byte(a)..byte(b)]);
            }
            if self.start {
                break;
            }
            from = if e > s { e } else { s + 1 };
        }
        out
    }

    fn at(&self, text: &[(usize, char)], ni: usize, pos: usize, caps: &mut (Option<usize>, Option<usize>)) -> Option<usize> {
        let Some(node) = self.nodes.get(ni) else {
            return (!self.end || pos == text.len()).then_some(pos);
        };
        match &node.atom {
            Atom::Open => {
                let prev = caps.0.replace(pos);
                let r = self.at(text, ni + 1, pos, caps);
                if r.is_none() {
                    caps.0 = prev;
                }
                r
            }
            Atom::Close => {
                let prev = caps.1.replace(pos);
                let r = self.at(text, ni + 1, pos, caps);
                if r.is_none() {
                    caps.1 = prev;
                }
                r
            }
            atom => {
                let mut n = 0;
                while n < node.max && pos + n < text.len() && matches(atom, text[pos + n].1) {
                    n += 1;
                }
                loop {
                    if n < node.min {
                        return None;
                    }
                    if let Some(e) = self.at(text, ni + 1, pos + n, caps) {
                        return Some(e);
                    }
                    if n == 0 {
                        return None;
                    }
                    n -= 1;
                }
            }
        }
    }
}

fn matches(atom: &Atom, c: char) -> bool {
    match atom {
        Atom::Lit(l) => *l == c,
        Atom::Any => true,
        Atom::Class(ranges, neg) => ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi) != *neg,
        Atom::Open | Atom::Close => false,
    }
}

#[cfg(test)]
mod tests {
    use super::Pattern;

    fn find(p: &str, t: &str) -> Option<String> {
        Pattern::parse(p).unwrap().find(t).map(str::to_string)
    }

    #[test]
    fn grupo_y_coincidencia_entera() {
        assert_eq!(find(r"prueba-(\d+)", "lote/prueba-0042_ok.csv").as_deref(), Some("0042"));
        assert_eq!(find(r"prueba-\d+", "x prueba-7 y").as_deref(), Some("prueba-7"));
        assert_eq!(find(r"prueba-(\d+)", "sin numero prueba-x"), None);
    }

    #[test]
    fn clases_rangos_y_negacion() {
        assert_eq!(find(r"\[[^ \]]+ ([0-9a-f]{7,40})\]", "[main 1a2b3c4] fix").as_deref(), Some("1a2b3c4"));
        assert_eq!(find(r"([A-Z]+-\d+)", "arregla ABC-123 y DEF-4").as_deref(), Some("ABC-123"));
        assert_eq!(find(r"/([^/]+)/[^/]+$", "a/b/c/d.txt").as_deref(), Some("c"));
        assert_eq!(find(r"[\w.-]+@x", "yo.mismo-1@x").as_deref(), Some("yo.mismo-1@x"));
    }

    #[test]
    fn anclas_cuantificadores_y_vuelta_atras() {
        assert_eq!(find(r"^(\w+)", "abc def").as_deref(), Some("abc"));
        assert_eq!(find(r"^def", "abc def"), None);
        assert_eq!(find(r"(\d+)$", "a1 b22").as_deref(), Some("22"));
        assert_eq!(find(r"a.*(b)", "aXbYb").as_deref(), Some("b"));
        assert_eq!(find(r"colou?r", "color").as_deref(), Some("color"));
        assert_eq!(find(r"x\.y", "xzy x.y").as_deref(), Some("x.y"));
        assert_eq!(find(r"ñ(\d{2})", "año ñ12").as_deref(), Some("12"));
    }

    #[test]
    fn todas_las_coincidencias() {
        let p = Pattern::parse(r"#(\d+)").unwrap();
        assert_eq!(p.find_all("cierra #12 y #345, no #"), vec!["12", "345"]);
    }

    #[test]
    fn errores_con_posicion() {
        for bad in ["(a(b))", "(a)(b)", "a)", "(ab", "[ab", "*a", "(a)+", "a{3,1}", "a\\"] {
            assert!(Pattern::parse(bad).is_err(), "{bad} debería fallar");
        }
    }
}
