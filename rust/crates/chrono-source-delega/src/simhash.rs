//! Primitivas hash sin dependencias: FNV-1a de 64 bits y SimHash determinista.
//! Duplicadas a propósito de `chrono-source-jsonl` (cada adaptador es un crate
//! independiente, ver `docs/DECISIONS.md §15`).

/// FNV-1a de 64 bits sobre un slice de bytes.
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

/// SimHash de 64 bits de un conjunto de tokens (voto por bit).
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

/// Convierte MILISEGUNDOS UTC desde epoch a "YYYY-MM-DDThh:mm:ss[.mmm]Z" (la
/// parte de ms solo si no es cero), con el algoritmo civil de Hinnant, el
/// mismo que el resto de adaptadores. `at_epoch` es ms en todo el dominio.
pub fn epoch_to_iso8601(epoch_ms: i64) -> String {
    let days = epoch_ms.div_euclid(86_400_000);
    let ms_of_day = epoch_ms.rem_euclid(86_400_000);
    let (secs_of_day, millis) = (ms_of_day / 1000, ms_of_day % 1000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    let (hh, mm, ss) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    if millis == 0 {
        format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
    } else {
        format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_de_epoch_conocido() {
        assert_eq!(epoch_to_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(epoch_to_iso8601(1_791_840_778_000), "2026-10-12T21:32:58Z");
        assert_eq!(epoch_to_iso8601(1_791_840_778_643), "2026-10-12T21:32:58.643Z");
    }

    #[test]
    fn simhash_vacio_es_cero() {
        assert_eq!(simhash(&[]), 0);
    }
}
