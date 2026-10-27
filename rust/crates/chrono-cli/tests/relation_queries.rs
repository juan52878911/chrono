//! Consultas por clave de relación con el binario `chrono` real: `--join` en
//! `events`/`top`/`timeline`, `correlate --via` y `gaps`. Los fixtures son
//! pequeños (jsonl en un directorio temporal, una fuente glob por alias) y
//! cada cifra esperada se cuenta a mano en el comentario del fixture.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir(tag: &str) -> TempDir {
    let p = std::env::temp_dir().join(format!("chrono-relq-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    TempDir(std::fs::canonicalize(&p).unwrap())
}

fn write(base: &Path, rel: &str, contents: &str) {
    let p = base.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, contents).unwrap();
}

/// Una línea jsonl: `ts` y `msg` más los campos de `fields` (un objeto).
fn line(ts: &str, msg: &str, fields: Value) -> Value {
    let mut o = json!({"ts": ts, "msg": msg});
    for (k, v) in fields.as_object().unwrap() {
        o[k.as_str()] = v.clone();
    }
    o
}

fn jsonl(lines: &[Value]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// Raíz de prueba: `<tmp>/.chrono/config.json`, `<tmp>/.chrono/index.db` y
/// una fuente glob por alias en `<tmp>/datos/<alias>/*.jsonl`.
struct Env {
    root: TempDir,
}

impl Env {
    fn new(tag: &str, config: Value) -> Env {
        let root = tempdir(tag);
        write(&root.0, ".chrono/config.json", &config.to_string());
        Env { root }
    }
    fn dir(&self) -> &Path {
        &self.root.0
    }
    fn db(&self) -> String {
        self.dir().join(".chrono/index.db").to_string_lossy().into_owned()
    }
    /// `chrono <args> --db <db>`, sin mirar cómo sale.
    fn run(&self, args: &[&str]) -> Output {
        let db = self.db();
        let mut a = args.to_vec();
        a.extend(["--db", &db]);
        Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args(&a)
            .current_dir(self.dir())
            .env("CHRONO_LANG", "en")
            .output()
            .unwrap()
    }
    /// Falla si sale con error. Devuelve stdout parseado como JSON.
    fn q(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "chrono {args:?}: {err}");
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("chrono {args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)))
    }
    /// Falla si NO sale con error. Devuelve stdout+stderr.
    fn fail(&self, args: &[&str]) -> String {
        let out = self.run(args);
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!out.status.success(), "chrono {args:?} debía fallar: {text}");
        text
    }
    /// Escribe `datos/<alias>/<alias>.jsonl` y lo añade como fuente glob con ese alias.
    fn add_source(&self, alias: &str, lines: &[Value]) {
        write(self.dir(), &format!("datos/{alias}/{alias}.jsonl"), &jsonl(lines));
        let pattern = self.dir().join(format!("datos/{alias}/*.jsonl"));
        let pattern = pattern.to_string_lossy().into_owned();
        let out = self.run(&["add", &pattern, "--name", alias]);
        assert!(out.status.success(), "add {alias}: {}", String::from_utf8_lossy(&out.stderr));
    }
    /// Id del único evento cuyo título contiene `msg`.
    fn id_of(&self, msg: &str) -> String {
        let v = self.q(&["events", "--grep", msg]);
        let evs = v["result"]["events"].as_array().unwrap();
        assert_eq!(evs.len(), 1, "{msg}: {v}");
        evs[0]["id"].as_str().unwrap().to_string()
    }
}

/// Títulos de `result.events`, ordenados.
fn titles(v: &Value) -> Vec<String> {
    let mut t: Vec<String> =
        v["result"]["events"].as_array().unwrap().iter().map(|e| e["title"].as_str().unwrap().to_string()).collect();
    t.sort();
    t
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// `kind -> eventos` de `result.events`.
fn kinds_of_events(v: &Value) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for e in v["result"]["events"].as_array().unwrap() {
        *m.entry(e["kind"].as_str().unwrap().to_string()).or_insert(0) += 1;
    }
    m
}

/// `value -> count` de `result.top`.
fn top_map(v: &Value) -> BTreeMap<String, i64> {
    v["result"]["top"].as_array().unwrap().iter().map(|r| (r["value"].as_str().unwrap().to_string(), r["count"].as_i64().unwrap())).collect()
}

fn sample_units(v: &Value) -> Vec<String> {
    v["result"]["missing_sample"].as_array().unwrap().iter().map(|u| u["unit"].as_str().unwrap().to_string()).collect()
}

// ───────────────────────── gaps ─────────────────────────

