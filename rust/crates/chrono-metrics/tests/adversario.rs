//! Lados de `gaps` (`parse_side`) con entradas raras: comillas, `=` dentro del
//! valor, vacíos, espacios unicode y basura generada con semilla fija.

use chrono_metrics::{parse_side, SideError};

/// (source, kind, level, entity, grep, attr, join)
type Campos<'a> = [Option<&'a str>; 7];

fn campos<'a>(raw: &'a str) -> Result<Campos<'a>, SideError> {
    parse_side(raw).map(|s| [s.source, s.kind, s.level, s.entity, s.grep, s.attr, s.join])
}

#[test]
fn lados_tabla() {
    let n = None;
    let ok: &[(&str, Campos)] = &[
        ("claude", [Some("claude"), n, n, n, n, n, n]),
        ("  claude  ", [Some("claude"), n, n, n, n, n, n]),
        ("claude:grep=exit code 1", [Some("claude"), n, n, n, Some("exit code 1"), n, n]),
        ("claude grep=exit code 1 level=ERROR", [Some("claude"), n, Some("ERROR"), n, Some("exit code 1"), n, n]),
        ("grep=a=b", [n, n, n, n, Some("a=b"), n, n]),
        ("grep=x y=z w", [n, n, n, n, Some("x y=z w"), n, n]),
        ("grep=\"a  b\" kind=log", [n, Some("log"), n, n, Some("a  b"), n, n]),
        ("grep='it\"s' x", [Some("x"), n, n, n, Some("it\"s"), n, n]),
        ("attr=container:db-1:5432", [n, n, n, n, n, Some("container:db-1:5432"), n]),
        ("join=sessionId=@e1", [n, n, n, n, n, n, Some("sessionId=@e1")]),
        ("join=K", [n, n, n, n, n, n, Some("K")]),
        ("source=git kind=commit", [Some("git"), Some("commit"), n, n, n, n, n]),
        ("ñandú grep=😀 日本", [Some("ñandú"), n, n, n, Some("😀 日本"), n, n]),
        ("x\u{3000}grep=a\u{a0}b", [Some("x"), n, n, n, Some("a\u{a0}b"), n, n]),
        ("entity=a,b,c", [n, n, n, Some("a,b,c"), n, n, n]),
        ("kind=log*", [n, Some("log*"), n, n, n, n, n]),
    ];
    for (raw, want) in ok {
        assert_eq!(campos(raw).as_ref(), Ok(want), "{raw:?}");
    }
    let bad: &[(&str, SideError)] = &[
        ("", SideError::Empty),
        ("   ", SideError::Empty),
        ("a b", SideError::TwoSources("b".into())),
        ("a b:grep=x", SideError::TwoSources("b".into())),
        ("foo=bar", SideError::UnknownKey("foo".into())),
        ("=bar", SideError::UnknownKey("".into())),
        (":grep=x", SideError::UnknownKey(":grep".into())),
        ("grep=\"abc", SideError::UnclosedQuote),
        ("grep='", SideError::UnclosedQuote),
        ("grep=\"\"", SideError::Empty),
    ];
    for (raw, want) in bad {
        assert_eq!(campos(raw).as_ref(), Err(want), "{raw:?}");
    }
}

/// Basura generada: nunca panic, y cada valor es un trozo del texto.
#[test]
fn lados_fuzz_sin_panic() {
    const P: &[&str] = &[
        "grep", "kind", "level", "attr", "join", "entity", "source", "=", ":", "\"", "'", " ", "  ", "\t", "a", "b", "ñ",
        "😀", "\u{3000}", "*", "@", ",", "claude",
    ];
    let mut x: u64 = 0x5eed_0008;
    let mut casos = 0;
    for _ in 0..50_000 {
        let mut raw = String::new();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        for k in 0..(x % 12) {
            raw.push_str(P[((x >> (k * 5)) % P.len() as u64) as usize]);
        }
        let r = std::panic::catch_unwind(|| campos(&raw).map(|c| c.map(|v| v.map(str::to_string))));
        let Ok(r) = r else { panic!("panic con {raw:?}") };
        if let Ok(c) = r {
            for v in c.iter().flatten() {
                assert!(raw.contains(v.as_str()), "{raw:?} → {v:?}");
            }
        }
        casos += 1;
    }
    assert_eq!(casos, 50_000);
}
