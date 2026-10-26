//! Redacción de secretos antes de indexar: el índice es un fichero portable y
//! los logs y salidas de comandos traen tokens, contraseñas en URLs y cabeceras
//! `Authorization`. Lo que casa se cambia por `<SECRET>` (y así las plantillas
//! siguen agrupando: `Bearer <SECRET>` es una sola).
//!
//! Sin dependencias (como `template`): detectores a mano, por token. Se prefiere
//! no redactar a redactar de más: cada regla exige una forma reconocible
//! (prefijo de proveedor, JWT con sus tres partes, `clave=valor` con un nombre
//! de clave sensible) y una longitud mínima del valor.

/// Lo que se pone en lugar del secreto.
pub const MARK: &str = "<SECRET>";

/// Prefijos de tokens de proveedores, con la longitud mínima del resto.
const PREFIXES: &[(&str, usize)] = &[
    ("ghp_", 30), ("gho_", 30), ("ghu_", 30), ("ghs_", 30), ("ghr_", 30), ("github_pat_", 40),
    ("glpat-", 20), ("xoxb-", 20), ("xoxp-", 20), ("xoxa-", 20), ("xoxs-", 20), ("xapp-", 20),
    ("sk-ant-", 30), ("sk-proj-", 30), ("sk-", 32), ("AKIA", 16), ("ASIA", 16), ("AIza", 35),
    ("ya29.", 30), ("npm_", 30), ("pypi-", 40), ("hf_", 30),
];

/// Nombres de clave cuyo valor es secreto en `clave=valor`, `clave: valor` o
/// `"clave":"valor"` (se comparan en minúsculas y como subcadena).
const KEYS: &[&str] = &[
    "password", "passwd", "pwd", "secret", "token", "api_key", "apikey", "api-key", "access_key",
    "private_key", "client_secret", "authorization", "auth_token", "cookie",
];

/// Redacta `s`; devuelve el texto y cuántos secretos quitó.
pub fn redact(s: &str) -> (String, usize) {
    // Camino rápido sin reservar memoria: casi ninguna línea de log tiene nada
    // con forma de secreto (medido: el análisis completo de cada línea subía
    // la ingesta de 1M eventos un 23 %).
    if !maybe_secret(s) {
        return (s.to_string(), 0);
    }
    let mut n = 0;
    let s = redact_pem(s, &mut n);
    let mut out = String::with_capacity(s.len());
    let mut after_bearer = false;
    // Una clave sensible sin valor (`password:`, `"token":`): el valor es el
    // trozo siguiente (`password: x`, JSON con formato, YAML).
    let mut after_key = false;
    // Se recorre por trozos separados por espacio, conservando los separadores.
    for (i, piece) in s.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if piece.is_empty() {
            continue;
        }
        if std::mem::take(&mut after_key) {
            if let Some(r) = redact_spaced_value(piece) {
                out.push_str(&r);
                n += 1;
                continue;
            }
        }
        if after_bearer && piece.len() >= 16 && is_token_chars(trim_edges(piece)) {
            let t = trim_edges(piece);
            out.push_str(&piece.replacen(t, MARK, 1));
            n += 1;
            after_bearer = false;
            continue;
        }
        let t = trim_edges(piece);
        after_bearer = t.eq_ignore_ascii_case("bearer") || t.eq_ignore_ascii_case("basic");
        after_key = key_without_value(piece);
        out.push_str(&redact_piece(piece, &mut n));
    }
    (out, n)
}

