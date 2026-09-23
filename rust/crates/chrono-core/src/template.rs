//! Normalización "Drain-light" de un mensaje a una PLANTILLA determinista:
//! se sustituyen las partes variables (uuids, ips, hex, números) por
//! marcadores (`<UUID>`, `<IP>`, `<HEX>`, `<NUM>`), de modo que miles de líneas
//! de log que solo difieren en sus valores caen en la misma plantilla. Es la
//! base de `templates`/`rollups` (ver `docs/DESIGN-GENERAL-CORE.md §6`).
//!
//! Sin dependencias (chrono-core es dominio puro): tokenización a mano en vez
//! de `regex`. Determinista: misma entrada → misma plantilla, byte a byte.

/// Convierte un mensaje en su plantilla. Trocea por espacios; cada token se
/// clasifica como un todo (uuid/ip/hex) o, si no, se le sustituyen las tiras de
/// dígitos internas por `<NUM>` (para rutas tipo `/users/12345` o `id_42`).
/// Colapsa espacios en blanco a uno solo.
pub fn templatize(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut first = true;
    for tok in msg.split_whitespace() {
        if !first {
            out.push(' ');
        }
        first = false;
        out.push_str(&normalize_token(tok));
    }
    out
}

/// Clasifica/normaliza un token ya sin espacios.
fn normalize_token(tok: &str) -> String {
    // Se separa la puntuación de los bordes (comillas, paréntesis, comas…)
    // para clasificar el "cuerpo", y se recompone: así `"deadbeef",` conserva
    // sus bordes pero el cuerpo se marca como `<HEX>`.
    let (lead, core, trail) = split_edges(tok);
    let replaced = if is_uuid(core) {
        "<UUID>".to_string()
    } else if is_ipv4(core) {
        "<IP>".to_string()
    } else if is_hex_number(core) {
        "<HEX>".to_string()
    } else {
        replace_digit_runs(core)
    };
    format!("{lead}{replaced}{trail}")
}

/// Separa la puntuación no alfanumérica de los bordes del token.
fn split_edges(tok: &str) -> (&str, &str, &str) {
    let is_edge = |c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == ':' || c == '/');
    let start = tok.find(|c: char| !is_edge(c)).unwrap_or(tok.len());
    let end = tok.rfind(|c: char| !is_edge(c)).map(|i| i + tok[i..].chars().next().unwrap().len_utf8()).unwrap_or(start);
    (&tok[..start], &tok[start..end], &tok[end..])
}

/// UUID canónico 8-4-4-4-12 (hex, con guiones), insensible a mayúsculas.
fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let lens = [8usize, 4, 4, 4, 12];
    parts.len() == 5
        && parts.iter().zip(lens).all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// IPv4 `a.b.c.d` (con octetos 0-255), opcionalmente con `:puerto`.
fn is_ipv4(s: &str) -> bool {
    let host = s.split(':').next().unwrap_or(s);
    let octets: Vec<&str> = host.split('.').collect();
    if octets.len() != 4 {
        return false;
    }
    octets.iter().all(|o| !o.is_empty() && o.bytes().all(|b| b.is_ascii_digit()) && o.parse::<u16>().map(|n| n <= 255).unwrap_or(false))
}

/// Número hex: `0x…` con dígitos hex, o una tira de ≥6 hex que incluya al
/// menos una letra a-f (para no marcar como HEX un entero decimal largo, que
/// va a `<NUM>`).
fn is_hex_number(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit());
    }
    s.len() >= 6
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && s.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Sustituye cada tira maximal de dígitos por `<NUM>` (deja el resto intacto).
fn replace_digit_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_digits = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push_str("<NUM>");
                in_digits = true;
            }
        } else {
            out.push(c);
            in_digits = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeros_y_rutas() {
        assert_eq!(templatize("GET /api/users/12345 took 42ms"), "GET /api/users/<NUM> took <NUM>ms");
        assert_eq!(templatize("retry 3 of 5"), "retry <NUM> of <NUM>");
    }

    #[test]
    fn uuid_ip_hex() {
        assert_eq!(
            templatize("req 550e8400-e29b-41d4-a716-446655440000 from 10.0.0.1:8080"),
            "req <UUID> from <IP>"
        );
        assert_eq!(templatize("addr 0xdeadbeef and hash cafebabe1234"), "addr <HEX> and hash <HEX>");
    }

    #[test]
    fn puntuacion_de_bordes_se_conserva() {
        assert_eq!(templatize("id=\"42\", ok"), "id=\"<NUM>\", ok");
        assert_eq!(templatize("(port 8080)"), "(port <NUM>)");
    }

    #[test]
    fn decimal_largo_es_num_no_hex() {
        assert_eq!(templatize("count 1234567"), "count <NUM>");
    }

    #[test]
    fn determinista_y_colapsa_espacios() {
        assert_eq!(templatize("a   b\t c"), "a b c");
        assert_eq!(templatize("mismo 1"), templatize("mismo 2"));
    }

    #[test]
    fn sin_variables_queda_igual() {
        assert_eq!(templatize("connection pool exhausted"), "connection pool exhausted");
    }
}
