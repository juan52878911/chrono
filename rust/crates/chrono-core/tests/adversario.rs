//! Entradas adversarias y bordes contra los parsers puros del núcleo:
//! `Pattern`, `templatize` y `redact`. Generación pseudoaleatoria con semilla
//! fija (sin dependencias): el mismo fallo sale siempre igual.

use std::time::{Duration, Instant};

use chrono_core::{redact, templatize, Pattern};

/// xorshift64*: determinista y suficiente para fuzz.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

/// Texto aleatorio de un alfabeto con metacaracteres, unicode y espacios raros.
fn texto(rng: &mut Rng, alfabeto: &[&str], max: usize) -> String {
    let n = rng.below(max + 1);
    (0..n).map(|_| *rng.pick(alfabeto)).collect()
}

const META: &[&str] = &[
    "a", "b", "0", "9", "-", "_", ".", "*", "+", "?", "{", "}", ",", "(", ")", "[", "]", "^", "$", "\\", "d", "w", "s",
    "D", "W", "S", "1", "3", "ñ", "日", "😀", " ", "\t", "/", ":", "|",
];

// ── Pattern ───────────────────────────────────────────────────────────────────

#[test]
fn pattern_fuzz_nunca_hace_panic() {
    let mut rng = Rng(0x5eed_0001);
    let textos: Vec<String> = (0..40).map(|_| texto(&mut rng, META, 30)).collect();
    let mut compilados = 0;
    for _ in 0..20_000 {
        let p = texto(&mut rng, META, 14);
        let r = std::panic::catch_unwind(|| {
            if let Ok(pat) = Pattern::parse(&p) {
                for t in &textos {
                    let todas = pat.find_all(t);
                    // Cada valor es un trozo no vacío del texto.
                    for v in &todas {
                        assert!(!v.is_empty() && t.contains(v), "{p:?} en {t:?} dio {v:?}");
                    }
                    assert_eq!(pat.find(t), todas.first().copied(), "{p:?} {t:?}");
                }
                true
            } else {
                false
            }
        });
        match r {
            Ok(true) => compilados += 1,
            Ok(false) => {}
            Err(_) => panic!("panic con el patrón {p:?}"),
        }
    }
    assert!(compilados > 1000, "el generador casi no produce patrones válidos: {compilados}");
}

#[test]
fn pattern_tabla_de_bordes() {
    let ok: &[(&str, &str, Option<&str>)] = &[
        ("", "abc", None), // vacío: casa vacío, no cuenta
        ("^", "abc", None),
        ("$", "abc", None),
        ("^$", "", None),
        ("()", "abc", None),
        ("a()", "abc", None),
        ("(a)", "", None),
        ("a$", "aaa", Some("a")),
        ("^a", "aaa", Some("a")),
        ("a^", "xa^", Some("a^")),     // `^` en medio es literal
        ("$a", "x$a", Some("$a")),     // `$` al principio es literal
        (r"\$", "a$b", Some("$")),
        (r"[]a]+", "x]a]y", Some("]a]")), // `]` primero dentro de la clase es literal
        (r"[a-]+", "x-a-y", Some("-a-")),
        (r"[^]]+", "]]ab]", Some("ab")),
        (r"[\d\s]+", "x1 2y", Some("1 2")),
        (r"[\]]", "a]b", Some("]")),
        (r"a{0}b", "ab", Some("b")),
        (r"a{2}", "aaa", Some("aa")),
        (r"a{2,}", "aaaa", Some("aaaa")),
        (r"a{ 1 , 2 }", "aaa", Some("aa")),
        (r"\D+", "12ab34", Some("ab")),
        (r"\W", "ab-c", Some("-")),
        (r"\S+", "  ñu  ", Some("ñu")),
        (r"日(\d+)", "日本 日42", Some("42")),
        (r"😀.", "a😀b", Some("😀b")),
        (r".", "😀", Some("😀")),
        (r"(\d*)x", "x", None), // grupo vacío: no cuenta
        (r"a.c", "a\nc", Some("a\nc")),
        (r"a?", "bbb", None),
        (r"[a-a]", "a", Some("a")),
    ];
    for (p, t, want) in ok {
        let pat = Pattern::parse(p).unwrap_or_else(|e| panic!("{p:?} debía compilar: {e}"));
        assert_eq!(pat.find(t), *want, "patrón {p:?} sobre {t:?}");
    }
    for bad in [
        "(", ")", "((a))", "(a)(b)", "(a)?", "(a){2}", "a)", "[", "[a", "[^", "[]", r"[\", r"[\D]", r"[z-a]", "*",
        "+a", "?", "{2}", "a**", "a+*", "a{", "a{x}", "a{}", "a{,2}", "a{3,2}", "a{99999}", "a{1,99999999999999999999}",
        r"\", "a{-1}",
    ] {
        assert!(Pattern::parse(bad).is_err(), "{bad:?} debía fallar");
    }
}

