//! Tests del adaptador delega con un directorio de estado SINTÉTICO
//! (`testdata/state`): un trabajo con nombres nuevos, uno legado en español, uno que
//! nunca arrancó, uno rechazado, una carpeta que no es de trabajo y un `audit.jsonl`
//! con una línea rota.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/state")
}

fn drain(cur: &mut Box<dyn Cursor>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(e) = cur.next().unwrap() {
        out.push(e);
    }
    out
}

fn open_with(path: &Path, wm: Option<Watermark>, opts: &[(&str, &str)]) -> CoreResult<Box<dyn Cursor>> {
    let cfg = SourceConfig { options: opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    DelegaSource::new().open(path, wm, &cfg)
}

fn all(path: &Path) -> Vec<Event> {
    drain(&mut open_with(path, None, &[]).unwrap())
}

fn by_id<'a>(evs: &'a [Event], id: &str) -> &'a Event {
    evs.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("falta {id}"))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Copia del fixture en un directorio temporal (para los tests que escriben).
struct TempState(PathBuf);
impl TempState {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dst = std::env::temp_dir().join(format!("chrono-delega-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dst);
        copy_dir(&fixture(), &dst);
        TempState(dst)
    }
}
impl Drop for TempState {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &dst.join(e.file_name()));
        } else {
            fs::copy(&p, dst.join(e.file_name())).unwrap();
        }
    }
}

const NEW: &str = "20261026-122251-88099-chrono";
const OLD: &str = "20261021-150800-61797-legado";

#[test]
fn detect_reconoce_el_directorio_de_estado() {
    let s = DelegaSource::new();
    assert_eq!(s.detect(&fixture()), 85);
    assert_eq!(s.detect(&fixture().join("audit.jsonl")), 0);
    assert_eq!(s.detect(&fixture().join("metricas")), 0);
    // Sin audit.jsonl también: basta una carpeta con meta y brief.md.
    let t = TempState::new();
    fs::remove_file(t.0.join("audit.jsonl")).unwrap();
    assert_eq!(s.detect(&t.0), 85);
}

#[test]
fn trabajo_nuevo_inicio_fin_coste_y_atributos() {
    let evs = all(&fixture());
    let start = by_id(&evs, &format!("delega:{NEW}:start"));
    assert_eq!(start.kind, "delega.job.start");
    // Hora local sin zona; el offset (-05:00) se deduce de resumen frente a estado.
    assert_eq!(start.at, "2026-10-26T17:22:55Z");
    assert_eq!(start.title, format!("job.start {NEW} work@pc1"));
    assert_eq!(start.touches[0].entity, NEW);
    assert_eq!(start.touches[0].entity_type, "job");
    assert_eq!(start.actor.name, "orq-alfa");
    for (k, v) in [("profile", "work"), ("node", "pc1"), ("owner", "orq-alfa"), ("repo", "chrono"), ("branch", "delega/20261026-122251-88099")] {
        assert_eq!(start.attrs.get(k).map(String::as_str), Some(v), "{k}");
    }
    let end = by_id(&evs, &format!("delega:{NEW}:end"));
    assert_eq!(end.at, "2026-10-26T17:26:54Z");
    assert_eq!(end.level, "INFO");
    for (k, v) in [("cost_usd", "0.75"), ("cost_from", "summary"), ("duration_min", "4"), ("messages", "37"), ("tools", "21"), ("rc", "0"), ("output", "yes"), ("model", "claude-sonnet-5-5")] {
        assert_eq!(end.attrs.get(k).map(String::as_str), Some(v), "{k}");
    }
    assert!(!end.attrs.contains_key("cap"));
}

