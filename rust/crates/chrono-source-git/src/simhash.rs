//! SimHash de 64 bits determinista (FNV-1a por token, voto por bit).
//! Réplica del comportamiento de `internal/simhash` en Go, sin dependencias.

/// FNV-1a de 64 bits (offset básico y primo estándar del algoritmo).
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Tokeniza en minúsculas, partiendo por caracteres no alfanuméricos.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Calcula el SimHash de 64 bits de un conjunto de tokens ya extraídos.
pub fn simhash(tokens: &[String]) -> u64 {
    let mut votes = [0i64; 64];
    for t in tokens {
        let h = fnv1a64(t);
        for (i, vote) in votes.iter_mut().enumerate() {
            if (h >> i) & 1 == 1 {
                *vote += 1;
            } else {
                *vote -= 1;
            }
        }
    }
    let mut out: u64 = 0;
    for (i, vote) in votes.iter().enumerate() {
        if *vote > 0 {
            out |= 1 << i;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_parte_por_no_alfanumerico_y_minusculas() {
        assert_eq!(
            tokenize("Fix: bug #123 in Módulo!"),
            vec!["fix", "bug", "123", "in", "módulo"]
        );
    }

    #[test]
    fn simhash_es_determinista() {
        let toks = tokenize("hola mundo\ncuerpo del commit");
        let a = simhash(&toks);
        let b = simhash(&toks);
        assert_eq!(a, b);
        assert_ne!(a, 0);
    }

    #[test]
    fn simhash_vacio_es_cero() {
        assert_eq!(simhash(&[]), 0);
    }
}
