//! Fuentes de COMANDO: chrono ejecuta lo que el usuario declara en
//! `.chrono/config.json` (`commands`), lee su salida JSON y la convierte en una
//! fuente JSONL más del índice (ver `chrono_classify_rules::CommandSpec`).
//!
//! Por qué así y no un adaptador por herramienta: chrono es generalista y no
//! abre red propia; `gh`, `kubectl`, `docker`, `curl` o un script del usuario
//! ya saben hablar con su sistema. chrono solo pone lo que hace bien —índice,
//! plantillas, correlación con git— sobre lo que esos comandos imprimen.
//!
//! Materialización incremental: cada ejecución AÑADE al caché
//! `<dir del índice>/cmd/<nombre>.cmd.jsonl` solo los registros que no estaban
//! (por `id_field`, o por el hash del registro), así la fuente JSONL que lo lee
//! sigue siendo append-only y `chrono sync` procesa solo el delta.
//!
//! Seguridad: se ejecuta exactamente el argv de la config, sin shell y con
//! límite de tiempo. Nada de lo que imprima el comando se ejecuta.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono_classify_rules::{CommandSpec, COMMAND_CACHE_SUFFIX};
use serde_json::Value;

use crate::i18n::t;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Lo que dejó una ejecución.
pub struct Materialized {
    pub cache: PathBuf,
    pub records: usize,
    pub added: usize,
}

/// Prefijo con el que se nombra una fuente de comando en la CLI: `cmd:<nombre>`.
pub const PREFIX: &str = "cmd:";

/// Caché de una fuente de comando junto al índice.
pub fn cache_path(db: &Path, name: &str) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("cmd").join(format!("{name}{COMMAND_CACHE_SUFFIX}"))
}

/// Ejecuta el comando `name` y añade al caché los registros nuevos.
pub fn materialize(db: &Path, index_root: &Path, name: &str, spec: &CommandSpec) -> Result<Materialized> {
    if spec.run.is_empty() {
        return Err(t(
            format!("command {name:?} has an empty \"run\" in .chrono/config.json"),
            format!("el comando {name:?} tiene \"run\" vacío en .chrono/config.json"),
        )
        .into());
    }
    let cwd = if spec.cwd.is_empty() { index_root.to_path_buf() } else { index_root.join(&spec.cwd) };
    eprintln!("  $ {}", spec.run.join(" "));
    let stdout = run_with_timeout(&spec.run, &cwd, Duration::from_secs(spec.timeout_secs.max(1)))?;
    let records = extract_records(&stdout, &spec.records)?;

    let cache = cache_path(db, name);
    std::fs::create_dir_all(cache.parent().expect("cache tiene padre"))?;
    let keys_path = cache.with_extension("keys");
    let mut seen: BTreeSet<String> = std::fs::read_to_string(&keys_path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let id_field = spec.options.get("id_field").map(String::as_str).filter(|s| !s.is_empty());

    let mut out = std::fs::OpenOptions::new().create(true).append(true).open(&cache)?;
    let mut keys_out = std::fs::OpenOptions::new().create(true).append(true).open(&keys_path)?;
    let mut added = 0usize;
    let redact = spec.options.get("redact").map(String::as_str) != Some("off");
    for rec in &records {
        let line = serde_json::to_string(rec)?;
        let key = id_field.and_then(|f| lookup(rec, f)).unwrap_or_else(|| format!("h:{:016x}", fnv1a64(line.as_bytes())));
        if seen.insert(key.clone()) {
            // El caché es un fichero en disco: los secretos no llegan ni a él.
            let line = if redact {
                let mut r = rec.clone();
                redact_json(&mut r);
                serde_json::to_string(&r)?
            } else {
                line
            };
            writeln!(out, "{line}")?;
            writeln!(keys_out, "{key}")?;
            added += 1;
        }
    }
    Ok(Materialized { cache, records: records.len(), added })
}

/// Ejecuta `argv` en `cwd` y devuelve su stdout. Error si no arranca, si sale
/// con código ≠ 0 (con el final de stderr) o si pasa del tiempo.
fn run_with_timeout(argv: &[String], cwd: &Path, timeout: Duration) -> Result<String> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| t(format!("cannot run {:?}: {e}", argv[0]), format!("no se pudo ejecutar {:?}: {e}", argv[0])))?;
    // Lectores en hilos: un stdout grande llenaría el pipe y el hijo no
    // terminaría nunca si se leyera solo al final.
    let mut so = child.stdout.take().expect("stdout piped");
    let mut se = child.stderr.take().expect("stderr piped");
    let h_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let h_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(t(
                format!("{:?} took longer than {}s (timeout_secs)", argv[0], timeout.as_secs()),
                format!("{:?} tardó más de {}s (timeout_secs)", argv[0], timeout.as_secs()),
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = h_out.join().unwrap_or_default();
    let stderr = h_err.join().unwrap_or_default();
    if !status.success() {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
        return Err(t(
            format!("{:?} exited with {status}: {tail}", argv[0]),
            format!("{:?} terminó con {status}: {tail}", argv[0]),
        )
        .into());
    }
    Ok(stdout)
}

/// Registros (objetos) de la salida: JSON entero (array, objeto, o `records`
/// dentro de él) o, si no parsea entero, NDJSON línea a línea.
pub fn extract_records(stdout: &str, records_path: &str) -> Result<Vec<Value>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let objs = |v: Vec<Value>| v.into_iter().filter(Value::is_object).collect::<Vec<_>>();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        let target = if records_path.is_empty() {
            Some(&v)
        } else {
            records_path.split('.').try_fold(&v, |cur, k| match cur {
                Value::Object(m) => m.get(k),
                Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)),
                _ => None,
            })
        };
        return match target {
            Some(Value::Array(a)) => Ok(objs(a.clone())),
            Some(o @ Value::Object(_)) => Ok(vec![o.clone()]),
            _ => Err(t(
                format!("\"records\": {records_path:?} is not an array in the command output"),
                format!("\"records\": {records_path:?} no es un array en la salida del comando"),
            )
            .into()),
        };
    }
    Ok(objs(trimmed.lines().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).collect()))
}

