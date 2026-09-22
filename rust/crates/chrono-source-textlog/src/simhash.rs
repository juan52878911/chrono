//! Primitivas hash sin dependencias: FNV-1a de 64 bits y SimHash determinista.
//! Réplica del estilo de `chrono-source-git::simhash`, reutilizada aquí para
//! el SimHash de `title` y para el `id` de respaldo (línea+offset).

/// FNV-1a de 64 bits sobre un slice de bytes (offset básico y primo estándar).
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// FNV-1a de 64 bits sobre (bytes de la línea, offset de inicio en el fichero).
/// Usado como `id` de respaldo cuando la línea no trae `id`/`_id`/`uuid`: el
/// offset evita que líneas de log idénticas repetidas en distintos puntos del
/// fichero colisionen en el mismo id.
pub fn fnv1a64_line(line: &[u8], offset: u64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in line {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for b in offset.to_le_bytes() {
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
    fn fnv1a64_line_depende_del_offset() {
        let a = fnv1a64_line(b"misma linea", 0);
        let b = fnv1a64_line(b"misma linea", 10);
        assert_ne!(a, b);
    }

    #[test]
    fn fnv1a64_line_es_determinista() {
        let a = fnv1a64_line(b"misma linea", 42);
        let b = fnv1a64_line(b"misma linea", 42);
        assert_eq!(a, b);
    }

    #[test]
    fn simhash_vacio_es_cero() {
        assert_eq!(simhash(&[]), 0);
    }
}
