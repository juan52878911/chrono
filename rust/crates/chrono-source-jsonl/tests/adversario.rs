//! Ingesta jsonl con entradas raras: bytes no UTF-8, líneas gigantes, una
//! escritura a medias al final del fichero que se completa en el sync
//! siguiente, CRLF, BOM, ficheros vacíos, epochs en s/ms/µs/ns, `explode`
//! sobre lo que no es un array, claves con punto y nulos.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono_core::{Event, Source, SourceConfig, Watermark};
use chrono_source_jsonl::JsonlSource;

static N: AtomicU64 = AtomicU64::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new(bytes: &[u8]) -> Tmp {
        let p = std::env::temp_dir().join(format!("chrono-jsonl-adv-{}-{}.jsonl", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&p, bytes).unwrap();
        Tmp(p)
    }
    fn append(&self, bytes: &[u8]) {
        std::fs::OpenOptions::new().append(true).open(&self.0).unwrap().write_all(bytes).unwrap();
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn cfg(pairs: &[(&str, &str)]) -> SourceConfig {
    SourceConfig { options: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
}

/// Un sync: abre desde el watermark, lo drena y devuelve eventos y watermark.
fn sync(path: &Path, wm: Option<Watermark>, c: &SourceConfig) -> (Vec<Event>, Watermark) {
    let mut cur = JsonlSource::new().open(path, wm, c).unwrap();
    let mut out = Vec::new();
    while let Some(e) = cur.next().unwrap() {
        out.push(e);
    }
    (out, cur.watermark())
}

fn titles(evs: &[Event]) -> Vec<String> {
    evs.iter().map(|e| e.title.clone()).collect()
}

/// El escritor está a mitad de línea cuando llega el sync; la termina
/// después. La línea no se pierde ni se duplica.
#[test]
fn linea_a_medias_al_final_se_completa_en_el_sync_siguiente() {
    let f = Tmp::new(b"{\"msg\":\"uno\"}\n{\"msg\":\"do");
    let (evs, wm) = sync(&f.0, None, &cfg(&[]));
    assert_eq!(titles(&evs), ["uno"]);
    f.append(b"s\"}\n{\"msg\":\"tres\"}\n");
    let (evs, wm) = sync(&f.0, Some(wm), &cfg(&[]));
    assert_eq!(titles(&evs), ["dos", "tres"], "la línea a medias se perdió o se duplicó");
    let (evs, _) = sync(&f.0, Some(wm), &cfg(&[]));
    assert!(evs.is_empty());
}

/// Corte justo antes del `\n`: la línea ya es JSON completo y entra; el `\n`
/// que llega después no la repite.
#[test]
fn linea_completa_sin_salto_final_entra_una_vez() {
    let f = Tmp::new(b"{\"msg\":\"uno\"}\n{\"msg\":\"dos\"}");
    let (evs, wm) = sync(&f.0, None, &cfg(&[]));
    assert_eq!(titles(&evs), ["uno", "dos"]);
    f.append(b"\n{\"msg\":\"tres\"}\n");
    let (evs, _) = sync(&f.0, Some(wm), &cfg(&[]));
    assert_eq!(titles(&evs), ["tres"]);
}

/// Cortes en cada byte posible de un fichero: sumando los dos syncs salen
/// todas las líneas exactamente una vez.
#[test]
fn cortes_en_cada_byte_ni_pierden_ni_duplican() {
    let full = "{\"msg\":\"a\",\"n\":1}\r\n{\"msg\":\"b ñ 😀\",\"n\":2}\n\n{\"msg\":\"c\",\"x\":{\"y\":[1,2]}}\n{\"msg\":\"d\"}\n";
    let b = full.as_bytes();
    for cut in 0..=b.len() {
        let f = Tmp::new(&b[..cut]);
        let (mut evs, wm) = sync(&f.0, None, &cfg(&[]));
        f.append(&b[cut..]);
        let (more, _) = sync(&f.0, Some(wm), &cfg(&[]));
        evs.extend(more);
        assert_eq!(titles(&evs), ["a", "b ñ 😀", "c", "d"], "corte en el byte {cut}");
    }
}

#[test]
fn bom_al_principio_no_pierde_la_primera_linea() {
    let f = Tmp::new("\u{feff}{\"ts\":1704067200,\"msg\":\"uno\"}\n{\"ts\":1704067201,\"msg\":\"dos\"}\n".as_bytes());
    let (evs, _) = sync(&f.0, None, &cfg(&[]));
    assert_eq!(titles(&evs), ["uno", "dos"]);
    assert_eq!(evs[0].at_epoch, 1_704_067_200_000, "la autodetección del campo de tiempo falla con BOM");
}

#[test]
fn bytes_no_utf8_crlf_vacios_y_basura() {
    let mut b = Vec::new();
    b.extend_from_slice(b"{\"msg\":\"lat\xe9n1 \xff\xfe\"}\r\n");
    b.extend_from_slice(b"\r\n\n   \n\t\n");
    b.extend_from_slice(b"no es json\n[1,2,3]\n\"cadena\"\n42\nnull\n{\n}\n");
    b.extend_from_slice(b"{\"msg\":\"ok\"}\r\n");
    b.extend_from_slice(b"\x00\x00\x00\n");
    let f = Tmp::new(&b);
    let (evs, _) = sync(&f.0, None, &cfg(&[]));
    assert_eq!(evs.len(), 2, "{:?}", titles(&evs));
    assert!(evs[0].title.starts_with("lat"), "{:?}", evs[0].title);
    assert_eq!(evs[1].title, "ok");
    let vacio = Tmp::new(b"");
    let (evs, wm) = sync(&vacio.0, None, &cfg(&[]));
    assert!(evs.is_empty());
    vacio.append(b"{\"msg\":\"x\"}\n");
    assert_eq!(titles(&sync(&vacio.0, Some(wm), &cfg(&[])).0), ["x"]);
}

#[test]
fn linea_gigante_de_mas_de_10_mb() {
    let big = "x".repeat(11 << 20);
    let mut b = format!("{{\"msg\":\"antes\"}}\n{{\"msg\":\"{big}\"}}\n").into_bytes();
    b.extend_from_slice(b"{\"msg\":\"despues\"}\n");
    let f = Tmp::new(&b);
    let t0 = std::time::Instant::now();
    let (evs, _) = sync(&f.0, None, &cfg(&[]));
    assert_eq!(evs.len(), 3);
    assert_eq!(evs[2].title, "despues");
    assert!(t0.elapsed() < std::time::Duration::from_secs(20), "{:?}", t0.elapsed());
}

/// Epochs numéricos en s, ms, µs y ns, como número o cadena, y valores
/// imposibles (que no dan una fecha absurda).
#[test]
fn epochs_en_todas_las_unidades() {
    let want = 1_704_067_200_123i64;
    let casos: &[(&str, Option<i64>)] = &[
        ("1704067200", Some(1_704_067_200_000)),
        ("1704067200.123", Some(want)),
        ("1704067200123", Some(want)),
        ("1704067200123456", Some(want)),
        ("1704067200123456789", Some(want)),
        ("\"2024-01-01T00:00:00.123Z\"", Some(want)),
        ("\"2024-01-01 00:00:00.123\"", Some(want)),
        ("\"1704067200\"", Some(1_704_067_200_000)),
        ("\"1704067200123\"", Some(want)),
        ("0", Some(0)),
        ("-1", Some(-1000)),
        ("1e300", None),
        ("-1e300", None),
        ("1e30", None),
        ("99999999999999999999999", None),
        ("\"20240101\"", None),
        ("\"\"", None),
        ("\"ayer\"", None),
        ("null", None),
        ("true", None),
        ("[]", None),
        ("{}", None),
    ];
    for (v, want) in casos {
        let f = Tmp::new(format!("{{\"ts\":{v},\"msg\":\"m\"}}\n").as_bytes());
        let (evs, _) = sync(&f.0, None, &cfg(&[("time_field", "ts")]));
        assert_eq!(evs.len(), 1, "{v}");
        let got = (!evs[0].at.is_empty()).then_some(evs[0].at_epoch);
        assert_eq!(got, *want, "ts={v} → at={:?} at_epoch={}", evs[0].at, evs[0].at_epoch);
    }
}

#[test]
fn explode_sobre_vacios_y_no_arrays() {
    let c = cfg(&[("explode", "items"), ("message_field", "name")]);
    let f = Tmp::new(
        concat!(
            "{\"items\":[]}\n",
            "{\"items\":null}\n",
            "{\"items\":\"texto\"}\n",
            "{\"items\":{\"name\":\"obj\"}}\n",
            "{\"items\":[1,\"a\",null,[2]]}\n",
            "{\"otro\":1}\n",
            "{\"items\":[{\"name\":\"bueno\"}]}\n",
        )
        .as_bytes(),
    );
    let (evs, _) = sync(&f.0, None, &c);
    assert!(evs.iter().any(|e| e.title == "bueno"), "{:?}", titles(&evs));
}

#[test]
fn claves_con_punto_y_nulos() {
    let c = cfg(&[("time_field", "a.ts"), ("message_field", "m.sg"), ("entity_field", "svc")]);
    let f = Tmp::new(
        concat!(
            "{\"a.ts\":\"2024-01-01T00:00:00Z\",\"m.sg\":\"literal\",\"svc\":null}\n",
            "{\"a\":{\"ts\":\"2024-01-01T00:00:01Z\"},\"m\":{\"sg\":\"anidado\"},\"svc\":\"api\"}\n",
            "{\"a.ts\":null,\"m.sg\":null,\"svc\":null}\n",
        )
        .as_bytes(),
    );
    let (evs, _) = sync(&f.0, None, &c);
    assert_eq!(evs.len(), 3);
    assert_eq!(evs[0].at_epoch, 1_704_067_200_000);
    assert_eq!(evs[1].at_epoch, 1_704_067_201_000);
    assert_eq!(titles(&evs)[..2], ["literal", "anidado"]);
}

/// Basura con forma de JSON generada con semilla fija: nunca panic.
#[test]
fn fuzz_lineas_sin_panic() {
    const P: &[&str] = &[
        "{", "}", "[", "]", "\"", ":", ",", "\"ts\"", "\"msg\"", "\"items\"", "1", "-1", "1e400", "null", "true", "\\u0000",
        "\\ud800", "ñ", "😀", " ", "\n", "\r\n", "\u{feff}",
    ];
    let mut x: u64 = 0x5eed_0009;
    let c = cfg(&[("explode", "items"), ("time_field", "ts")]);
    for _ in 0..300 {
        let mut s = String::new();
        for _ in 0..400 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.push_str(P[(x % P.len() as u64) as usize]);
        }
        let f = Tmp::new(s.as_bytes());
        for c in [&c, &cfg(&[])] {
            let r = std::panic::catch_unwind(|| sync(&f.0, None, c));
            assert!(r.is_ok(), "panic con {s:?}");
        }
    }
}
