//! Tests del adaptador Nod con el fixture REAL de Nod (`testdata/nod-v1-sesion-real.jsonl`,
//! copiado de `nod/test/fixtures/`) y uno sintético que cumple el esquema
//! (`testdata/nod-v1-sintetico.jsonl`: dos ejecuciones de una sesión, tarea,
//! `vcs.commit`, permisos negados, un bucle, un turno abortado, un tipo
//! desconocido y un sobre entregado dos veces).

use super::*;
use chrono_core::{Cursor, Event, Link};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata").join(name)
}

fn drain(cur: &mut Box<dyn Cursor>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(e) = cur.next().unwrap() {
        out.push(e);
    }
    out
}

fn open(path: &Path) -> Box<dyn Cursor> {
    NodSource::new().open(path, None, &SourceConfig::default()).unwrap()
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new(contents: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("chrono-nod-test-{}-{n}.jsonl", std::process::id()));
        fs::write(&p, contents).unwrap();
        Temp(p)
    }
    fn append(&self, s: &str) {
        let mut f = fs::OpenOptions::new().append(true).open(&self.0).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn detect_reconoce_sobres_y_rechaza_jsonl_generico() {
    let src = NodSource::new();
    assert_eq!(src.detect(&testdata("nod-v1-sesion-real.jsonl")), 90);
    assert_eq!(src.detect(&testdata("nod-v1-sintetico.jsonl")), 90);
    let generic = Temp::new("{\"ts\": 1, \"msg\": \"hola\"}\n");
    assert_eq!(src.detect(&generic.0), 0);
    let otra_version = Temp::new("{\"nod\": 2, \"source\": {}, \"events\": []}\n");
    assert_eq!(src.detect(&otra_version.0), 0);
    assert_eq!(src.detect(&std::env::temp_dir()), 0);
}

#[test]
fn fixture_real_mapea_los_13_eventos() {
    let path = testdata("nod-v1-sesion-real.jsonl");
    let mut cur = open(&path);
    let evs = drain(&mut cur);
    assert_eq!(evs.len(), 13);

    let first = &evs[0];
    assert_eq!(first.id, "nod:personal-c1254137:cdfed7fb:1");
    assert_eq!(first.kind, "nod.session.start");
    // `ts` de Nod (ms) es `at_epoch` tal cual; `at` lleva los ms.
    assert_eq!(first.at_epoch, 1_791_840_778_028);
    assert_eq!(first.at, "2026-10-12T21:32:58.028Z");
    assert_eq!(first.actor.name, "personal");
    assert_eq!(first.actor.key, "personal-c1254137");
    assert_eq!(first.attrs["session"], "de10457d-ca23-4f4b-8dce-e8fff4b4864b");
    assert_eq!(first.attrs["cwd"], "/Users/dev/proyecto");
    assert!(first.source_id.starts_with("nod:"));

    // El texto libre no entra en el índice (ni en attrs, ni en title/body).
    let prompt = evs.iter().find(|e| e.kind == "nod.prompt.submit").unwrap();
    assert_eq!(prompt.title, "prompt.submit composer");
    assert!(prompt.body.is_empty());
    assert!(!prompt.attrs.contains_key("text"));
    assert_eq!(prompt.attrs["edit.typing_ms"], "508");

    // tool.call → touch de la tool.
    let call = evs.iter().find(|e| e.kind == "nod.tool.call").unwrap();
    assert_eq!(call.touches.len(), 1);
    assert_eq!(call.touches[0].entity, "tool/Bash");
    assert_eq!(call.touches[0].entity_type, "tool");
    assert_eq!(call.attrs["verb"], "sleep");

    // turn.step: usage aplanado.
    let step = evs.iter().find(|e| e.kind == "nod.turn.step").unwrap();
    assert_eq!(step.attrs["usage.cache_read"], "28897");
    assert_eq!(step.attrs["usage.input"], "2");
    assert_eq!(step.attrs["tools"], "Bash");
    assert_eq!(step.attrs["turn"], "cc377d46-25aa-4caa-96fc-18702c492153");

    let decision = evs.iter().find(|e| e.kind == "nod.permission.decision").unwrap();
    assert_eq!(decision.title, "permission.decision Bash allow by user");
    assert_eq!(decision.attrs["wait_ms"], "3962");

    let usage = evs.iter().find(|e| e.kind == "nod.usage").unwrap();
    assert_eq!(usage.attrs["cost_usd"], "0.00412558");
    assert_eq!(usage.attrs["context.percent"], "4");

    let m = cur.manifest();
    assert_eq!(m["format"], "nod-v1");
    assert_eq!(m["nod_envelopes"], "8");
    assert_eq!(m["nod_skipped_events"], "0");
}

#[test]
fn sintetico_dedup_por_instance_run_seq_e_ignora_desconocidos() {
    let path = testdata("nod-v1-sintetico.jsonl");
    let mut cur = open(&path);
    let evs = drain(&mut cur);
    // 52 eventos conocidos en el fichero (uno de los sobres llega dos veces) y
    // 1 de tipo desconocido (`foo.bar`), que no se emite.
    assert_eq!(evs.len(), 52);
    let ids: BTreeSet<&str> = evs.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids.len(), 49, "el sobre repetido produce los MISMOS ids (el store los ignora)");
    assert!(evs.iter().all(|e| e.kind != "nod.foo.bar"));
    assert_eq!(cur.manifest()["nod_skipped_events"], "1");

    // vcs.commit → link al commit y a la tarea.
    let commit = evs.iter().find(|e| e.kind == "nod.vcs.commit").unwrap();
    assert_eq!(commit.title, "vcs.commit 111111111111 (feature/ABC-1-login)");
    assert!(commit.links.iter().any(|l| l.rel == "commit" && l.target == "1111111111111111111111111111111111111111"));
    assert!(commit.links.iter().any(|l| l.rel == "ticket" && l.target == "ABC-1"));
    assert_eq!(commit.attrs["task"], "ABC-1");

    // Write/Edit con privacidad `completo` → touch del fichero editado.
    let edit = evs.iter().find(|e| e.kind == "nod.tool.call" && e.attrs.get("tool_use_id").map(String::as_str) == Some("u1")).unwrap();
    let keys: Vec<&str> = edit.touches.iter().map(|t| t.entity.as_str()).collect();
    assert_eq!(keys, vec!["tool/Edit", "edit:{{ROOT}}/src/hot.rs"]);
    assert_eq!(edit.touches[1].entity_type, "edited-file");

    // Sin privacidad `completo` no hay `input` y por tanto no hay fichero.
    let edit_trabajo = evs.iter().find(|e| e.kind == "nod.tool.call" && e.attrs["privacy"] == "trabajo").unwrap();
    assert_eq!(edit_trabajo.touches.len(), 1);

    // Severidad derivada.
    let loop_fail = evs.iter().filter(|e| e.kind == "nod.tool.result" && e.level == "ERROR").count();
    assert_eq!(loop_fail, 3);
    let aborted = evs.iter().find(|e| e.kind == "nod.turn.end" && e.attrs["outcome"] == "aborted").unwrap();
    assert_eq!(aborted.level, "WARN");

    // task.set: data.task choca con el campo común `task` → `data.task`.
    let ts = evs.iter().find(|e| e.kind == "nod.task.set").unwrap();
    assert_eq!(ts.attrs["task"], "ABC-1");
    assert_eq!(ts.attrs["data.task"], "ABC-1");
    assert_eq!(ts.attrs["source"], "branch");
}

