//! Test de extremo a extremo de `chrono delega` con el binario: el directorio de estado
//! sintético de `chrono-source-delega/testdata/state` (formato nuevo y legado) y un
//! fichero de Nod con una sesión de orquestador (su `cwd` acaba en el dueño `orq-alfa`),
//! otra que no lo es y dos `decision.request` de delega. Las cifras se derivan a mano.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir(tag: &str) -> TempDir {
    let p = std::env::temp_dir().join(format!("chrono-delega-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(p.join(".chrono")).unwrap();
    TempDir(std::fs::canonicalize(&p).unwrap())
}

fn chrono(args: &[&str], cwd: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_chrono")).args(args).current_dir(cwd).env("CHRONO_LANG", "es").output().unwrap();
    assert!(out.status.success(), "chrono {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const T: i64 = 1_793_034_000_000; // 2026-10-26T17:00:00Z

fn nod_fixture() -> String {
    let ev = |seq: i64, dt_s: i64, ty: &str, session: &str, extra: &str| {
        format!(r#"{{"ts":{},"seq":{seq},"type":"{ty}","session":"{session}"{extra}}}"#, T + dt_s * 1000)
    };
    let claude = [
        ev(1, 0, "session.start", "o-1", r#","data":{"cwd":"/w/.claude/worktrees/orq-alfa"}"#),
        ev(2, 1, "prompt.submit", "o-1", r#","data":{"origin":"composer"}"#),
        ev(3, 2, "turn.start", "o-1", r#","turn":"t1","data":{}"#),
        ev(4, 3, "tool.call", "o-1", r#","turn":"t1","data":{"tool":"Bash"}"#),
        ev(5, 4, "turn.end", "o-1", r#","turn":"t1","data":{"outcome":"answer"}"#),
        ev(6, 10, "prompt.submit", "o-1", r#","data":{"origin":"sdk"}"#),
        ev(7, 11, "turn.start", "o-1", r#","turn":"t2","data":{}"#),
        ev(8, 12, "turn.end", "o-1", r#","turn":"t2","data":{"outcome":"answer"}"#),
        ev(9, 20, "prompt.submit", "o-1", r#","data":{"origin":"task-notification"}"#),
        ev(10, 21, "turn.start", "o-1", r#","turn":"t3","data":{}"#),
        // La herramienta de un subagente no cuenta para el turno del principal.
        ev(11, 22, "tool.call", "o-1", r#","turn":"t3","agent":"a1","data":{"tool":"Read"}"#),
        ev(12, 23, "turn.end", "o-1", r#","turn":"t3","data":{"outcome":"answer"}"#),
        ev(13, 30, "session.start", "x-1", r#","data":{"cwd":"/w/otro"}"#),
        ev(14, 31, "turn.start", "x-1", r#","turn":"t9","data":{}"#),
        ev(15, 32, "turn.end", "x-1", r#","turn":"t9","data":{"outcome":"answer"}"#),
    ];
    let job = "20261026-122251-88099";
    let delega = [
        format!(r#"{{"ts":{},"seq":1,"type":"decision.request","job":"{job}","data":{{"id":"d1","question":"¿A o B?"}}}}"#, T),
        format!(r#"{{"ts":{},"seq":2,"type":"decision.answer","job":"{job}","data":{{"id":"d1"}}}}"#, T + 40 * 60_000),
        format!(r#"{{"ts":{},"seq":3,"type":"decision.request","job":"{job}","data":{{"id":"d2"}}}}"#, T + 50 * 60_000),
    ];
    format!(
        "{{\"nod\":1,\"source\":{{\"instance\":\"claude-1\",\"run\":\"r1\",\"host\":\"claude\",\"profile\":\"claude\",\"privacy\":\"completo\"}},\"events\":[{}]}}\n\
         {{\"nod\":1,\"source\":{{\"instance\":\"delega-mac\",\"run\":\"r1\",\"host\":\"delega\",\"profile\":\"work\",\"privacy\":\"completo\"}},\"events\":[{}]}}\n",
        claude.join(","),
        delega.join(",")
    )
}

#[test]
fn informe_delega_con_carpetas_audit_y_nod() {
    let dir = tempdir("e2e");
    let db = dir.0.join(".chrono/index.db");
    let db_s = db.to_string_lossy().to_string();
    let state = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../chrono-source-delega/testdata/state");
    let nod = dir.0.join("events.jsonl");
    std::fs::write(&nod, nod_fixture()).unwrap();
    // `--as`: el fixture vive dentro del repo de chrono y `detect` de git también lo reclamaría.
    chrono(&["add", state.to_str().unwrap(), "--as", "delega", "--db", &db_s], &dir.0);
    chrono(&["add", nod.to_str().unwrap(), "--db", &db_s], &dir.0);

    let r: Value = serde_json::from_str(&chrono(&["delega", "--db", &db_s, "--format", "json"], &dir.0)).unwrap();
    // Gasto: 0,75 (nuevo) + 24,5 (legado; resumen.json manda sobre salida.json).
    assert_eq!(r["jobs"], 4);
    assert_eq!(r["jobs_ended"], 2);
    assert_eq!(r["spend"]["total_usd"], 25.25);
    assert_eq!(r["spend"]["jobs_without_cost"], 2);
    let days: Vec<(String, f64)> =
        r["spend"]["by_day"].as_array().unwrap().iter().map(|d| (d["key"].as_str().unwrap().to_string(), d["cost_usd"].as_f64().unwrap())).collect();
    assert_eq!(days, vec![("2026-10-21".into(), 24.5), ("2026-10-22".into(), 0.0), ("2026-10-26".into(), 0.75)]);
    assert_eq!(r["spend"]["by_owner"][0]["key"], "orq-beta");

    // Cuadre: el end de audit (id corto) casa con la carpeta y con su coste.
    let a = &r["audit_check"];
    assert_eq!(a["jobs_with_audit_end"], 1);
    assert_eq!(a["matched"], 1);
    assert_eq!(a["audit_without_dir"], serde_json::json!([]));
    assert_eq!(a["dir_without_audit"], serde_json::json!([]));

    // Infra: el rechazado (audit refused) y el que nunca arrancó; el legado con rc=1 no.
    let i = &r["infra_failures"];
    assert_eq!(i["considered"], 4);
    assert_eq!(i["failed"], 2);
    assert_eq!(i["rate"], 0.5);
    assert_eq!(i["by_reason"], serde_json::json!({"never_ended": 1, "refused": 1}));

    // Latencia: pregunta 12:23:10 → mensaje 12:41:10 (18 min); la del legado no tuvo respuesta.
    let q = &r["decision_latency"]["question_to_message"];
    assert_eq!((q["asked"].as_i64(), q["answered"].as_i64(), q["median_ms"].as_i64()), (Some(2), Some(1), Some(1_080_000)));
    let b = &r["decision_latency"]["blocker_to_message"];
    assert_eq!((b["asked"].as_i64(), b["answered"].as_i64()), (Some(1), Some(0)));
    let n = &r["decision_latency"]["nod_decision_request"];
    assert_eq!((n["asked"].as_i64(), n["answered"].as_i64(), n["median_ms"].as_i64()), (Some(2), Some(1), Some(2_400_000)));
    assert_eq!(n["over_30min"], 1);

    // Turnos: solo la sesión cuyo cwd acaba en un dueño; t2 y t3 sin herramientas del principal.
    let t = &r["orchestrator_turns"];
    assert_eq!(t["sessions"], serde_json::json!([{"session": "o-1", "matched_by": "owner-cwd"}]));
    assert_eq!((t["turns"].as_i64(), t["empty"].as_i64()), (Some(3), Some(2)));
    assert_eq!((t["wakeups"].as_i64(), t["empty_wakeups"].as_i64()), (Some(2), Some(2)));

    // Shas medidos por rama (integrate).
    assert_eq!(r["shas_per_branch"], serde_json::json!([{"branch": "feature/x", "shas": 2, "verified": 1, "failed": 1}]));

    // Texto: el mismo informe legible.
    let text = chrono(&["delega", "--db", &db_s], &dir.0);
    assert!(text.contains("Gasto: 25.25 $"), "{text}");
    assert!(text.contains("Fallos de infraestructura: 2 de 4 (50.0 %)"), "{text}");
    assert!(text.contains("sin herramientas 2 (66.7 %)"), "{text}");

    // Ventana: desde el 23 solo quedan los dos trabajos del 26.
    let r: Value = serde_json::from_str(&chrono(&["delega", "--db", &db_s, "--format", "json", "--since", "2026-10-23"], &dir.0)).unwrap();
    assert_eq!(r["jobs"], 2);
    assert_eq!(r["spend"]["total_usd"], 0.75);

    // `tail --since` con lista de fuentes y de kinds: una línea por evento.
    let mut child = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["tail", "--db", &db_s, "--since", "2026-10-26", "--source", "delega,nod", "--kind", "delega.notice,nod.decision.*", "--interval", "200ms"])
        .current_dir(&dir.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().unwrap();
    let out = child.wait_with_output().unwrap();
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("[delega]  notice.question 20261026-122251-88099-chrono: ¿[redacted] o Beta?")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("WARN   [nod]  decision.request 20261026-122251-88099")), "{lines:?}");
}
