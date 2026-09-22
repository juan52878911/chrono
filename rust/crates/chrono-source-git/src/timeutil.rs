//! Conversión epoch UNIX -> ISO-8601 UTC sin dependencias externas.
//!
//! Usa el algoritmo civil de Howard Hinnant (`days_from_civil` / `civil_from_days`),
//! válido para cualquier año proléptico gregoriano, con enteros de 64 bits.

/// Convierte segundos UTC desde epoch a "YYYY-MM-DDThh:mm:ssZ".
pub fn epoch_to_iso8601(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs_of_day = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Días desde 1970-01-01 -> (año, mes, día). Algoritmo civil de Hinnant.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_cero_es_epoch() {
        assert_eq!(epoch_to_iso8601(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn epoch_conocido() {
        // 2024-01-01T00:00:00Z
        assert_eq!(epoch_to_iso8601(1_704_067_200), "2024-01-01T00:00:00Z");
        // 2000-03-01T00:00:00Z (justo tras el bisiesto 2000)
        assert_eq!(epoch_to_iso8601(951_868_800), "2000-03-01T00:00:00Z");
    }

    #[test]
    fn siempre_termina_en_z() {
        assert!(epoch_to_iso8601(1_600_000_000).ends_with('Z'));
    }
}