#[test]
fn pattern_find_all_no_solapa_y_avanza_en_vacias() {
    let p = Pattern::parse(r"\d*").unwrap();
    assert_eq!(p.find_all("a12b3"), vec!["12", "3"]);
    let p = Pattern::parse(r"(a)b").unwrap();
    assert_eq!(p.find_all("ababab"), vec!["a", "a", "a"]);
    let p = Pattern::parse(r"^a").unwrap();
    assert_eq!(p.find_all("aaa"), vec!["a"]);
}

/// Un patrón con varias repeticiones encadenadas que no casa no puede tardar
/// exponencialmente: `\w*\w*\w*\w*!` contra 200 letras sin `!`.
#[test]
fn pattern_sin_vuelta_atras_catastrofica() {
    let texto = "a".repeat(200);
    for p in [r"\w*\w*\w*\w*!", r"(a*)*", r"a*a*a*a*a*a*b", r"[a-z]+[a-z]+[a-z]+[a-z]+\d"] {
        let Ok(pat) = Pattern::parse(p) else { continue };
        let t0 = Instant::now();
        assert_eq!(pat.find(&texto), None, "{p}");
        assert!(t0.elapsed() < Duration::from_secs(2), "{p} tardó {:?}", t0.elapsed());
    }
}

/// Un título largo (los patrones de relaciones se aplican a cada evento) no
/// puede costar `n × MAX_REPEAT`.
#[test]
fn pattern_titulo_largo_es_lineal() {
    let texto = "A".repeat(300_000);
    let pat = Pattern::parse(r"([A-Z]+-\d+)").unwrap();
    let t0 = Instant::now();
    assert_eq!(pat.find(&texto), None);
    assert!(t0.elapsed() < Duration::from_secs(2), "tardó {:?}", t0.elapsed());
}

/// Un patrón muy largo (de config) no desborda la pila al casar.
#[test]
fn pattern_muy_largo_no_desborda_la_pila() {
    let p = "a".repeat(5_000);
    let pat = Pattern::parse(&p).unwrap();
    let h = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || pat.find(&"a".repeat(5_000)).map(str::len))
        .unwrap();
    assert_eq!(h.join().expect("sin desbordar la pila"), Some(5_000));
}

// ── templatize ────────────────────────────────────────────────────────────────

const PIEZAS: &[&str] = &[
    "GET", "/api/users/", "12345", "0x7f27fb262780", "550e8400-e29b-41d4-a716-446655440000", "10.0.0.1", ":8080",
    "cafebabe1234", "deadbeef", "2024-01-01T10:00:00Z", "10:00:00.123", "ñandú", "日本語", "😀", "\"", ",", "(", ")",
    "=", "@", "-", ".", "_", "<NUM>", "<HEX>", "<IP>", "<UUID>", "<TS>", "<", ">", "C-", "id_", "v1.2.3", " ", "  ",
    "\t", "\u{a0}", "\u{2028}", "e", "1e10", "-42", "+3", "3.14", "255.255.255.256", "1.2.3.4.5", "::1", "fe80::1",
    "/home/juan/x.rs:42", "a1b2c3", "ABCDEF", "0X", "0x", "x",
];

#[test]
fn templatize_idempotente_y_sin_panic() {
    let mut rng = Rng(0x5eed_0002);
    for _ in 0..30_000 {
        let m = texto(&mut rng, PIEZAS, 12);
        let t1 = templatize(&m);
        let t2 = templatize(&t1);
        assert_eq!(t1, t2, "no idempotente: {m:?} → {t1:?} → {t2:?}");
        assert!(!t1.contains("  "), "{m:?} → {t1:?}");
    }
}

/// Dos mensajes con la misma forma (cambian solo los valores) dan la misma
/// plantilla.
#[test]
fn templatize_misma_forma_misma_plantilla() {
    let formas: &[fn(u64) -> String] = &[
        |n| format!("GET /api/users/{n} took {}ms", n % 997),
        |n| format!("req {:08x}-{:04x}-41d4-a716-{:012x} from 10.0.{}.{}:{}", n, n % 65536, n, n % 256, (n / 7) % 256, n % 65535),
        |n| format!("LOG C-0x{:012x}: crm_db/crm_user@172.18.0.{}:{} login", n, n % 256, n % 65535),
        |n| format!("hash {:040x} pushed", n.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 0xa000_0000_0000_0000),
        |n| format!("2026-10-{:02}T{:02}:{:02}:{:02}.{:03}Z worker {} done", n % 28 + 1, n % 24, n % 60, n % 60, n % 1000, n),
        |n| format!("[{}] ñandú «{}» terminó", n, n * 3),
        |n| format!("path=/var/log/app-{}/out.log size={}", n, n * 1024),
    ];
    let mut rng = Rng(0x5eed_0003);
    for (i, f) in formas.iter().enumerate() {
        let base = templatize(&f(1_000_003));
        for _ in 0..500 {
            let n = rng.next() % 1_000_000_000 + 10_000;
            assert_eq!(templatize(&f(n)), base, "forma {i}: {:?}", f(n));
        }
    }
}