#[test]
fn sync_incremental_y_linea_a_medio_escribir() {
    let real = fs::read_to_string(testdata("nod-v1-sesion-real.jsonl")).unwrap();
    let lines: Vec<&str> = real.lines().collect();
    let f = Temp::new(&format!("{}\n{}\n", lines[0], lines[1]));
    let src = NodSource::new();
    let cfg = SourceConfig::default();

    let mut cur = src.open(&f.0, None, &cfg).unwrap();
    assert_eq!(drain(&mut cur).len(), 3, "sobre 1: session.start; sobre 2: prompt.submit + turn.start");
    let wm = cur.watermark();
    assert_eq!(wm.kind, "nod");

    // Se añade un sobre entero y medio sobre (sin `\n`): solo entra el entero.
    let half = &lines[3][..lines[3].len() / 2];
    f.append(&format!("{}\n{half}", lines[2]));
    let mut cur = src.open(&f.0, Some(wm), &cfg).unwrap();
    let evs = drain(&mut cur);
    assert_eq!(evs.len(), 2, "el sobre 3 trae tool.call + turn.step");
    assert_eq!(evs[0].kind, "nod.tool.call");
    let wm2 = cur.watermark();

    // Se completa la línea: el siguiente sync la lee entera.
    f.append(&format!("{}\n", &lines[3][lines[3].len() / 2..]));
    let mut cur = src.open(&f.0, Some(wm2), &cfg).unwrap();
    let evs = drain(&mut cur);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, "nod.permission.request");
    assert_eq!(cur.manifest()["nod_invalid_lines"], "0");
}

#[test]
fn rotacion_da_diverged() {
    let real = fs::read_to_string(testdata("nod-v1-sesion-real.jsonl")).unwrap();
    let f = Temp::new(&real);
    let src = NodSource::new();
    let cfg = SourceConfig::default();
    let mut cur = src.open(&f.0, None, &cfg).unwrap();
    drain(&mut cur);
    let wm = cur.watermark();
    fs::write(&f.0, real.lines().next().unwrap()).unwrap();
    match src.open(&f.0, Some(wm), &cfg) {
        Err(CoreError::Diverged(_)) => {}
        _ => panic!("esperaba Diverged tras truncar"),
    }
}