/// `pruebas`: P-1…P-5 a las 10:00:01…10:00:05.
///   P-1 ERROR unit · P-2 ERROR unit · P-3 INFO e2e · P-4 ERROR e2e · P-5 INFO e2e
/// `resultados` (kind `res`): P-1 INFO, P-2 INFO, P-2 ERROR, P-9 ERROR.
/// Valores distintos de `prueba` en `resultados`: P-1, P-2, P-9.
fn pruebas_env(tag: &str) -> Env {
    let env = Env::new(
        tag,
        json!({
            "version": 2,
            "source_options": {"pruebas": {"kind_field": "tipo"}, "resultados": {"kind_field": "tipo"}},
            "relations": {"prueba": [
                {"source": "pruebas", "field": "prueba"},
                {"source": "resultados", "field": "prueba"}]}
        }),
    );
    let p = |n: u32, level: &str, tipo: &str| {
        line(&format!("2026-01-01T10:00:0{n}Z"), &format!("prueba-{n}"), json!({"prueba": format!("P-{n}"), "level": level, "tipo": tipo}))
    };
    env.add_source("pruebas", &[p(1, "ERROR", "unit"), p(2, "ERROR", "unit"), p(3, "INFO", "e2e"), p(4, "ERROR", "e2e"), p(5, "INFO", "e2e")]);
    let r = |n: u32, prueba: &str, level: &str| {
        line(&format!("2026-01-01T10:10:0{n}Z"), &format!("res-{n}"), json!({"prueba": prueba, "level": level, "tipo": "res"}))
    };
    env.add_source("resultados", &[r(1, "P-1", "INFO"), r(2, "P-2", "INFO"), r(3, "P-2", "ERROR"), r(4, "P-9", "ERROR")]);
    env
}

#[test]
fn gaps_sin_by_cuenta_unidades_cubiertas_y_huecos() {
    let env = pruebas_env("gaps");
    let v = env.q(&["gaps", "--from", "pruebas", "--to", "resultados", "--via", "prueba"]);
    let r = &v["result"];
    assert_eq!(r["via"], "prueba");
    assert_eq!(r["by"], Value::Null);
    assert_eq!(r["units"], 5, "{v}");
    assert_eq!(r["covered"], 2, "P-1 y P-2; P-2 dos veces cuenta una: {v}");
    assert_eq!(r["missing"], 3, "{v}");
    assert_eq!(r["to_values"], 3, "P-1, P-2 y P-9: {v}");
    // La más reciente primero: P-5, P-4, P-3.
    assert_eq!(sample_units(&v), ["P-5", "P-4", "P-3"]);
    let first = &r["missing_sample"][0];
    assert_eq!(first["events"], 1);
    assert_eq!(first["first_at"], "2026-01-01T10:00:05Z");
    assert_eq!(first["last_at"], "2026-01-01T10:00:05Z");
    assert_eq!(v["token_budget"]["truncated"], false, "3 < 25: {v}");
}