#[test]
fn templatize_bordes() {
    assert_eq!(templatize(""), "");
    assert_eq!(templatize("   \t\n "), "");
    assert_eq!(templatize("😀😀 日本"), "😀😀 日本");
    // Un mensaje enorme: lineal y sin panic.
    let largo = "x=12 ".repeat(400_000);
    let t0 = Instant::now();
    let t = templatize(&largo);
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert!(t.starts_with("x=<NUM> x=<NUM>"));
    // Un token enorme sin espacios.
    let tok = "ab1".repeat(500_000);
    let _ = templatize(&tok);
}

// ── redact ────────────────────────────────────────────────────────────────────

fn r(s: &str) -> String {
    redact(s).0
}

/// Los formatos que promete redactar, en contextos distintos: el secreto no
/// puede sobrevivir en la salida.
#[test]
fn redact_no_deja_secretos() {
    let secretos: &[(&str, &str)] = &[
        ("ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8", "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"),
        ("github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz0123456789ABCD", "github_pat_11ABCDEFG0123456789"),
        ("gl\x70at-xY9zA1b2C3d4E5f6G7h8", "xY9zA1b2C3d4E5f6G7h8"),
        ("xo\x78b-123456789012-abcdefABCDEF1234", "abcdefABCDEF1234"),
        ("AKIAIOSFODNN7EXAMPLE", "IOSFODNN7EXAMPLE"),
        ("sk-ant-api03-Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0Uv", "Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0Uv"),
        ("sk-proj-Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0Uv12", "Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0Uv12"),
        ("AIzaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q", "SyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q"),
        ("npm_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7"),
        ("hf_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7"),
        (
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        ),
    ];
    let contextos: &[&str] = &[
        "{}",
        "token {} ok",
        "\"{}\"",
        "({})",
        "x={}",
        "url?access_token={}&x=1",
        "Authorization: Bearer {}",
        "{\"k\":\"{}\"}",
        "'{}',",
        "[{}]",
        "{}.",
    ];
    let mut casos = 0;
    for (s, nucleo) in secretos {
        for c in contextos {
            let linea = c.replace("{}", s);
            let out = r(&linea);
            assert!(!out.contains(nucleo), "queda el secreto en {linea:?} → {out:?}");
            casos += 1;
        }
    }
    assert!(casos > 100);
}