/// ¿Puede haber algo que redactar? Una pasada por los bytes, sin reservar
/// memoria, que solo mira los inicios de token; sin falsos negativos respecto
/// de `redact`: cada regla necesita una de estas marcas (prefijo de proveedor o
/// `eyJ` al empezar un token, `bearer`/`basic`, `://`, `-----BEGIN `, o un
/// separador `=`/`:` con un nombre de clave sensible). Nada de menos de 9 bytes
/// puede llevar un secreto (`a://u:p@h` es lo más corto que se redacta).
fn maybe_secret(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 9 {
        return false;
    }
    let mut sep = false;
    let mut prev_tok = false;
    for i in 0..b.len() {
        let c = b[i];
        if c == b':' || c == b'=' {
            sep = true;
            if c == b':' && b[i + 1..].starts_with(b"//") {
                return true;
            }
        }
        if c == b'-' && b[i..].starts_with(b"-----BEGIN ") {
            return true;
        }
        let tok = c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.');
        if tok && !prev_tok {
            let rest = &b[i..];
            let ci = |w: &[u8]| rest.len() >= w.len() && rest[..w.len()].eq_ignore_ascii_case(w);
            if rest.starts_with(b"eyJ") || ci(b"bearer") || ci(b"basic") || PREFIXES.iter().any(|(p, _)| rest.starts_with(p.as_bytes())) {
                return true;
            }
        }
        prev_tok = tok;
    }
    sep && KEYS.iter().any(|k| contains_ci(b, k.as_bytes()))
}

/// Subcadena sin distinguir mayúsculas ASCII, sin reservar memoria.
fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

/// ¿El nombre de clave (de un atributo o un campo JSON) delata un secreto?
pub fn sensitive_key(k: &str) -> bool {
    KEYS.iter().any(|s| contains_ci(k.as_bytes(), s.as_bytes()))
}

/// Valor de una clave sensible que sí se redacta: largo y no solo cifras.
pub fn sensitive_value(v: &str) -> bool {
    v.len() >= 8 && v != MARK && !v.bytes().all(|b| b.is_ascii_digit())
}

/// Redacta un evento en su sitio: título, cuerpo, atributos (entero si la clave
/// es sensible) y destinos de los enlaces. Devuelve cuántos secretos quitó.
pub fn redact_event(ev: &mut crate::Event) -> usize {
    fn fix(s: &mut String) -> usize {
        if !maybe_secret(s) {
            return 0;
        }
        let (r, k) = redact(s);
        if k > 0 {
            *s = r;
        }
        k
    }
    let mut n = fix(&mut ev.title) + fix(&mut ev.body);
    for l in &mut ev.links {
        n += fix(&mut l.target);
    }
    for (k, v) in &mut ev.attrs {
        if sensitive_value(v) && sensitive_key(k) {
            *v = MARK.to_string();
            n += 1;
        } else {
            n += fix(v);
        }
    }
    n
}

/// Un trozo sin espacios: credenciales en URL, `clave=valor` sensible, o
/// subtokens con forma de secreto (prefijo de proveedor o JWT).
fn redact_piece(piece: &str, n: &mut usize) -> String {
    let redone = redact_url_password(piece).or_else(|| redact_key_value(piece));
    if redone.is_some() {
        *n += 1;
    }
    // El resto del trozo puede llevar otro secreto (`ghp_…=valor`).
    let piece = redone.as_deref().unwrap_or(piece);
    // Subtokens: tiras de caracteres de token separadas por cualquier otra cosa.
    let mut out = String::with_capacity(piece.len());
    let mut start: Option<usize> = None;
    let flush = |out: &mut String, tok: &str, n: &mut usize| {
        if looks_secret(tok) {
            out.push_str(MARK);
            *n += 1;
        } else {
            out.push_str(tok);
        }
    };
    for (i, c) in piece.char_indices() {
        let tokc = c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.');
        match (tokc, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                flush(&mut out, &piece[s..i], n);
                out.push(c);
                start = None;
            }
            (false, None) => out.push(c),
            (true, Some(_)) => {}
        }
    }
    if let Some(s) = start {
        flush(&mut out, &piece[s..], n);
    }
    out
}

fn looks_secret(tok: &str) -> bool {
    let tok = tok.trim_end_matches('.');
    PREFIXES.iter().any(|(p, min)| {
        tok.strip_prefix(p).is_some_and(|rest| rest.len() >= *min && is_token_chars(rest) && has_mixed(rest))
    }) || is_jwt(tok)
}