#[test]
fn gaps_limit_acota_la_muestra_pero_no_las_cifras() {
    let env = pruebas_env("gaps-limit");
    let v = env.q(&["gaps", "--from", "pruebas", "--to", "resultados", "--via", "prueba", "--limit", "2"]);
    assert_eq!(v["result"]["missing_sample"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(sample_units(&v), ["P-5", "P-4"]);
    assert_eq!(v["token_budget"]["truncated"], true, "{v}");
    assert_eq!(v["token_budget"]["max_items"], 2);
    assert_eq!(v["token_budget"]["returned"], 2);
    assert_eq!(v["result"]["units"], 5);
    assert_eq!(v["result"]["covered"], 2);
    assert_eq!(v["result"]["missing"], 3, "limit no recorta las cifras: {v}");
    assert_eq!(v["result"]["to_values"], 3);
}

#[test]
fn gaps_con_filtros_en_el_lado_cuentan_solo_lo_filtrado() {
    let env = pruebas_env("gaps-filtros");
    let gaps = |from: &str, to: &str| env.q(&["gaps", "--from", from, "--to", to, "--via", "prueba"]);
    let cifras = |v: &Value| {
        let r = &v["result"];
        (r["units"].as_i64().unwrap(), r["covered"].as_i64().unwrap(), r["missing"].as_i64().unwrap(), r["to_values"].as_i64().unwrap())
    };

    // level=ERROR en pruebas: P-1, P-2, P-4. Cubiertas P-1 y P-2; falta P-4.
    let v = gaps("pruebas level=ERROR", "resultados");
    assert_eq!(cifras(&v), (3, 2, 1, 3), "{v}");
    assert_eq!(sample_units(&v), ["P-4"]);
    assert_eq!(v["result"]["from"], "pruebas level=ERROR");

    // kind=e2e: P-3, P-4, P-5; ninguna está en resultados.
    let v = gaps("pruebas kind=e2e", "resultados");
    assert_eq!(cifras(&v), (3, 0, 3, 3), "{v}");
    assert_eq!(sample_units(&v), ["P-5", "P-4", "P-3"]);

    // kind=unit: P-1 y P-2, las dos cubiertas, sin hueco.
    let v = gaps("pruebas kind=unit", "resultados");
    assert_eq!(cifras(&v), (2, 2, 0, 3), "{v}");
    assert!(sample_units(&v).is_empty());

    // Los dos filtros a la vez: solo P-4.
    let v = gaps("pruebas level=ERROR kind=e2e", "resultados");
    assert_eq!(cifras(&v), (1, 0, 1, 3), "{v}");
    assert_eq!(sample_units(&v), ["P-4"]);

    // `source=` explícito y filtro por join en el lado.
    let v = gaps("source=pruebas join=prueba=P-1", "resultados");
    assert_eq!(cifras(&v), (1, 1, 0, 3), "{v}");

    // El filtro también vale en el lado `to`: con level=ERROR solo P-2 y P-9.
    // Todas las pruebas: solo P-2 queda cubierta.
    let v = gaps("pruebas", "resultados level=ERROR");
    assert_eq!(cifras(&v), (5, 1, 4, 2), "{v}");
    assert_eq!(sample_units(&v), ["P-5", "P-4", "P-3", "P-1"]);

    // Un filtro que no casa nada: sin unidades.
    let v = gaps("pruebas level=FATAL", "resultados");
    assert_eq!(cifras(&v), (0, 0, 0, 3), "{v}");
}

/// `sesiones` (dim `sesion`, clave `clave`):
///   S1: k1 (10:00:01), k2 (10:00:02) · S2: k3 (10:00:03), k4 (10:00:04) · S3: k5 (10:00:05)
/// `cierres`: k4 y k9.
fn sesiones_env(tag: &str) -> Env {
    let env = Env::new(
        tag,
        json!({
            "version": 2,
            "source_options": {"sesiones": {"dims": "sesion"}},
            "relations": {"clave": [
                {"source": "sesiones", "field": "clave"},
                {"source": "cierres", "field": "clave"}]}
        }),
    );
    let s = |n: u32, sesion: &str, clave: &str| line(&format!("2026-01-01T10:00:0{n}Z"), &format!("ses-{n}"), json!({"sesion": sesion, "clave": clave}));
    env.add_source("sesiones", &[s(1, "S1", "k1"), s(2, "S1", "k2"), s(3, "S2", "k3"), s(4, "S2", "k4"), s(5, "S3", "k5")]);
    env.add_source(
        "cierres",
        &[line("2026-01-01T11:00:00Z", "cierre-4", json!({"clave": "k4"})), line("2026-01-01T11:00:01Z", "cierre-9", json!({"clave": "k9"}))],
    );
    env
}

#[test]
fn gaps_con_by_cuenta_la_unidad_cubierta_si_alguna_clave_aparece() {
    let env = sesiones_env("gaps-by");
    let v = env.q(&["gaps", "--from", "sesiones", "--to", "cierres", "--via", "clave", "--by", "sesion"]);
    let r = &v["result"];
    assert_eq!(r["by"], "sesion");
    assert_eq!(r["units"], 3, "{v}");
    assert_eq!(r["covered"], 1, "solo S2 (por k4); k3 sin cerrar no la descubre: {v}");
    assert_eq!(r["missing"], 2, "{v}");
    assert_eq!(r["to_values"], 2, "k4 y k9: {v}");
    // S3 (último evento 10:00:05) antes que S1 (10:00:02).
    assert_eq!(sample_units(&v), ["S3", "S1"]);
    let s1 = &r["missing_sample"][1];
    assert_eq!(s1["events"], 2);
    assert_eq!(s1["first_at"], "2026-01-01T10:00:01Z");
    assert_eq!(s1["last_at"], "2026-01-01T10:00:02Z");

    // Sin --by la unidad es la clave: 5 claves, solo k4 cubierta.
    let v = env.q(&["gaps", "--from", "sesiones", "--to", "cierres", "--via", "clave"]);
    assert_eq!(v["result"]["units"], 5, "{v}");
    assert_eq!(v["result"]["covered"], 1);
    assert_eq!(v["result"]["missing"], 4);
}

#[test]
fn gaps_con_by_y_filtro_en_el_lado_from() {
    let env = sesiones_env("gaps-by-filtro");
    // Con grep=ses-1 solo queda el evento de k1 (S1): una unidad, sin cubrir.
    let v = env.q(&["gaps", "--from", "sesiones grep=ses-1", "--to", "cierres", "--via", "clave", "--by", "sesion"]);
    assert_eq!(v["result"]["units"], 1, "{v}");
    assert_eq!(v["result"]["covered"], 0);
    assert_eq!(sample_units(&v), ["S1"]);
    assert_eq!(v["result"]["missing_sample"][0]["events"], 1);
}

#[test]
fn gaps_errores_de_uso() {
    let env = pruebas_env("gaps-errores");
    let t = env.fail(&["gaps", "--from", "pruebas", "--to", "resultados"]);
    assert!(t.contains("--via"), "{t}");
    let t = env.fail(&["gaps", "--to", "resultados", "--via", "prueba"]);
    assert!(t.contains("--from"), "{t}");
    let t = env.fail(&["gaps", "--from", "pruebas", "--via", "prueba"]);
    assert!(t.contains("--to"), "{t}");
    // Un lado con una clave desconocida.
    let t = env.fail(&["gaps", "--from", "foo=1", "--to", "resultados", "--via", "prueba"]);
    assert!(t.contains("foo"), "{t}");
    let t = env.fail(&["gaps", "--from", "pruebas", "--to", "resultados foo=1", "--via", "prueba"]);
    assert!(t.contains("foo") && t.contains("--to"), "{t}");
    // Una fuente que no existe (un typo) no se lee como «todo es hueco».
    let t = env.fail(&["gaps", "--from", "pruebas", "--to", "resultadoz", "--via", "prueba"]);
    assert!(t.contains("resultadoz"), "{t}");
    // Dos fuentes en un lado.
    let t = env.fail(&["gaps", "--from", "pruebas resultados", "--to", "resultados", "--via", "prueba"]);
    assert!(t.contains("one source"), "{t}");
    // Claves de relación inválidas, en --via y en --by.
    let t = env.fail(&["gaps", "--from", "pruebas", "--to", "resultados", "--via", "a b"]);
    assert!(t.contains("a b"), "{t}");
    let t = env.fail(&["gaps", "--from", "pruebas", "--to", "resultados", "--via", "prueba", "--by", "x'y"]);
    assert!(t.contains("x'y"), "{t}");
}

/// `ediciones` (dim `sesion`, clave `ruta`) y `commits` (clave `ruta`):
///   ediciones: S1 a.rs 10:00 · S2 b.rs 10:00, c.rs 12:00 · S3 sin ruta 10:05 · sin sesión d.rs 10:06
///   commits:   a.rs 09:00 (ANTES de la edición) · c.rs 11:00 (después del primer evento de S2)
/// Títulos `edit N ok` (con espacios, para `grep=`).
fn orden_env(tag: &str) -> Env {
    let env = Env::new(
        tag,
        json!({
            "version": 2,
            "source_options": {"ediciones": {"dims": "sesion"}},
            "relations": {"ruta": [
                {"source": "ediciones", "field": "ruta"},
                {"source": "commits", "field": "ruta"}]}
        }),
    );
    let e = |ts: &str, n: u32, sesion: &str, ruta: &str| line(ts, &format!("edit {n} ok"), json!({"sesion": sesion, "ruta": ruta}));
    env.add_source(
        "ediciones",
        &[
            e("2026-01-01T10:00:00Z", 1, "S1", "a.rs"),
            e("2026-01-01T10:00:01Z", 2, "S2", "b.rs"),
            e("2026-01-01T12:00:00Z", 3, "S2", "c.rs"),
            line("2026-01-01T10:05:00Z", "edit 4 ok", json!({"sesion": "S3"})),
            line("2026-01-01T10:06:00Z", "edit 5 ok", json!({"ruta": "d.rs"})),
        ],
    );
    env.add_source(
        "commits",
        &[line("2026-01-01T09:00:00Z", "commit-a", json!({"ruta": "a.rs"})), line("2026-01-01T11:00:00Z", "commit-c", json!({"ruta": "c.rs"}))],
    );
    env
}

#[test]
fn gaps_por_defecto_un_to_anterior_al_from_no_cubre() {
    let env = orden_env("gaps-orden");
    let args = ["gaps", "--from", "ediciones", "--to", "commits", "--via", "ruta"];
    // Por clave: a.rs se commiteó ANTES de editarse (no cubre); c.rs, a las 11:00,
    // es anterior a su edición de las 12:00 (tampoco); b.rs y d.rs, sin commit.
    let v = env.q(&args);
    assert_eq!(v["result"]["order"], "to_after_first_from", "{v}");
    assert!(v["result"]["criterion"].as_str().unwrap().contains("--any-order"), "{v}");
    assert_eq!((v["result"]["units"].as_i64(), v["result"]["covered"].as_i64()), (Some(4), Some(0)), "{v}");
    let v = env.q(&[&args[..], &["--any-order"]].concat());
    assert_eq!(v["result"]["order"], "any", "{v}");
    assert_eq!(v["result"]["covered"], 2, "a.rs y c.rs, en cualquier orden: {v}");

    // Por sesión: el primer evento de S2 (10:00:01) es anterior al commit de c.rs
    // (11:00), así que S2 queda cubierta; S1 no (su commit es de las 09:00).
    let by = [&args[..], &["--by", "sesion"]].concat();
    let v = env.q(&by);
    assert_eq!((v["result"]["units"].as_i64(), v["result"]["covered"].as_i64()), (Some(2), Some(1)), "{v}");
    assert_eq!(sample_units(&v), ["S1"]);
    let v = env.q(&[&by[..], &["--any-order"]].concat());
    assert_eq!(v["result"]["covered"], 2, "{v}");
}

#[test]
fn gaps_lado_con_grep_con_espacios() {
    let env = orden_env("gaps-grep");
    for side in ["ediciones grep=edit 3 ok", "ediciones:grep=edit 3", "ediciones grep=\"edit 3\" kind=log", "grep=edit 3 source=ediciones"] {
        let v = env.q(&["gaps", "--from", side, "--to", "commits", "--via", "ruta", "--any-order"]);
        assert_eq!((v["result"]["units"].as_i64(), v["result"]["covered"].as_i64()), (Some(1), Some(1)), "{side:?}: {v}");
        assert_eq!(sample_units(&v), Vec::<String>::new(), "{side:?}");
    }
    // Dos fuentes siguen siendo un error, y unas comillas sin cerrar también.
    let t = env.fail(&["gaps", "--from", "ediciones commits", "--to", "commits", "--via", "ruta"]);
    assert!(t.contains("one source"), "{t}");
    let t = env.fail(&["gaps", "--from", "ediciones grep=\"edit", "--to", "commits", "--via", "ruta"]);
    assert!(t.contains("quote"), "{t}");
}

#[test]
fn gaps_until_acota_los_dos_lados() {
    let env = orden_env("gaps-until");
    // Hasta las 10:30: en from a.rs, b.rs y d.rs (c.rs es de las 12:00); en to solo
    // a.rs (el commit de c.rs es de las 11:00). Con --any-order, solo a.rs cubre.
    let v = env.q(&["gaps", "--from", "ediciones", "--to", "commits", "--via", "ruta", "--until", "2026-01-01T10:30:00Z", "--any-order"]);
    let r = &v["result"];
    assert_eq!((r["units"].as_i64(), r["covered"].as_i64(), r["to_values"].as_i64()), (Some(3), Some(1), Some(1)), "{v}");
    assert_eq!(r["until"], "2026-01-01T10:30:00Z");
    // Desde las 10:00:01 en from (S2 b.rs, S3, d.rs y c.rs); los commits, de cualquier momento.
    let v = env.q(&["gaps", "--from", "ediciones", "--to", "commits", "--via", "ruta", "--since", "2026-01-01T10:00:01Z", "--any-order"]);
    assert_eq!((v["result"]["units"].as_i64(), v["result"]["covered"].as_i64(), v["result"]["to_values"].as_i64()), (Some(3), Some(1), Some(2)), "{v}");
}

#[test]
fn gaps_cuenta_los_eventos_de_from_sin_clave() {
    let env = orden_env("gaps-sin-clave");
    // Sin --by: solo `edit 4` (S3) no tiene ruta.
    let v = env.q(&["gaps", "--from", "ediciones", "--to", "commits", "--via", "ruta"]);
    assert_eq!(v["result"]["from_without_key"], 1, "{v}");
    // Con --by sesion: además `edit 5` (d.rs), sin sesión.
    let v = env.q(&["gaps", "--from", "ediciones", "--to", "commits", "--via", "ruta", "--by", "sesion"]);
    assert_eq!(v["result"]["from_without_key"], 2, "{v}");
    assert_eq!(v["result"]["units"], 2, "S3 sin ruta y d.rs sin sesión no son unidad: {v}");
}

#[test]
fn gaps_un_from_sin_tiempo_no_deja_que_cualquier_to_cubra() {
    let env = Env::new(
        "gaps-sin-tiempo",
        json!({"version": 2, "relations": {"ruta": [
            {"source": "ediciones", "field": "ruta"},
            {"source": "commits", "field": "ruta"}]}}),
    );
    // e.rs: solo una edición sin tiempo. f.rs: una sin tiempo y otra a las 10:30.
    // Commits de los dos a las 10:00. Antes, el `from` sin tiempo (0) era el
    // «primero» y los dos commits cubrían.
    env.add_source(
        "ediciones",
        &[
            json!({"msg": "edit e", "ruta": "e.rs"}),
            json!({"msg": "edit f0", "ruta": "f.rs"}),
            line("2026-01-01T10:30:00Z", "edit f1", json!({"ruta": "f.rs"})),
        ],
    );
    env.add_source(
        "commits",
        &[line("2026-01-01T10:00:00Z", "commit-e", json!({"ruta": "e.rs"})), line("2026-01-01T10:00:00Z", "commit-f", json!({"ruta": "f.rs"}))],
    );
    let args = ["gaps", "--from", "ediciones", "--to", "commits", "--via", "ruta"];
    let v = env.q(&args);
    let r = &v["result"];
    assert_eq!(
        (r["units"].as_i64(), r["covered"].as_i64(), r["missing"].as_i64(), r["units_without_time"].as_i64()),
        (Some(2), Some(0), Some(1), Some(1)),
        "{v}"
    );
    assert_eq!(sample_units(&v), ["f.rs"], "e.rs no se puede juzgar por orden: {v}");
    // Sin orden, el tiempo no importa: las dos cubiertas.
    let v = env.q(&[&args[..], &["--any-order"]].concat());
    let r = &v["result"];
    assert_eq!((r["covered"].as_i64(), r["units_without_time"].as_i64()), (Some(2), Some(0)), "{v}");
}

// ───────────────────────── correlate --via ─────────────────────────

/// Clave `ticket` (links) en dos fuentes; cada evento puede traer `ticket` y `ticket2`.
///   tickets (kind `issue`): a1 10:00:00 AB-1+AB-2 (el ancla) · a2 09:59:50 AB-2 · a3 10:01:00 XY-5
///   commits: b1 10:00:05 AB-1+AB-2 (commit) · b2 10:00:10 AB-2 (commit) · b4 10:00:20 AB-1 (ci)
///            b3 10:02:00 XY-9 (commit)
fn tickets_env(tag: &str) -> Env {
    let env = Env::new(
        tag,
        json!({
            "version": 2,
            "source_options": {"tickets": {"kind_field": "tipo"}, "commits": {"kind_field": "tipo"}},
            "relations": {"ticket": [
                {"source": "tickets", "field": "ticket"}, {"source": "tickets", "field": "ticket2"},
                {"source": "commits", "field": "ticket"}, {"source": "commits", "field": "ticket2"}]}
        }),
    );
    env.add_source(
        "tickets",
        &[
            line("2026-01-01T10:00:00Z", "ev-a1", json!({"tipo": "issue", "ticket": "AB-1", "ticket2": "AB-2"})),
            line("2026-01-01T09:59:50Z", "ev-a2", json!({"tipo": "issue", "ticket": "AB-2"})),
            line("2026-01-01T10:01:00Z", "ev-a3", json!({"tipo": "issue", "ticket": "XY-5"})),
        ],
    );
    env.add_source(
        "commits",
        &[
            line("2026-01-01T10:00:05Z", "ev-b1", json!({"tipo": "commit", "ticket": "AB-1", "ticket2": "AB-2"})),
            line("2026-01-01T10:00:10Z", "ev-b2", json!({"tipo": "commit", "ticket": "AB-2"})),
            line("2026-01-01T10:02:00Z", "ev-b3", json!({"tipo": "commit", "ticket": "XY-9"})),
            line("2026-01-01T10:00:20Z", "ev-b4", json!({"tipo": "ci", "ticket": "AB-1"})),
        ],
    );
    env
}

fn by_source(v: &Value) -> Vec<(String, i64)> {
    v["result"]["by_source"].as_array().unwrap().iter().map(|s| (s["source_id"].as_str().unwrap().to_string(), s["count"].as_i64().unwrap())).collect()
}

fn by_kind(v: &Value) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for k in v["result"]["by_kind_level"].as_array().unwrap() {
        *m.entry(k["kind"].as_str().unwrap().to_string()).or_insert(0) += k["count"].as_i64().unwrap();
    }
    m
}

fn related_titles(v: &Value) -> Vec<String> {
    v["result"]["related"].as_array().unwrap().iter().map(|e| e["title"].as_str().unwrap().to_string()).collect()
}

#[test]
fn correlate_via_cuenta_una_vez_lo_que_comparte_dos_valores() {
    let env = tickets_env("corr");
    let anchor = env.id_of("ev-a1");
    let v = env.q(&["correlate", &anchor, "--via", "ticket"]);
    let r = &v["result"];
    assert_eq!(r["via"], "ticket");
    assert_eq!(r["values"], json!(["AB-1", "AB-2"]), "{v}");
    assert_eq!(r["anchor"]["id"], anchor.as_str());
    // Relacionados: a2 (AB-2), b1 (AB-1 y AB-2: una vez), b2 (AB-2), b4 (AB-1). El ancla no cuenta.
    assert_eq!(r["related_total"], 4, "{v}");
    // Por tiempo: a2 (-10 s), b1 (+5 s), b2 (+10 s), b4 (+20 s); sin el ancla.
    assert_eq!(related_titles(&v), ["ev-a2", "ev-b1", "ev-b2", "ev-b4"], "{v}");
    let deltas: Vec<i64> = r["related"].as_array().unwrap().iter().map(|e| e["delta_ms"].as_i64().unwrap()).collect();
    assert_eq!(deltas, [-10_000, 5_000, 10_000, 20_000]);
    assert!(r["related"].as_array().unwrap().iter().all(|e| e["id"] != anchor.as_str()));

    // by_source suma related_total: commits 3 (b1, b2, b4), tickets 1 (a2).
    let bs = by_source(&v);
    assert_eq!(bs.iter().map(|(_, n)| n).sum::<i64>(), 4, "{bs:?}");
    assert_eq!(bs.len(), 2, "{bs:?}");
    assert!(bs[0].0.contains("/datos/commits/") && bs[0].1 == 3, "{bs:?}");
    assert!(bs[1].0.contains("/datos/tickets/") && bs[1].1 == 1, "{bs:?}");
    // by_kind_level también suma el total.
    assert_eq!(by_kind(&v), BTreeMap::from([("ci".to_string(), 1), ("commit".to_string(), 2), ("issue".to_string(), 1)]));
    assert_eq!(v["token_budget"]["truncated"], false);
}

#[test]
fn correlate_via_con_kind_y_limit() {
    let env = tickets_env("corr-kind");
    let anchor = env.id_of("ev-a1");

    let v = env.q(&["correlate", &anchor, "--via", "ticket", "--kind", "commit"]);
    assert_eq!(v["result"]["related_total"], 2, "b1 y b2: {v}");
    assert_eq!(related_titles(&v), ["ev-b1", "ev-b2"]);
    let bs = by_source(&v);
    assert_eq!(bs.len(), 1, "{bs:?}");
    assert_eq!(bs[0].1, 2);
    assert_eq!(by_kind(&v), BTreeMap::from([("commit".to_string(), 2)]));

    // Lista de kinds y prefijo con `*`.
    let v = env.q(&["correlate", &anchor, "--via", "ticket", "--kind", "commit,ci"]);
    assert_eq!(v["result"]["related_total"], 3, "{v}");
    let v = env.q(&["correlate", &anchor, "--via", "ticket", "--kind", "comm*"]);
    assert_eq!(v["result"]["related_total"], 2, "{v}");
    let v = env.q(&["correlate", &anchor, "--via", "ticket", "--kind", "nada"]);
    assert_eq!(v["result"]["related_total"], 0, "{v}");
    assert!(v["result"]["related"].as_array().unwrap().is_empty());

    // --limit acota `related` y marca truncado, pero las cifras siguen siendo las exactas.
    let v = env.q(&["correlate", &anchor, "--via", "ticket", "--limit", "2"]);
    assert_eq!(related_titles(&v), ["ev-a2", "ev-b1"], "{v}");
    assert_eq!(v["result"]["related_total"], 4);
    assert_eq!(by_source(&v).iter().map(|(_, n)| n).sum::<i64>(), 4);
    assert_eq!(v["token_budget"]["truncated"], true);
}

#[test]
fn correlate_via_con_una_clave_que_no_es_relacion_falla() {
    let env = tickets_env("corr-sin");
    // `ticket2` no es una relación (es un campo de `ticket`): antes daba vacío.
    let anchor = env.id_of("ev-a2");
    let t = env.fail(&["correlate", &anchor, "--via", "ticket2"]);
    assert!(t.contains("unknown relation key"), "{t}");
    // Y un id inexistente o una clave inválida fallan con mensaje.
    let t = env.fail(&["correlate", "no-existe", "--via", "ticket"]);
    assert!(t.contains("not found"), "{t}");
    let t = env.fail(&["correlate", &anchor, "--via", "a b"]);
    assert!(t.contains("a b"), "{t}");
}

// ───────────────────────── --join ─────────────────────────

#[test]
fn join_clave_valor_devuelve_los_eventos_de_las_dos_fuentes() {
    let env = tickets_env("join");
    let v = env.q(&["events", "--join", "ticket=AB-1"]);
    assert_eq!(titles(&v), ["ev-a1", "ev-b1", "ev-b4"], "{v}");
    let v = env.q(&["events", "--join", "ticket=AB-2"]);
    assert_eq!(titles(&v), ["ev-a1", "ev-a2", "ev-b1", "ev-b2"], "{v}");
    let v = env.q(&["events", "--join", "ticket=XY-9"]);
    assert_eq!(titles(&v), ["ev-b3"]);
    // Un valor que no existe: vacío, no error.
    let v = env.q(&["events", "--join", "ticket=nada"]);
    assert!(titles(&v).is_empty(), "{v}");
    // Solo la clave: los 7 eventos que la tienen.
    let v = env.q(&["events", "--join", "ticket"]);
    assert_eq!(titles(&v).len(), 7, "{v}");
    // Se combina con los demás filtros.
    let v = env.q(&["events", "--join", "ticket=AB-2", "--kind", "commit"]);
    assert_eq!(titles(&v), ["ev-b1", "ev-b2"], "{v}");
    let v = env.q(&["events", "--join", "ticket=AB-2", "--source", "tickets"]);
    assert_eq!(titles(&v), ["ev-a1", "ev-a2"], "{v}");
}

#[test]
fn join_arroba_id_trae_los_que_comparten_algun_valor_con_el_evento() {
    let env = tickets_env("join-id");
    // a1 tiene AB-1 y AB-2: comparten b4 (AB-1), a2/b2 (AB-2), b1 (los dos) y él mismo.
    let a1 = env.id_of("ev-a1");
    let v = env.q(&["events", "--join", &format!("ticket=@{a1}")]);
    assert_eq!(titles(&v), ["ev-a1", "ev-a2", "ev-b1", "ev-b2", "ev-b4"], "{v}");
    // a3 solo tiene XY-5: nadie más lo comparte.
    let a3 = env.id_of("ev-a3");
    let v = env.q(&["events", "--join", &format!("ticket=@{a3}")]);
    assert_eq!(titles(&v), ["ev-a3"], "{v}");
    // Un id que no existe no casa nada.
    let v = env.q(&["events", "--join", "ticket=@no-existe"]);
    assert!(titles(&v).is_empty(), "{v}");
}

#[test]
fn join_prefijo_con_asterisco() {
    let env = tickets_env("join-pre");
    let v = env.q(&["events", "--join", "ticket=AB-*"]);
    assert_eq!(titles(&v), ["ev-a1", "ev-a2", "ev-b1", "ev-b2", "ev-b4"], "{v}");
    let v = env.q(&["events", "--join", "ticket=XY-*"]);
    assert_eq!(titles(&v), ["ev-a3", "ev-b3"], "{v}");
    let v = env.q(&["events", "--join", "ticket=AB-1*"]);
    assert_eq!(titles(&v), ["ev-a1", "ev-b1", "ev-b4"], "{v}");
    let v = env.q(&["events", "--join", "ticket=ZZ*"]);
    assert!(titles(&v).is_empty(), "{v}");
}

#[test]
fn top_y_timeline_con_join_cuentan_lo_mismo_que_events() {
    let env = tickets_env("join-top");
    for join in ["ticket=AB-1", "ticket=AB-2", "ticket=AB-*", "ticket"] {
        let ev = env.q(&["events", "--join", join]);
        let n = titles(&ev).len() as i64;
        let top = env.q(&["top", "kind", "--join", join]);
        assert_eq!(top_map(&top), kinds_of_events(&ev), "top kind --join {join}: {top}");
        assert_eq!(top_map(&top).values().sum::<i64>(), n);
        // Todos los eventos caen el mismo día: los buckets suman lo mismo.
        let tl = env.q(&["timeline", "--bucket", "24h", "--join", join]);
        let total: i64 = tl["result"]["buckets"].as_array().unwrap().iter().map(|b| b["count"].as_i64().unwrap()).sum();
        assert_eq!(total, n, "timeline --join {join}: {tl}");
    }
    // A mano: AB-2 son a1 y a2 (issue) y b1 y b2 (commit).
    let top = env.q(&["top", "kind", "--join", "ticket=AB-2"]);
    assert_eq!(top_map(&top), BTreeMap::from([("commit".to_string(), 2), ("issue".to_string(), 2)]));
}

#[test]
fn join_con_clave_invalida_o_desconocida_da_error_como_gaps_via_y_precede() {
    let env = tickets_env("join-mala");
    for join in ["a b=1", "a'b=1", "=1", "ticket x=AB-1", "tikcet=AB-1", "tikcet"] {
        for cmd in [&["events"][..], &["top", "kind"], &["timeline"], &["precede", "--level", "ERROR"]] {
            let t = env.fail(&[cmd, &["--join", join]].concat());
            assert!(t.contains("relation key"), "{cmd:?} --join {join:?}: {t}");
        }
    }
    // El mismo error en --via de correlate y gaps, y en el join de un lado de gaps.
    let id = env.id_of("a1");
    let t = env.fail(&["correlate", &id, "--via", "tikcet"]);
    assert!(t.contains("unknown relation key") && t.contains("tikcet"), "{t}");
    let t = env.fail(&["gaps", "--from", "tickets", "--to", "commits", "--via", "tikcet"]);
    assert!(t.contains("unknown relation key"), "{t}");
    let t = env.fail(&["gaps", "--from", "tickets join=tikcet=AB-1", "--to", "commits", "--via", "ticket"]);
    assert!(t.contains("unknown relation key"), "{t}");
    // Una clave que existe con un valor que no: vacío, sin error.
    let v = env.q(&["events", "--join", "ticket=ZZ-0"]);
    assert!(titles(&v).is_empty(), "{v}");
}

// ───────────────────────── dim y link a la vez ─────────────────────────

/// `dimsrc` guarda `ticket` como dim; `linksrc`, como link (relación).
///   dimsrc: d1 AB-1, d2 AB-2 · linksrc: l1 AB-1, l2 AB-3
fn dim_y_link_env(tag: &str) -> Env {
    let env = Env::new(
        tag,
        json!({
            "version": 2,
            "source_options": {"dimsrc": {"dims": "ticket", "kind_field": "tipo"}, "linksrc": {"kind_field": "tipo"}},
            "relations": {"ticket": [{"source": "linksrc", "field": "ticket"}]}
        }),
    );
    env.add_source(
        "dimsrc",
        &[
            line("2026-01-01T10:00:01Z", "d1", json!({"tipo": "dimk", "ticket": "AB-1"})),
            line("2026-01-01T10:00:02Z", "d2", json!({"tipo": "dimk", "ticket": "AB-2"})),
        ],
    );
    env.add_source(
        "linksrc",
        &[
            line("2026-01-01T10:00:03Z", "l1", json!({"tipo": "linkk", "ticket": "AB-1"})),
            line("2026-01-01T10:00:04Z", "l2", json!({"tipo": "linkk", "ticket": "AB-3"})),
        ],
    );
    env
}

#[test]
fn join_une_la_clave_como_dim_y_como_link() {
    let env = dim_y_link_env("dimlink");
    let v = env.q(&["events", "--join", "ticket=AB-1"]);
    assert_eq!(titles(&v), ["d1", "l1"], "{v}");
    let v = env.q(&["events", "--join", "ticket=AB-*"]);
    assert_eq!(titles(&v), ["d1", "d2", "l1", "l2"], "{v}");
    let v = env.q(&["events", "--join", "ticket"]);
    assert_eq!(titles(&v).len(), 4, "{v}");
    let d1 = env.id_of("d1");
    let v = env.q(&["events", "--join", &format!("ticket=@{d1}")]);
    assert_eq!(titles(&v), ["d1", "l1"], "{v}");
    let top = env.q(&["top", "kind", "--join", "ticket=AB-1"]);
    assert_eq!(top_map(&top), BTreeMap::from([("dimk".to_string(), 1), ("linkk".to_string(), 1)]));
}

#[test]
fn correlate_y_gaps_por_una_clave_que_es_dim_en_un_lado_y_link_en_el_otro() {
    let env = dim_y_link_env("dimlink-q");
    let d1 = env.id_of("d1");
    let v = env.q(&["correlate", &d1, "--via", "ticket"]);
    assert_eq!(v["result"]["values"], json!(["AB-1"]), "{v}");
    assert_eq!(v["result"]["related_total"], 1);
    assert_eq!(related_titles(&v), ["l1"]);
    // gaps: dimsrc tiene AB-1 y AB-2; linksrc, AB-1 y AB-3. Solo AB-1 cubierta.
    let v = env.q(&["gaps", "--from", "dimsrc", "--to", "linksrc", "--via", "ticket"]);
    let r = &v["result"];
    assert_eq!((r["units"].as_i64(), r["covered"].as_i64(), r["missing"].as_i64(), r["to_values"].as_i64()), (Some(2), Some(1), Some(1), Some(2)), "{v}");
    assert_eq!(sample_units(&v), ["AB-2"]);
}

// ───────────────────────── prefijos con comodines de LIKE ─────────────────────────

/// Valores de `k`: `v_1`, `vX2`, `V_3`. `--join` con prefijo usa `LIKE` sin escapar
/// (ver el informe): estos dos tests describen lo que cabría esperar.
fn comodines_env(tag: &str) -> Env {
    let env = Env::new(tag, json!({"version": 2, "relations": {"k": [{"source": "etiquetas", "field": "k"}]}}));
    let e = |n: u32, k: &str| line(&format!("2026-01-01T10:00:0{n}Z"), k, json!({"k": k}));
    env.add_source("etiquetas", &[e(1, "v_1"), e(2, "vX2"), e(3, "V_3")]);
    env
}

#[test]
fn join_prefijo_el_guion_bajo_es_literal() {
    let env = comodines_env("like-us");
    // `v_*` casa `v_1` y no `vX2`: el prefijo es literal (con LIKE, `_` sería «un carácter»).
    assert_eq!(titles(&env.q(&["events", "--join", "k=v_1"])), ["v_1"]);
    let v = env.q(&["events", "--join", "k=v_*"]);
    assert_eq!(titles(&v), ["v_1"], "el `_` del prefijo no es un comodín: {v}");
}

#[test]
fn join_prefijo_distingue_mayusculas_como_el_valor_exacto() {
    let env = comodines_env("like-case");
    // `k=V_3` es exacto y sensible a mayúsculas; el prefijo `v*` debería serlo igual
    // (LIKE de SQLite no lo es para ASCII).
    assert_eq!(titles(&env.q(&["events", "--join", "k=v_3"])), Vec::<String>::new());
    let v = env.q(&["events", "--join", "k=v*"]);
    assert_eq!(titles(&v), strs(&["vX2", "v_1"]), "{v}");
}
