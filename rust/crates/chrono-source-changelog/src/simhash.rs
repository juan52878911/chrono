//! Primitivas hash sin dependencias: FNV-1a de 64 bits y SimHash determinista.
//! Réplica del estilo de `chrono-source-git::simhash`, reutilizada aquí para
//! el watermark de contenido (`fnv1a64` del fichero completo) y el SimHash
//! de `title`+`body`.

/// FNV-1a de 64 bits sobre un slice de bytes (offset básico y primo estándar).
/// Usado tanto para el watermark de contenido (hash del fichero completo,
/// ver `cursor::content_watermark`) como para el SimHash de tokens.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
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

/// SimHash de 64 bits de un conjunto de tokens ya extraídos (voto por bit).
pub fn simhash(tokens: &[String]) -> u64 {
    let mut votes = [0i64; 64];
    for t in tokens {
        let h = fnv1a64(t.as_bytes());
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
    fn fnv1a64_es_determinista_y_depende_del_contenido() {
        let a = fnv1a64(b"contenido a");
        let b = fnv1a64(b"contenido a");
        let c = fnv1a64(b"contenido b");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn simhash_vacio_es_cero() {
        assert_eq!(simhash(&[]), 0);
    }
}