/// `eyJ…` en tres partes base64url separadas por punto.
fn is_jwt(tok: &str) -> bool {
    let parts: Vec<&str> = tok.split('.').collect();
    tok.starts_with("eyJ")
        && parts.len() == 3
        && parts[0].len() >= 10
        && parts[1].len() >= 10
        && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
}

/// `esquema://usuario:CLAVE@host` → `esquema://usuario:<SECRET>@host`.
fn redact_url_password(piece: &str) -> Option<String> {
    let scheme_end = piece.find("://")? + 3;
    let rest = &piece[scheme_end..];
    // El host va tras la ÚLTIMA `@` de la autoridad: una clave con `@`
    // (`u:p@ss@host`) dejaba la cola a la vista.
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let at = authority.rfind('@')?;
    let userinfo = &rest[..at];
    let colon = userinfo.find(':')?;
    let password = &userinfo[colon + 1..];
    if password.is_empty() || password == MARK {
        return None;
    }
    Some(format!("{}{}:{MARK}{}", &piece[..scheme_end], &userinfo[..colon], &rest[at..]))
}

/// `password=xxxx`, `"token":"xxxx"`, `api_key:xxxx` con un valor de ≥ 8
/// caracteres: se cambia solo el valor.
fn redact_key_value(piece: &str) -> Option<String> {
    let sep = piece.find(['=', ':'])?;
    let key = piece[..sep].trim_matches(|c: char| c == '"' || c == '\'' || c == '{' || c == ',').to_ascii_lowercase().replace("<secret>", "");
    if key.is_empty() || key.len() > 40 || !KEYS.iter().any(|k| key.contains(k)) {
        return None;
    }
    let raw = &piece[sep + 1..];
    let value = raw.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == '}' || c == ';');
    // Ni marcadores ya redactados, ni URLs, ni rutas (`PWD=/home/x`), ni cifras (`tokens=12345678`).
    if value.len() < 8
        || value.starts_with(['<', '/', '~'])
        || value.contains("://")
        || value.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let start = sep + 1 + raw.find(value)?;
    Some(format!("{}{MARK}{}", &piece[..start], &piece[start + value.len()..]))
}

/// `password:`, `"token":`, `api_key=`: una clave sensible cuyo valor viene
/// en el trozo siguiente.
fn key_without_value(piece: &str) -> bool {
    let p = piece.trim_end_matches(['"', '\'']);
    let Some(key) = p.strip_suffix(':').or_else(|| p.strip_suffix('=')) else { return false };
    let key = key.trim_matches(|c: char| c == '"' || c == '\'' || c == '{' || c == ',').to_ascii_lowercase().replace("<secret>", "");
    !key.is_empty() && key.len() <= 40 && !key.contains([':', '=']) && KEYS.iter().any(|k| key.contains(k))
}

/// El valor tras `clave: ` (con espacio). Entre comillas se redacta como en
/// `clave=valor`; sin comillas, además no puede ser una palabra corriente
/// (`password: required`, `token: invalid signature`).
fn redact_spaced_value(piece: &str) -> Option<String> {
    let value = piece.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == '}' || c == ';');
    let quoted = piece.starts_with(['"', '\'']);
    if value.len() < 8
        || value.starts_with(['<', '/', '~'])
        || value.contains("://")
        || value.bytes().all(|b| b.is_ascii_digit())
        || (!quoted && value.chars().all(|c| c.is_alphabetic() || c == '_' || c == '-'))
    {
        return None;
    }
    let start = piece.find(value)?;
    Some(format!("{}{MARK}{}", &piece[..start], &piece[start + value.len()..]))
}