#[test]
fn trabajo_legado_nombres_en_espanol() {
    let evs = all(&fixture());
    let end = by_id(&evs, &format!("delega:{OLD}:end"));
    // resumen.json manda sobre salida.json (como `delega spend`).
    assert_eq!(end.attrs["cost_usd"], "24.5");
    assert_eq!(end.attrs["cost_from"], "summary");
    assert_eq!(end.attrs["messages"], "122");
    assert_eq!(end.attrs["rc"], "1");
    assert_eq!(end.attrs["cap"], "19.70 $ ≥ 15");
    assert_eq!(end.attrs["node"], "local");
    assert_eq!(end.attrs["owner"], "orq-beta");
    assert_eq!(end.level, "WARN");
    assert_eq!(end.title, format!("job.end {OLD} rc=1 $24.50 cap"));
    let kinds: Vec<(&str, &str)> = evs
        .iter()
        .filter(|e| e.touches.first().is_some_and(|t| t.entity == OLD) && e.kind != "delega.audit.end")
        .map(|e| (e.kind.as_str(), e.attrs.get("notice").or(e.attrs.get("alert")).map_or("", String::as_str)))
        .collect();
    assert!(kinds.contains(&("delega.notice", "question")));
    assert!(kinds.contains(&("delega.notice", "blocker")));
    assert!(kinds.contains(&("delega.alert", "loop")));
    assert!(kinds.contains(&("delega.alert", "undelivered")));
    // Los AVISO del progreso no se duplican: los avisos salen de avisos.jsonl.
    assert_eq!(kinds.iter().filter(|k| k.0 == "delega.notice").count(), 2);
    let blocker = evs.iter().find(|e| e.attrs.get("notice").map(String::as_str) == Some("blocker")).unwrap();
    assert_eq!(blocker.level, "ERROR");
}

#[test]
fn sin_texto_libre_y_redactado() {
    let evs = all(&fixture());
    for e in &evs {
        let all_text = format!("{} {} {:?}", e.title, e.body, e.attrs);
        assert!(!all_text.contains("SECRET-BRIEF"), "{}", e.id);
        assert!(!all_text.contains("Detalle largo"), "{}", e.id);
        assert!(!all_text.contains("segunda línea"), "{}", e.id);
        // El texto de las alertas cita comandos: no entra.
        assert!(!all_text.contains("última acción"), "{}", e.id);
    }
    let res = by_id(&evs, &format!("delega:{NEW}:result"));
    assert_eq!(res.title, format!("job.result {NEW}: Hecho para [redacted]: tests en verde"));
    let q = evs.iter().find(|e| e.id == format!("delega:{NEW}:n1")).unwrap();
    assert_eq!(q.title, format!("notice.question {NEW}: ¿[redacted] o Beta?"));
    assert_eq!(q.level, "WARN");
    // El forbidden.txt de un trabajo no se aplica a otro ni al audit; el fichero global sí.
    let refused = evs.iter().find(|e| e.kind == "delega.audit.refused").unwrap();
    assert!(refused.title.contains("Acme"));
    let terms = std::env::temp_dir().join(format!("chrono-delega-terms-{}", std::process::id()));
    fs::write(&terms, "# comentario\nacme\n").unwrap();
    let evs = drain(&mut open_with(&fixture(), None, &[("forbidden_terms", terms.to_str().unwrap())]).unwrap());
    let refused = evs.iter().find(|e| e.kind == "delega.audit.refused").unwrap();
    assert_eq!(refused.title, "audit.refused 20261026-001509-83575-rechazado: kindling no disponible para [redacted]: sin capacidad");
    assert_eq!(refused.level, "ERROR");
    assert_eq!(refused.attrs["rc"], "5");
    let _ = fs::remove_file(terms);
}

