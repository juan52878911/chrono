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
    format!("{lead}{}{trail}", normalize_core(core))
}

/// El cuerpo entero si es una sola cosa reconocible; si no, por segmentos.
///
/// Los tokens compuestos son lo normal en logs reales: `C-0x7f27fb262780:`
/// (PgBouncer), `crm_user@172.18.0.26:40362`, `/api/x/550e8400-…`. Clasificar
/// solo el token entero dejaba el hex de dentro como tiras de `<NUM>` entre
/// letras sueltas, y cada id producía una plantilla distinta.
fn normalize_core(core: &str) -> String {
    if let Some(m) = classify_whole(core) {
        return m.to_string();
    }
    // Nivel 1: separadores "estructurales" (`/ = @ : ,`). Cada segmento puede
    // ser entero un uuid, una ip o un hex (`…/<UUID>`, `@<IP>:`).
    let mut out = String::with_capacity(core.len());
    let mut seg_start = 0;
    for (i, c) in core.char_indices() {
        if matches!(c, '/' | '=' | '@' | ':' | ',') {
            out.push_str(&normalize_segment(&core[seg_start..i]));
            out.push(c);
            seg_start = i + c.len_utf8();
        }
    }
    out.push_str(&normalize_segment(&core[seg_start..]));
    out
}

/// Un segmento sin separadores estructurales: entero si se reconoce; si no,
/// trozo a trozo por el resto de separadores (`-`, `.`, …).
fn normalize_segment(seg: &str) -> String {
    if seg.is_empty() {
        return String::new();
    }
    if let Some(m) = classify_whole(seg) {
        return m.to_string();
    }
    let mut out = String::with_capacity(seg.len());
    let mut run_start: Option<usize> = None;
    for (i, c) in seg.char_indices() {
        let word = c.is_alphanumeric() || c == '_';
        match (word, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(s)) => {
                out.push_str(&normalize_run(&seg[s..i]));
                out.push(c);
                run_start = None;
            }
            (false, None) => out.push(c),
            (true, Some(_)) => {}
        }
    }
    if let Some(s) = run_start {
        out.push_str(&normalize_run(&seg[s..]));
    }
    out
}

/// Una tira de caracteres de palabra: hex (`0x…` o ≥6 con letra y dígito) o
/// sus dígitos a `<NUM>`.
fn normalize_run(run: &str) -> String {
    if is_hex_number(run) {
        "<HEX>".to_string()
    } else {
        replace_digit_runs(run)
    }
}

/// Marcador si el texto ENTERO es una sola cosa variable reconocible.
fn classify_whole(s: &str) -> Option<&'static str> {
    if is_uuid(s) {
        Some("<UUID>")
    } else if is_ipv4(s) {
        Some("<IP>")
    } else if is_timestamp(s) {
        Some("<TS>")
    } else if is_hex_number(s) {
        Some("<HEX>")
    } else {
        None
    }
}

/// Fecha `AAAA-MM-DD`, hora `HH:MM:SS[.fff]`, o ambas juntas (`T` o nada
/// entre ellas, como en ISO-8601), con `Z` u offset (`+0000`, `+02:00`)
/// opcionales al final. Es la cabecera de casi cualquier log: sin esto,
/// `2026-10-14 20:36:31.675 UTC` ocupaba siete `<NUM>` en cada plantilla.
fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |from: usize, n: usize| b.len() >= from + n && b[from..from + n].iter().all(u8::is_ascii_digit);
    let is_date = |at: usize| digits(at, 4) && b.get(at + 4) == Some(&b'-') && digits(at + 5, 2) && b.get(at + 7) == Some(&b'-') && digits(at + 8, 2);
    let is_time = |at: usize| digits(at, 2) && b.get(at + 2) == Some(&b':') && digits(at + 3, 2) && b.get(at + 5) == Some(&b':') && digits(at + 6, 2);
    // Fracción de segundo y zona horaria opcionales, desde `at`; true si
    // consumen el resto del texto.
    let tail_ok = |mut at: usize| {
        if matches!(b.get(at), Some(b'.') | Some(b',')) {
            at += 1;
            let start = at;
            while b.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            if at == start {
                return false;
            }
        }
        match b.get(at) {
            None => true,
            Some(b'Z') | Some(b'z') => at + 1 == b.len(),
            Some(b'+') | Some(b'-') => {
                let rest = &s[at + 1..];
                let compact = rest.len() == 4 && rest.bytes().all(|c| c.is_ascii_digit());
                let colon = rest.len() == 5 && rest.as_bytes()[2] == b':' && rest.bytes().filter(|c| c.is_ascii_digit()).count() == 4;
                compact || colon || (rest.len() == 2 && rest.bytes().all(|c| c.is_ascii_digit()))
            }
            _ => false,
        }
    };
    if is_date(0) {
        if b.len() == 10 {
            return true;
        }
        return matches!(b.get(10), Some(b'T') | Some(b't')) && is_time(11) && tail_ok(19);
    }
    is_time(0) && tail_ok(8)
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
/// menos una letra a-f Y un dígito. La letra evita marcar como HEX un entero
/// decimal largo (va a `<NUM>`); el dígito, marcar palabras que solo usan
/// letras a-f (`facade`, `decade`, `accede`).
fn is_hex_number(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit());
    }
    s.len() >= 6
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && s.bytes().any(|b| b.is_ascii_alphabetic())
        && s.bytes().any(|b| b.is_ascii_digit())
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

    #[test]
    fn hex_dentro_de_token_compuesto() {
        // PgBouncer: el id de cliente cambia en cada línea; antes cada uno era
        // una plantilla distinta (`C-<NUM>x<NUM>f<NUM>fb<NUM>:`).
        let a = templatize("LOG C-0x7f27fb262780: crm_db/crm_user@172.18.0.26:40362 login attempt");
        let b = templatize("LOG C-0x7f27fb261f40: crm_db/crm_user@127.0.0.1:50502 login attempt");
        assert_eq!(a, "LOG C-<HEX>: crm_db/crm_user@<IP>:<NUM> login attempt");
        assert_eq!(a, b);
    }

    #[test]
    fn uuid_dentro_de_ruta() {
        assert_eq!(
            templatize("GET /api/prospectos/550e8400-e29b-41d4-a716-446655440000 200"),
            "GET /api/prospectos/<UUID> <NUM>"
        );
    }

    #[test]
    fn marcas_de_tiempo() {
        assert_eq!(templatize("2026-10-14 20:36:31.675 UTC [66] ERROR: x"), "<TS> <TS> UTC [<NUM>] ERROR: x");
        assert_eq!(templatize("at 2026-10-14T20:36:31.675Z done"), "at <TS> done");
        assert_eq!(templatize("at 2026-10-14T20:36:31+0000 done"), "at <TS> done");
        assert_eq!(templatize("at 2026-10-14T20:36:31+02:00 done"), "at <TS> done");
        // No es una hora: se queda en números.
        assert_eq!(templatize("ratio 12:34"), "ratio <NUM>:<NUM>");
    }

    #[test]
    fn palabras_de_letras_hex_no_son_hex() {
        assert_eq!(templatize("facade decade accede"), "facade decade accede");
        assert_eq!(templatize("sha f27eb09f merged"), "sha <HEX> merged");
    }
}