#[test]
fn lineas_invalidas_se_saltan_sin_abortar() {
    let real = fs::read_to_string(testdata("nod-v1-sesion-real.jsonl")).unwrap();
    let first = real.lines().next().unwrap();
    let f = Temp::new(&format!("no es json\n{{\"nod\":2,\"source\":{{}},\"events\":[]}}\n{first}\n"));
    let mut cur = open(&f.0);
    assert_eq!(drain(&mut cur).len(), 1);
    assert_eq!(cur.manifest()["nod_invalid_lines"], "2");
}

#[test]
fn fixture_real_con_vcs_commit_abreviado() {
    // Sesión real de Nod (fase 2): tarea deducida de la rama y un commit con
    // el sha abreviado tal como lo imprime git.
    let path = testdata("nod-v1-commit-real.jsonl");
    assert_eq!(NodSource::new().detect(&path), 90);
    let mut cur = open(&path);
    let evs = drain(&mut cur);
    assert_eq!(evs.len(), 15);
    let start = &evs[0];
    assert_eq!(start.attrs["repo.branch"], "feature/DEMO-7-prueba");
    assert_eq!(start.attrs["repo.root"], "/Users/dev/repo-demo");
    let ts = evs.iter().find(|e| e.kind == "nod.task.set").unwrap();
    assert_eq!(ts.attrs["task"], "DEMO-7");
    assert_eq!(ts.attrs["source"], "branch");
    let c = evs.iter().find(|e| e.kind == "nod.vcs.commit").unwrap();
    assert_eq!(c.id, "nod:personal-c1254137:5d88443d:10");
    assert_eq!(c.title, "vcs.commit a18ffb6 (feature/DEMO-7-prueba)");
    assert_eq!(c.attrs["files"], "1");
    let mut links: Vec<(&str, &str)> = c.links.iter().map(|l| (l.rel.as_str(), l.target.as_str())).collect();
    links.sort();
    assert_eq!(links, vec![("commit", "a18ffb6"), ("ticket", "DEMO-7")]);
}

#[test]
fn opencode_real_host_tools_en_minusculas_y_permisos_del_humano() {
    // Salida Nod v1 de la sesión real de OpenCode 1.18.34 en la TUI
    // (`nod/test/fixtures/opencode-1.18.34-tui.jsonl` reproducida por el
    // adaptador de OpenCode de Nod): tarea de la rama, subagente, tres permisos
    // del humano (uno negado) y tools en minúsculas.
    let path = testdata("nod-v1-opencode-tui.jsonl");
    assert_eq!(NodSource::new().detect(&path), 90);
    let mut cur = open(&path);
    let evs = drain(&mut cur);
    assert_eq!(cur.manifest()["nod_skipped_events"], "0");
    assert_eq!(cur.manifest()["nod_invalid_lines"], "0");
    assert!(evs.iter().all(|e| e.attrs["host"] == "opencode" && e.actor.name == "opencode"));
    let start = &evs[0];
    assert_eq!(start.kind, "nod.session.start");
    assert_eq!(start.attrs["surface"], "terminal");
    assert_eq!(start.attrs["repo.branch"], "feature/NOD-42-prueba");
    let ts = evs.iter().find(|e| e.kind == "nod.task.set").unwrap();
    assert!(ts.links.iter().any(|l| l.rel == "ticket" && l.target == "NOD-42"));
    let tools: BTreeSet<&str> =
        evs.iter().flat_map(|e| e.touches.iter()).map(|t| t.entity.as_str()).collect();
    assert_eq!(tools, BTreeSet::from(["tool/bash", "tool/glob", "tool/task"]));
    let decisions: Vec<&str> =
        evs.iter().filter(|e| e.kind == "nod.permission.decision").map(|e| e.title.as_str()).collect();
    assert_eq!(
        decisions,
        vec!["permission.decision bash allow by user", "permission.decision bash deny by user", "permission.decision bash allow by user"]
    );
    // La negada por el humano: tool.result rechazado → WARN, no ERROR.
    assert!(evs.iter().any(|e| e.kind == "nod.tool.result" && e.attrs["rejected"] == "true" && e.level == "WARN"));
    // Orden por ms (varios eventos comparten instante: el orden fino es `seq`).
    assert!(evs.windows(2).all(|w| w[0].at_epoch <= w[1].at_epoch));
}