/// Bloques PEM de clave privada enteros.
fn redact_pem(s: &str, n: &mut usize) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(b) = rest.find("-----BEGIN ") {
        let head_end = rest[b..].find("-----\n").or_else(|| rest[b + 11..].find("-----").map(|i| i + 11));
        let Some(he) = head_end else { break };
        let header = &rest[b..b + he];
        if !header.contains("PRIVATE KEY") {
            out.push_str(&rest[..b + he]);
            rest = &rest[b + he..];
            continue;
        }
        let Some(e) = rest[b..].find("-----END ") else {
            // Sin cierre (línea cortada): de la cabecera al final.
            out.push_str(&rest[..b]);
            out.push_str(MARK);
            *n += 1;
            rest = "";
            break;
        };
        let end = rest[b + e + 9..].find("-----").map(|i| b + e + 9 + i + 5).unwrap_or(rest.len());
        out.push_str(&rest[..b]);
        out.push_str(MARK);
        *n += 1;
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn trim_edges(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'))
}

fn is_token_chars(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'=' | b'+' | b'/'))
}

/// Letras y dígitos: los tokens reales los mezclan; `sk-the-quick-brown-fox` no.
fn has_mixed(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_digit()) && s.bytes().any(|b| b.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> String {
        redact(s).0
    }

    #[test]
    fn tokens_de_proveedor() {
        assert_eq!(r("push with ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8 ok"), "push with <SECRET> ok");
        assert_eq!(r("key=AKIAIOSFODNN7EXAMPLE"), "key=<SECRET>");
        assert_eq!(r("using sk-ant-api03-Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0Uv"), "using <SECRET>");
    }

    #[test]
    fn bearer_jwt_y_url() {
        assert_eq!(r("Authorization: Bearer abcdef0123456789ABCDEF"), "Authorization: Bearer <SECRET>");
        assert_eq!(
            r("tok eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U end"),
            "tok <SECRET> end"
        );
        assert_eq!(r("dsn postgres://app:s3cr3tpass@db:5432/x"), "dsn postgres://app:<SECRET>@db:5432/x");
    }

    #[test]
    fn clave_valor() {
        assert_eq!(r("login password=hunter2hunter2 user=x"), "login password=<SECRET> user=x");
        assert_eq!(r(r#"{"client_secret":"abc123def456"}"#), r#"{"client_secret":"<SECRET>"}"#);
        let (_, n) = redact("password=hunter2hunter2 token=abcdefgh1234");
        assert_eq!(n, 2);
    }

    #[test]
    fn pem() {
        let s = "k -----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY----- fin";
        assert_eq!(r(s), "k <SECRET> fin");
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(r(cert), cert);
    }

    #[test]
    fn evento_entero() {
        let mut ev = crate::Event { title: "login password=hunter2hunter2".into(), ..Default::default() };
        ev.attrs.insert("api_token".into(), "abc123abc123".into());
        ev.attrs.insert("dsn".into(), "mysql://u:pw123456@h/db".into());
        ev.attrs.insert("tokens".into(), "123456789".into());
        assert_eq!(redact_event(&mut ev), 3);
        assert_eq!(ev.title, "login password=<SECRET>");
        assert_eq!(ev.attrs["api_token"], MARK);
        assert_eq!(ev.attrs["dsn"], "mysql://u:<SECRET>@h/db");
        assert_eq!(ev.attrs["tokens"], "123456789");
    }

    #[test]
    fn no_redacta_lo_corriente() {
        for s in [
            "auth token deadbeef expired",
            "token expired for user 42",
            "password reset requested",
            "retry 3/5 for job 1f2e3d4c",
            "GET /api/users/123 took 42ms",
            "sk-the-quick-brown-fox-jumps-over-the-lazy",
            "fix: token refresh race in auth middleware",
            "password=<redacted>",
            "token refresh_middleware_handler failed",
            "PWD=/home/juan/proyecto tokens_used=12345678",
            "sessionId=8f14e45f-ceea-467a-9575-1e3c6a5b2c11",
            "connection to 10.0.0.1:5432 refused",
            "Merge pull request #42 from user/feature-token-cache",
            "https://github.com/owner/repo/pull/12",
        ] {
            assert_eq!(r(s), s, "{s}");
        }
    }
}