#[test]
fn trabajos_sin_cierre_y_audit() {
    let evs = all(&fixture());
    // Nunca arrancó / rechazado: solo el inicio (a la hora del id del trabajo).
    for j in ["20261022-122205-45617-norun", "20261026-001509-83575-rechazado"] {
        assert!(evs.iter().any(|e| e.id == format!("delega:{j}:start")));
        assert!(!evs.iter().any(|e| e.id == format!("delega:{j}:end")));
    }
    assert_eq!(by_id(&evs, "delega:20261022-122205-45617-norun:start").at, "2026-10-22T17:22:05Z");
    // `metricas/` no es un trabajo.
    assert!(!evs.iter().any(|e| e.id.contains("metricas")));
    let audit: Vec<&Event> = evs.iter().filter(|e| e.kind.starts_with("delega.audit.")).collect();
    assert_eq!(audit.len(), 6, "la línea rota se salta");
    let integ: Vec<&&Event> = audit.iter().filter(|e| e.kind == "delega.audit.integrate").collect();
    assert_eq!(integ[0].level, "WARN");
    assert_eq!(integ[0].attrs["sha"], "aaa1111");
    assert!(integ[0].touches.is_empty(), "job: null no crea entidad");
    let golden = audit.iter().find(|e| e.kind == "delega.audit.golden_build").unwrap();
    assert!(!golden.attrs.contains_key("dbs"), "las listas no entran como atributo");
    // Orden cronológico y determinista.
    assert!(evs.windows(2).all(|w| (w[0].at_epoch, &w[0].id) <= (w[1].at_epoch, &w[1].id)));
    assert_eq!(evs, all(&fixture()));
    let m = open_with(&fixture(), None, &[]).unwrap().manifest();
    assert_eq!(m["delega_jobs"], "4");
    assert_eq!(m["delega_audit_invalid"], "1");
    assert_eq!(m["utc_offset_from"], "observed");
    assert_eq!(m["utc_offset_s"], "-18000");
}

#[test]
fn utc_offset_por_opcion() {
    let evs = drain(&mut open_with(&fixture(), None, &[("utc_offset", "+00:00")]).unwrap());
    assert_eq!(by_id(&evs, &format!("delega:{NEW}:start")).at, "2026-10-26T12:22:55Z");
}