/// Contraseñas y claves con forma `clave=valor` / `clave: valor`, también con
/// espacio tras el separador (YAML, JSON con formato).
#[test]
fn redact_clave_valor_con_espacios() {
    for (linea, secreto) in [
        ("password=hunter2hunter2", "hunter2hunter2"),
        ("password: hunter2hunter2", "hunter2hunter2"),
        ("  password:   hunter2hunter2", "hunter2hunter2"),
        (r#"{"password": "hunter2hunter2"}"#, "hunter2hunter2"),
        (r#"{"client_secret": "abc123def456", "x": 1}"#, "abc123def456"),
        ("api_key: Zx9!aB7#qW2$", "Zx9!aB7#qW2$"),
        ("DB_PASSWORD=S3cr3t-P4ss", "S3cr3t-P4ss"),
    ] {
        let out = r(linea);
        assert!(!out.contains(secreto), "{linea:?} → {out:?}");
    }
}

/// Una credencial `Basic` corta (menos de 16 caracteres) en una cabecera
/// `Authorization` quedaba a la vista: `dTpw` es `u:p` en base64.
#[test]
fn redact_basic_corta_en_authorization() {
    for (linea, secreto) in [
        ("Authorization: Basic dTpw", "dTpw"),
        ("authorization: basic YWRtaW46eA==", "YWRtaW46eA=="),
        ("curl -H 'Authorization: Basic dTpw' https://api", "dTpw"),
        ("Proxy-Authorization: Basic YTpiYw==", "YTpiYw=="),
        (r#"{"Authorization":"Basic dTpw"}"#, "dTpw"),
        (r#"headers={"authorization": "Basic YWRtaW46eA"}"#, "YWRtaW46eA"),
        ("Authorization=Basic dTpw;", "dTpw"),
    ] {
        let (out, n) = redact(linea);
        assert!(!out.contains(secreto) && n == 1, "{linea:?} → {out:?} ({n})");
    }
    // Fuera de una cabecera, o lo que no es `usuario:clave` en base64, queda.
    for linea in [
        "Basic dTpw",
        "authorization: basic auth disabled",
        "Authorization uses Basic realm=api",
        "WWW-Authenticate: Basic realm=x",
        "authorization header: Basic scheme only",
        "Authorization: Basic",
    ] {
        assert_eq!(r(linea), linea);
    }
}

/// Una contraseña con `@` en una URL no puede dejar la cola.
#[test]
fn redact_url_con_arroba_en_la_clave() {
    let out = r("dsn postgres://app:p@ssw0rd!@db:5432/x");
    assert!(!out.contains("ssw0rd"), "{out}");
    assert!(out.contains("@db:5432/x"), "{out}");
}

/// Un PEM sin `-----END` (línea cortada) no puede dejar la clave a la vista.
#[test]
fn redact_pem_truncado() {
    let s = "k -----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAsecretmaterial";
    let out = r(s);
    assert!(!out.contains("MIIEow"), "{out}");
}

/// Falsos positivos en texto normal: rutas, hashes de git, ids, URLs.
#[test]
fn redact_falsos_positivos() {
    let mut rng = Rng(0x5eed_0004);
    let mut normales: Vec<String> = [
        "commit 1e5d5fa3b2c4d6e8f0a1b2c3d4e5f6a7b8c9d0e1 by Juan",
        "Merge branch 'feature/token-refresh' into main",
        "/Users/juan/Github/chrono/rust/crates/chrono-core/src/redact.rs:42",
        "src/auth/password_reset.rs: fix password reset email",
        "sessionId=8f14e45f-ceea-467a-9575-1e3c6a5b2c11 cwd=/home/juan",
        "tool_use_id=toolu_01ABCdefGHIjklMNOpqrSTU done",
        "request_id=req_011CUabcdefGHIJKLmnopq status=200",
        "https://github.com/owner/repo/blob/main/src/token.rs#L10",
        "max_tokens=4096 input_tokens=1234 output_tokens=567",
        "token budget exhausted: 120000/120000",
        "password: required",
        "token: invalid_signature for user-session-cookie",
        "secret: not-configured",
        r#"{"token": 12345678901, "password": null}"#,
        "error: token expired",
        "Error: password: must be at least 8 characters",
        "sk-learn version 1.3.0",
        "AKIA prefix detected in config",
        "basic auth disabled",
        "authorization: basic auth disabled",
        "Authorization failed: basic scheme not allowed",
        "authorization=basic mode",
        "bearer token missing",
        "secret_santa_list.txt updated",
        "eyJ is the base64 of {\"",
        "cookie banner shown",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Hashes, uuids y rutas aleatorias.
    for _ in 0..2000 {
        let h = format!("{:016x}{:016x}{:08x}", rng.next(), rng.next(), rng.next() as u32);
        let u = format!("{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}", rng.next() as u32, rng.next() as u16, rng.next() % 4096, rng.next() % 4096, rng.next() % (1 << 48));
        normales.push(format!("commit {h} touched /src/{u}/mod.rs:{}", rng.below(9999)));
        normales.push(format!("id={u} parent={h}"));
    }
    let fp: Vec<(String, String)> =
        normales.iter().filter_map(|s| { let o = r(s); (o != *s).then(|| (s.clone(), o)) }).collect();
    assert!(fp.is_empty(), "{} falsos positivos de {}: {:#?}", fp.len(), normales.len(), &fp[..fp.len().min(10)]);
}

#[test]
fn redact_fuzz_sin_panic_y_cuenta_coherente() {
    const P: &[&str] = &[
        "password", "token", "=", ":", "\"", "'", " ", "Bearer", "basic", "://", "@", "/", "-----BEGIN ", "PRIVATE KEY",
        "-----", "-----END ", "\n", "ghp_", "sk-", "eyJ", ".", "a1B2c3D4e5F6g7H8", "ñ", "日", "😀", "{", "}", ",", "<SECRET>",
        "AKIA", "0123456789",
    ];
    let mut rng = Rng(0x5eed_0005);
    for _ in 0..30_000 {
        let s = texto(&mut rng, P, 16);
        let (out, n) = std::panic::catch_unwind(|| redact(&s)).unwrap_or_else(|_| panic!("panic con {s:?}"));
        assert_eq!(n == 0, out == s, "{s:?} → {out:?} ({n})");
        // Redactar lo redactado no quita nada más (salvo trozos de PEM
        // revueltos, donde quitar una parte puede formar otra cabecera).
        let (out2, n2) = redact(&out);
        assert!(n2 == 0 || s.contains("-----"), "{s:?} → {out:?} → {out2:?}");
    }
}