#[test]
fn opencode_write_edit_y_links_de_tool_result_y_remote_session() {
    // Campos nuevos de Nod v1: `tool.result.links` (lo que produjo la tool),
    // `remote_session` (Remote Control) y las tools de edición de OpenCode
    // (`write`/`edit` con `filePath`).
    // Un sobre por línea: los eventos se escriben en varias líneas por
    // legibilidad y se unen aquí.
    let env = |host: &str, evs: &str| {
        let evs = evs.replace('\n', " ");
        format!(
            r#"{{"nod":1,"source":{{"instance":"{host}-00000001","run":"aaaaaaaa","host":"{host}","profile":"{host}","privacy":"completo"}},"events":[{evs}]}}"#
        )
    };
    let oc = env(
        "opencode",
        r#"{"seq":1,"ts":1791844310740,"type":"tool.call","session":"s","data":{"tool_use_id":"c1","tool":"write","input":{"filePath":"/r/informe.md","content":"x"}}},
           {"seq":2,"ts":1791844310800,"type":"tool.result","session":"s","data":{"tool_use_id":"c1","tool":"write","duration_ms":3,"ok":true,"rejected":false,"output_chars":0,
             "links":[{"kind":"file","name":"informe.md","path":"/r/informe.md"}]}},
           {"seq":3,"ts":1791844310900,"type":"tool.call","session":"s","data":{"tool_use_id":"c2","tool":"edit","input":{"filePath":"/r/src/a.rs"}}}"#,
    );
    let cl = env(
        "claude",
        r#"{"seq":1,"ts":1791844311000,"type":"session.start","session":"t","data":{"interactive":true,"remote_session":"https://claude.ai/code/session_X"}},
           {"seq":2,"ts":1791844312000,"type":"tool.result","session":"t","data":{"tool_use_id":"b1","tool":"Bash","duration_ms":900,"ok":true,"rejected":false,"output_chars":80,
             "links":[{"kind":"link","name":"PR #42","url":"https://github.com/o/r/pull/42"},{"kind":"link","name":"PR #42","url":"https://github.com/o/r/pull/42"},{"kind":"otro"}]}},
           {"seq":3,"ts":1791844313000,"type":"usage","session":"t","data":{"cost_usd":0.5,"remote_session":"https://claude.ai/code/session_X"}}"#,
    );
    let f = Temp::new(&format!("{oc}\n{cl}\n"));
    let evs = drain(&mut open(&f.0));
    assert_eq!(evs.len(), 6);

    let write = &evs[0];
    let keys: Vec<&str> = write.touches.iter().map(|t| t.entity.as_str()).collect();
    assert_eq!(keys, vec!["tool/write", "edit:/r/informe.md"]);
    let edit = &evs[2];
    let keys: Vec<&str> = edit.touches.iter().map(|t| t.entity.as_str()).collect();
    assert_eq!(keys, vec!["tool/edit", "edit:/r/src/a.rs"]);

    let produced = &evs[1];
    assert_eq!(produced.links, vec![Link { rel: "file".into(), target: "/r/informe.md".into() }]);
    assert!(!produced.attrs.contains_key("links"));

    let pr = &evs[4];
    // Deduplicado y sin los de `kind` desconocido.
    assert_eq!(pr.links, vec![Link { rel: "url".into(), target: "https://github.com/o/r/pull/42".into() }]);

    assert_eq!(evs[3].attrs["remote_session"], "https://claude.ai/code/session_X");
    assert_eq!(evs[5].attrs["remote_session"], "https://claude.ai/code/session_X");
}

/// `include_command` pone el comando en el título de `tool.call`, solo con la opción activa y privacidad
/// `completo`; recortado a la primera línea y determinista.
#[test]
fn include_command_pone_el_comando_en_el_titulo() {
    let env = |privacy: &str| {
        format!(
            r#"{{"nod":1,"source":{{"instance":"i","run":"r-{privacy}","host":"claude","profile":"p","privacy":"{privacy}"}},"events":[{{"seq":1,"ts":1791844310740,"type":"tool.call","session":"s","data":{{"tool_use_id":"c1","tool":"Bash","input":{{"command":"bash .claude/hooks/build/slice-ops.sh gate wiring_verified pass\nsegunda línea"}}}}}}]}}"#
        )
    };
    let t = Temp::new(&format!("{}\n{}\n", env("completo"), env("trabajo")));
    let mut opts = SourceConfig::default();
    opts.options.insert("include_command".into(), "true".into());
    let titles = |cfg: &SourceConfig| -> Vec<String> {
        let mut cur = NodSource::new().open(&t.0, None, cfg).unwrap();
        drain(&mut cur).into_iter().map(|e| e.title).collect()
    };
    let con = titles(&opts);
    assert_eq!(con, vec![
        "tool.call Bash: bash .claude/hooks/build/slice-ops.sh gate wiring_verified pass".to_string(),
        "tool.call Bash".to_string(), // sin privacidad completo no se toca
    ]);
    assert_eq!(con, titles(&opts), "determinista");
    assert!(titles(&SourceConfig::default()).iter().all(|t| t == "tool.call Bash"), "apagada por defecto");
}