/// Valor escalar en una ruta `a.b.c` del registro, como texto.
fn lookup(v: &Value, path: &str) -> Option<String> {
    let found = path.split('.').try_fold(v, |cur, k| match cur {
        Value::Object(m) => m.get(k),
        _ => None,
    })?;
    match found {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Redacta un JSON en su sitio: el valor entero bajo una clave sensible
/// (`password`, `token`…) y, en el resto de textos, lo que tenga forma de
/// secreto. Devuelve cuántos quitó. En JSON compacto no hay espacios, así que
/// `chrono_core::redact` sobre la línea entera no vería `"password":"…"`.
pub(crate) fn redact_json(v: &mut Value) -> usize {
    match v {
        Value::String(s) => {
            let (r, n) = chrono_core::redact(s);
            if n > 0 {
                *s = r;
            }
            n
        }
        Value::Array(a) => a.iter_mut().map(redact_json).sum(),
        Value::Object(o) => o
            .iter_mut()
            .map(|(k, x)| match x {
                Value::String(s) if chrono_core::redact::sensitive_key(k) && chrono_core::redact::sensitive_value(s) => {
                    *s = chrono_core::redact::MARK.to_string();
                    1
                }
                _ => redact_json(x),
            })
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacta_json_por_clave_y_por_forma() {
        let mut v = serde_json::json!({"user": "ana", "password": "hunter2hunter2", "n": {"api_token": "abc123abc123"},
            "msg": "dsn postgres://app:s3cr3tpass@db/x", "count": 3, "tokens": "12345678"});
        assert_eq!(redact_json(&mut v), 3);
        assert_eq!(v["password"], "<SECRET>");
        assert_eq!(v["n"]["api_token"], "<SECRET>");
        assert_eq!(v["msg"], "dsn postgres://app:<SECRET>@db/x");
        assert_eq!(v["user"], "ana");
        assert_eq!(v["tokens"], "12345678");
    }

    #[test]
    fn registros_de_array_ruta_objeto_y_ndjson() {
        assert_eq!(extract_records(r#"[{"a":1},{"a":2},3]"#, "").unwrap().len(), 2, "los no-objeto se ignoran");
        assert_eq!(extract_records(r#"{"jobs":[{"a":1}]}"#, "jobs").unwrap().len(), 1);
        assert_eq!(extract_records(r#"{"data":{"items":[{"a":1},{"a":2}]}}"#, "data.items").unwrap().len(), 2);
        assert_eq!(extract_records(r#"{"a":1}"#, "").unwrap().len(), 1);
        assert_eq!(extract_records("{\"a\":1}\n{\"a\":2}\nbasura\n", "").unwrap().len(), 2);
        assert!(extract_records(r#"{"jobs":1}"#, "jobs").is_err());
        assert!(extract_records("   ", "").unwrap().is_empty());
    }

    #[test]
    fn materializa_incremental_por_id() {
        let dir = std::env::temp_dir().join(format!("chrono-cmd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.db");
        let spec = |out: &str| CommandSpec {
            run: vec!["printf".into(), out.into()],
            options: [("id_field".to_string(), "id".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let m1 = materialize(&db, &dir, "prueba", &spec(r#"[{"id":1},{"id":2}]"#)).unwrap();
        assert_eq!((m1.records, m1.added), (2, 2));
        let m2 = materialize(&db, &dir, "prueba", &spec(r#"[{"id":2},{"id":3}]"#)).unwrap();
        assert_eq!((m2.records, m2.added), (2, 1), "solo el 3 es nuevo");
        let cache = std::fs::read_to_string(&m2.cache).unwrap();
        assert_eq!(cache.lines().count(), 3);
        assert!(m2.cache.ends_with("cmd/prueba.cmd.jsonl"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn error_claro_si_falla_o_tarda() {
        let dir = std::env::temp_dir();
        let falla = CommandSpec { run: vec!["sh".into(), "-c".into(), "echo roto >&2; exit 3".into()], ..Default::default() };
        let e = materialize(&dir.join("x.db"), &dir, "falla", &falla).err().unwrap().to_string();
        assert!(e.contains("roto"), "{e}");
        let lento = CommandSpec { run: vec!["sleep".into(), "5".into()], timeout_secs: 1, ..Default::default() };
        assert!(materialize(&dir.join("x.db"), &dir, "lento", &lento).is_err());
    }
}