#[test]
fn incremental_audit_y_carpetas() {
    let t = TempState::new();
    let mut cur = open_with(&t.0, None, &[]).unwrap();
    let first = drain(&mut cur);
    let wm = cur.watermark();
    assert!(wm.value.starts_with("mt:") && wm.value.contains("|tz:-18000"), "{}", wm.value);

    // Sin cambios: nada.
    let mut cur = open_with(&t.0, Some(wm.clone()), &[]).unwrap();
    assert!(drain(&mut cur).is_empty());

    // Una línea nueva en audit y un aviso nuevo en un trabajo.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut f = fs::OpenOptions::new().append(true).open(t.0.join("audit.jsonl")).unwrap();
    use std::io::Write;
    writeln!(f, r#"{{"ts": "2026-10-26T14:00:00", "event": "launch", "job": "j2", "profile": "work"}}"#).unwrap();
    write!(f, r#"{{"ts": "2026-10-26T14:00:01", "event": "end""#).unwrap(); // a medio escribir
    drop(f);
    let mut f = fs::OpenOptions::new().append(true).open(t.0.join(NEW).join("notices.jsonl")).unwrap();
    writeln!(f, r#"{{"ts": "2026-10-26T12:26:00", "kind": "blocker", "text": "x"}}"#).unwrap();
    drop(f);
    let mut cur = open_with(&t.0, Some(wm), &[]).unwrap();
    let delta = drain(&mut cur);
    assert_eq!(delta.iter().filter(|e| e.kind.starts_with("delega.audit")).count(), 1);
    assert!(delta.iter().any(|e| e.id == format!("delega:{NEW}:n3")));
    // Solo se relee la carpeta que cambió; lo ya visto conserva su id.
    assert!(delta.iter().all(|e| e.kind.starts_with("delega.audit") || e.id.contains(NEW)));
    assert!(delta.iter().filter(|e| !e.id.ends_with(":n3") && !e.kind.starts_with("delega.audit")).all(|e| first.contains(e)));
    let wm2 = cur.watermark();

    // audit reescrito → diverge.
    fs::write(t.0.join("audit.jsonl"), "{}\n").unwrap();
    assert!(matches!(open_with(&t.0, Some(wm2.clone()), &[]).err(), Some(CoreError::Diverged(_))));
    // Cambiar el offset con un watermark previo → diverge (hay que reindexar).
    assert!(matches!(open_with(&t.0, Some(wm2), &[("utc_offset", "+01:00")]).err(), Some(CoreError::Diverged(_))));
}

#[test]
fn utilidades_de_tiempo_y_redaccion() {
    assert_eq!(util::parse_ts("2026-10-26T12:22:55"), util::parse_ts("2026-10-26 12:22:55"));
    assert_eq!(ts_ms("2026-10-26T12:22:55Z", -18000), ts_ms("2026-10-26T07:22:55", -18000));
    assert_eq!(ts_ms("2026-10-26T12:22:55.5-05:00", 0).unwrap() % 1000, 500);
    assert!(util::parse_ts("ayer").is_none());
    assert_eq!(redact("ACME y acme", &["Acme".to_string()]), "[redacted] y [redacted]");
    assert_eq!(util::first_line("\n\n# Título largo\nresto", 8, &[]), "Título …");
    assert_eq!(parse_offset("-05:00"), Some(-18000));
    assert_eq!(parse_offset("+0530"), Some(19800));
}

#[test]
fn ids_de_audit_unicos_por_fuente() {
    // Dos directorios de estado con el mismo audit.jsonl: los mismos bytes, otra fuente.
    let (a, b) = (TempState::new(), TempState::new());
    let ids = |p: &Path| -> Vec<String> { all(p).into_iter().filter(|e| e.kind.starts_with("delega.audit.")).map(|e| e.id).collect() };
    let (ia, ib) = (ids(&a.0), ids(&b.0));
    assert!(!ia.is_empty());
    assert!(ia.iter().all(|id| id.starts_with("delega:audit:") && id.matches(':').count() == 3), "{ia:?}");
    assert!(ia.iter().all(|id| !ib.contains(id)), "{ia:?} {ib:?}");
    // Los de trabajo son la identidad del trabajo: iguales en las dos.
    let job = |p: &Path| all(p).into_iter().find(|e| e.kind == "delega.job.start").map(|e| e.id).unwrap();
    assert_eq!(job(&a.0), job(&b.0));
}

#[test]
fn opciones_declaradas_y_validadas() {
    let s = DelegaSource::new();
    let keys: Vec<&str> = s.options().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, vec!["utc_offset", "forbidden_terms"]);
    let cfg = |v: &str| SourceConfig { options: [("utc_offset".to_string(), v.to_string())].into() };
    for ok in ["-05:00", "+0530", "Z", "+14", "-18:00"] {
        assert!(s.validate(&cfg(ok)).is_ok(), "{ok}");
    }
    for bad in ["5", "-5:00", "+99:00", "+05:75", "+18:30", "−05:00", "+٠٥:٠٠", "abc"] {
        assert!(s.validate(&cfg(bad)).is_err(), "{bad}");
        assert!(open_with(&fixture(), None, &[("utc_offset", bad)]).is_err(), "{bad}");
    }
    // Un tz de watermark fuera de rango no es de esta fuente: diverge.
    let wm = Watermark { kind: "delega".into(), value: "mt:0|a:0|tz:9223372036854775807".into() };
    assert!(matches!(open_with(&fixture(), Some(wm), &[]).err(), Some(CoreError::Diverged(_))));
}

#[test]
fn fechas_imposibles_no_son_fechas() {
    for bad in [
        "2026-02-30T10:00:00", "2025-02-29T10:00:00", "2100-02-29T10:00:00", "2026-04-31T10:00:00", "2026-13-01T10:00:00",
        "2026-00-10T10:00:00", "2026-10-00T10:00:00", "2026-10-26T24:00:00", "2026-10-26T10:60:00", "2026-10-26T10:00:61",
        "-001-01-01T00:00:00", "+999-01-01T00:00:00", "2026-10-26T10:00:00+25:00", "2026-10-26T10:00:00+05:99",
        "2026-10-26T10:00:00+18:30", "2026-10-26T10:00:00.", "2026-10-26T10:00:00Zbasura", "2026-10-26", "2026-1-26T10:00:00",
    ] {
        assert_eq!(util::parse_ts(bad), None, "{bad}");
        assert_eq!(ts_ms(bad, 0), None, "{bad}");
    }
    for ok in ["2024-02-29T10:00:00", "2000-02-29T10:00:00", "2026-10-26T23:59:60", "2026-10-26T10:00:00-18:00", " 2026-10-26T10:00:00 "] {
        assert!(util::parse_ts(ok).is_some(), "{ok}");
    }
    // Lo que se lee sin zona es la hora local tal cual; la zona va aparte.
    assert_eq!(util::parse_ts("1970-01-02T00:00:00.250-05:00"), Some((86_400_250, Some(-18_000))));
    assert_eq!(util::epoch_s_to_ms(f64::NAN), None);
    assert_eq!(util::epoch_s_to_ms(1e300), None);
    assert_eq!(util::epoch_s_to_ms(-1e300), None);
    assert_eq!(util::epoch_s_to_ms(1_793_034_000.5), Some(1_793_034_000_500));
}

#[test]
fn entradas_raras_sin_panic() {
    let t = TempState::new();
    // Nombre de carpeta con un carácter de varios bytes donde va la fecha.
    let raro = t.0.join("202é102-122251-1-raro");
    fs::create_dir_all(&raro).unwrap();
    fs::write(raro.join("meta"), "PROFILE=x\nEND=1e300\n").unwrap();
    fs::write(raro.join("brief.md"), "x").unwrap();
    // Epochs enormes, negativos o NaN en el estado y un resumen con fechas imposibles.
    let j = t.0.join("20261026-130000-2-extremos");
    fs::create_dir_all(&j).unwrap();
    fs::write(j.join("meta"), "END=-1e300\n").unwrap();
    fs::write(j.join("brief.md"), "x").unwrap();
    fs::write(j.join("state.json"), r#"{"start": 1e300, "last_event": -1e300, "cost_usd": 1e308, "rc": 1e300}"#).unwrap();
    fs::write(j.join("summary.json"), r#"{"start": "2026-02-31T10:00:00", "end": "9999-12-31T23:59:59-18:00", "duration_min": 1e308}"#).unwrap();
    fs::write(j.join("progress.log"), "2026-10-26T13:00:00\tSTART\n\u{0}\t\t\n9999-99-99T99:99:99\tMESSAGE\tx\n").unwrap();
    fs::write(j.join("rc"), "99999999999999999999999").unwrap();
    let mut audit = fs::read(t.0.join("audit.jsonl")).unwrap();
    audit.extend_from_slice("{\"event\":\"end\",\"ts\":\"9999-12-31T23:59:59\",\"job\":\"é\",\"rc\":1e300}\n[1,2]\n\"x\"\n{\"event\":\"x\",\"ts\":\"2026-10-26T10:00:00+99:00\"}\n".as_bytes());
    fs::write(t.0.join("audit.jsonl"), audit).unwrap();
    for off in [None, Some("+14:00"), Some("-12:00")] {
        let opts: Vec<(&str, &str)> = off.map(|o| vec![("utc_offset", o)]).unwrap_or_default();
        let evs = drain(&mut open_with(&t.0, None, &opts).unwrap());
        for e in &evs {
            assert!(util::valid_ms(e.at_epoch).is_some() || e.at_epoch == 0, "{} {}", e.id, e.at_epoch);
        }
    }
}
