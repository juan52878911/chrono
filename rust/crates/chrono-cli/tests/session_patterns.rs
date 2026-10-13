//! Test de extremo a extremo de `session_patterns` con el binario `chrono`:
//! un repo git construido aquí (fechas fijas → shas deterministas), un hotspot,
//! una sesión de Nod con `vcs.commit` que apunta a esos commits y un retrabajo
//! posterior. Los valores esperados son exactos y se derivan a mano del
//! fixture sintético (`chrono-source-nod/testdata/nod-v1-sintetico.jsonl`) y
//! de los commits de abajo.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const T0: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z
const D: i64 = 86_400;

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir(tag: &str) -> TempDir {
    let p = std::env::temp_dir().join(format!("chrono-sp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    TempDir(std::fs::canonicalize(&p).unwrap())
}

fn git(repo: &Path, args: &[&str], at: i64) -> String {
    let date = format!("@{at} +0000");
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(repo: &Path, rel: &str, contents: &str) {
    let p = repo.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, contents).unwrap();
}

fn lines(prefix: &str, n: usize, changed: &[usize]) -> String {
    (1..=n).map(|i| if changed.contains(&i) { format!("{prefix}{i}*\n") } else { format!("{prefix}{i}\n") }).collect()
}

fn commit(repo: &Path, msg: &str, at: i64) -> String {
    git(repo, &["add", "-A"], at);
    git(repo, &["commit", "-q", "-m", msg], at);
    git(repo, &["rev-parse", "HEAD"], at)
}

/// Repo de prueba. Devuelve (sha1, sha2, sha3).
///
/// - día 0: src/hot.rs (10 líneas), src/a.rs (3), README.md (1)
/// - días 1-4: src/hot.rs cambia una línea cada día → hotspot
/// - día 10, rama feature/ABC-1-login: SHA1 (hot.rs ±1, a.rs +1 → 3 líneas),
///   SHA2 (README +1 → 1 línea); se integra en main (fast-forward)
/// - día 12, rama feature/ABC-2-wip: SHA3 (src/b.rs), sin mergear
/// - día 15, main: retrabajo de src/hot.rs (±2 → 4 líneas)
/// - día 50, main: cambia src/a.rs (fuera de la ventana de 30 días)
fn build_repo(repo: &Path) -> (String, String, String) {
    git(repo, &["init", "-q", "-b", "main"], T0);
    write(repo, "src/hot.rs", &lines("h", 10, &[]));
    write(repo, "src/a.rs", &lines("a", 3, &[]));
    write(repo, "README.md", "demo\n");
    commit(repo, "inicio", T0);
    for day in 1..=4 {
        write(repo, "src/hot.rs", &lines("h", 10, &[day as usize]));
        commit(repo, &format!("hot {day}"), T0 + day * D);
    }
    git(repo, &["checkout", "-q", "-b", "feature/ABC-1-login"], T0 + 10 * D);
    write(repo, "src/hot.rs", &lines("h", 10, &[4, 5]));
    write(repo, "src/a.rs", &lines("a", 4, &[]));
    let sha1 = commit(repo, "ABC-1 login", T0 + 10 * D + 36_000);
    write(repo, "README.md", "demo\nlogin\n");
    let sha2 = commit(repo, "ABC-1 docs", T0 + 10 * D + 37_800);
    git(repo, &["checkout", "-q", "main"], T0 + 10 * D + 37_900);
    git(repo, &["merge", "-q", "--ff-only", "feature/ABC-1-login"], T0 + 10 * D + 37_900);
    git(repo, &["checkout", "-q", "-b", "feature/ABC-2-wip"], T0 + 12 * D);
    write(repo, "src/b.rs", "b\n");
    let sha3 = commit(repo, "ABC-2 wip", T0 + 12 * D + 54_000);
    git(repo, &["checkout", "-q", "main"], T0 + 12 * D + 54_100);
    write(repo, "src/hot.rs", &lines("h", 10, &[1, 2, 4, 5]));
    commit(repo, "retrabajo hot", T0 + 15 * D);
    write(repo, "src/a.rs", &lines("a", 4, &[1]));
    commit(repo, "tarde", T0 + 50 * D);
    (sha1, sha2, sha3)
}

fn chrono(args: &[&str], cwd: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(args)
        .current_dir(cwd)
        .env("CHRONO_NOW", (T0 + 60 * D).to_string())
        .env("CHRONO_LANG", "en")
        .output()
        .unwrap();
    assert!(out.status.success(), "chrono {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn find<'a>(arr: &'a Value, key: &str, val: &str) -> &'a Value {
    arr.as_array().unwrap().iter().find(|g| g[key] == val).unwrap_or_else(|| panic!("no {key}={val}"))
}

fn approx(v: &Value, want: f64) {
    let got = v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"));
    assert!((got - want).abs() < 1e-9, "got {got}, want {want}");
}

#[test]
fn session_patterns_cruza_nod_con_git() {
    let dir = tempdir("e2e");
    let repo = dir.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let (sha1, sha2, sha3) = build_repo(&repo);
    let root = repo.to_string_lossy().to_string();

    // Fixture sintético con los shas reales: SHA1 y SHA3 abreviados a 7 (como
    // los imprime git y los emite Nod), SHA2 completo.
    let fixture = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../chrono-source-nod/testdata/nod-v1-sintetico.jsonl"),
    )
    .unwrap()
    .replace("{{ROOT}}", &root)
    .replace(&"1".repeat(40), &sha1[..7])
    .replace(&"2".repeat(40), &sha2)
    .replace(&"3".repeat(40), &sha3[..7]);
    let nod = dir.0.join("events.jsonl");
    std::fs::write(&nod, &fixture).unwrap();

    let db = repo.join(".chrono/index.db");
    let db_s = db.to_string_lossy().to_string();
    chrono(&["init", &root], &dir.0);
    chrono(&["add", nod.to_str().unwrap(), "--as", "nod", "--db", &db_s], &dir.0);

    let out: Value = serde_json::from_str(&chrono(&["session_patterns", "--db", &db_s, "--hotspots", "1"], &dir.0)).unwrap();
    assert_eq!(out["question"], "session_patterns");
    let r = &out["result"];
    assert_eq!(r["basis"]["events_scanned"], 49, "52 eventos en el fichero, 3 repetidos (dedup)");
    assert_eq!(r["basis"]["hotspots"], serde_json::json!(["src/hot.rs"]));
    assert_eq!(r["basis"]["base_branch"], "main");
    assert_eq!(r["basis"]["horizon"], "2026-03-02T00:00:00Z");

    // --- tarea ABC-1 (= sesión s-1, dos procesos) ---
    let t1 = find(&r["tasks"], "task", "ABC-1");
    assert_eq!(t1["sessions"], serde_json::json!(["s-1"]));
    assert_eq!(t1["tokens"]["input"], 116);
    assert_eq!(t1["tokens"]["output"], 77);
    assert_eq!(t1["tokens"]["cache_read"], 1308);
    assert_eq!(t1["tokens"]["cache_write"], 209);
    assert_eq!(t1["tokens"]["total"], 1710);
    assert_eq!(t1["tokens"]["subagents"], 10);
    approx(&t1["cost_usd"], 0.35); // 0.30 (proceso aaaaaaaa) + 0.05 (bbbbbbbb)
    assert_eq!(t1["time"]["agent_ms"], 87_000);
    assert_eq!(t1["time"]["waiting_human_ms"], 3000);
    assert_eq!(t1["time"]["waiting_decisions"], 2);
    assert_eq!(t1["permissions"]["asked"], 2);
    assert_eq!(t1["permissions"]["denied"], 2);
    assert_eq!(t1["permissions"]["denied_by_user"], 1);
    assert_eq!(
        t1["permissions"]["denied_by_tool"],
        serde_json::json!([{"tool": "Bash", "denied": 1}, {"tool": "Write", "denied": 1}])
    );
    assert_eq!(t1["interruptions"], 1);
    assert_eq!(t1["loops"]["count"], 1);
    assert_eq!(t1["loops"]["top"][0]["tool"], "Bash");
    assert_eq!(t1["loops"]["top"][0]["failures"], 3);
    let c = &t1["commits"];
    assert_eq!(c["delivered"], 3);
    assert_eq!(c["resolved"], 2);
    assert_eq!(c["ids"], serde_json::json!([sha1, sha2]));
    assert_eq!(c["unresolved"], serde_json::json!(["deadbeef"]));
    assert_eq!(c["touching_hotspots"], 1);
    assert_eq!(c["hotspots_touched"], serde_json::json!(["src/hot.rs"]));
    let rw = &t1["rework"];
    assert_eq!(rw["window_days"], 30);
    assert_eq!(rw["files_delivered"], 3);
    assert_eq!(rw["files_reworked"], 1);
    assert_eq!(rw["reworked"], serde_json::json!(["src/hot.rs"]));
    assert_eq!(rw["lines_delivered"], 4);
    assert_eq!(rw["lines_reworked"], 4);
    assert_eq!(rw["open_windows"], 0);
    approx(&rw["file_ratio"], 1.0 / 3.0);
    let inc = &t1["incomplete"];
    assert_eq!(inc["edited_files"], 3, "hot.rs, a.rs, docs/todo.md (notes.md fue negado)");
    assert_eq!(inc["uncommitted"], 1);
    assert_eq!(inc["examples"], serde_json::json!(["docs/todo.md"]));
    assert_eq!(inc["outside_repo"], 1);
    assert_eq!(t1["merge"]["status"], "merged");
    assert_eq!(t1["merge"]["via"], "branch");
    assert_eq!(t1["merge"]["branches"], serde_json::json!(["feature/ABC-1-login"]));
    approx(&t1["cost_per_commit"], 0.35 / 3.0);
    assert_eq!(t1["tokens_per_commit"], 570);

    // La sesión s-1 coincide con su tarea.
    let s1 = find(&r["sessions"], "session", "s-1");
    assert_eq!(s1["tasks"], serde_json::json!(["ABC-1"]));
    assert_eq!(s1["tokens"], t1["tokens"]);
    assert_eq!(s1["commits"], t1["commits"]);
    assert_eq!(s1["rework"], t1["rework"]);
    assert_eq!(s1["profiles"], serde_json::json!(["personal"]));
    assert_eq!(s1["hosts"], serde_json::json!(["claude"]));
    assert!(s1["remote_session"].is_null());
    assert_eq!(s1["produced"], serde_json::json!([]));
    assert_eq!(s1["first_at"], "2026-01-11T09:00:00.007Z", "at_epoch en ms: `at` conserva los ms de Nod");

    // --- tarea ABC-2 (sesión s-2, privacidad trabajo, rama sin mergear) ---
    let t2 = find(&r["tasks"], "task", "ABC-2");
    assert_eq!(t2["tokens"]["total"], 300);
    approx(&t2["cost_usd"], 0.02);
    assert_eq!(t2["time"]["agent_ms"], 10_000);
    assert_eq!(t2["commits"]["delivered"], 1);
    assert_eq!(t2["commits"]["resolved"], 0, "SHA3 no está en la historia de HEAD indexada");
    assert_eq!(t2["commits"]["unresolved"], serde_json::json!([&sha3[..7]]));
    assert!(t2["rework"].is_null());
    assert!(t2["incomplete"].is_null(), "sin privacidad completo no hay rutas editadas");
    assert_eq!(t2["merge"]["status"], "not_merged");
    assert_eq!(t2["merge"]["via"], "branch");
    approx(&t2["cost_per_commit"], 0.02);

    // --- resumen ---
    let s = &r["summary"];
    assert_eq!(s["sessions"], 2);
    assert_eq!(s["tasks"], 2);
    approx(&s["merge_rate"], 0.5);
    approx(&s["totals"]["cost_usd"], 0.37);
    assert_eq!(s["totals"]["commits"]["delivered"], 4);
    assert_eq!(s["totals"]["commits"]["resolved"], 2);
    assert_eq!(r["tasks_total"], 2);
    assert_eq!(r["sessions_total"], 2);
    // Sin forge (repo sin remoto): se avisa.
    assert!(r["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("no PR data")));

    // --- filtro por tarea ---
    let only: Value =
        serde_json::from_str(&chrono(&["session_patterns", "--db", &db_s, "--task", "ABC-2"], &dir.0)).unwrap();
    assert_eq!(only["result"]["tasks_total"], 1);
    assert_eq!(only["result"]["tasks"][0]["task"], "ABC-2");

    // --- sync incremental: un sobre repetido no suma; uno nuevo sí ---
    let dup_line = fixture.lines().nth(3).unwrap().to_string();
    let extra = r#"{"nod":1,"source":{"instance":"work-11223344","run":"cccccccc","host":"claude","profile":"work","privacy":"trabajo"},"events":[{"seq":11,"ts":1768316500007,"type":"usage","session":"s-2","task":"ABC-2","data":{"cost_usd":0.04}}]}"#;
    // Una sesión de OpenCode en la misma tarea que deja un PR (tool.result.links)
    // y se conecta a Remote Control.
    let oc = r#"{"nod":1,"source":{"instance":"opencode-0f0f0f0f","run":"dddddddd","host":"opencode","host_version":"1.18.34","profile":"opencode","privacy":"completo"},"events":[{"seq":1,"ts":1768400000000,"type":"session.start","session":"ses_oc","data":{"interactive":false,"surface":"cli","remote_session":"rs-1"}},{"seq":2,"ts":1768400001000,"type":"tool.result","session":"ses_oc","task":"ABC-1","data":{"tool_use_id":"c1","tool":"bash","duration_ms":800,"ok":true,"rejected":false,"output_chars":60,"links":[{"kind":"link","name":"PR #7","url":"https://github.com/o/demo/pull/7"}]}}]}"#;
    let mut f = std::fs::OpenOptions::new().append(true).open(&nod).unwrap();
    use std::io::Write;
    writeln!(f, "{dup_line}").unwrap();
    writeln!(f, "{extra}").unwrap();
    writeln!(f, "{oc}").unwrap();
    drop(f);
    chrono(&["sync", "--db", &db_s], &dir.0);
    let after: Value =
        serde_json::from_str(&chrono(&["session_patterns", "--db", &db_s, "--task", "ABC-2"], &dir.0)).unwrap();
    assert_eq!(after["result"]["basis"]["events_scanned"], 52);
    approx(&after["result"]["tasks"][0]["cost_usd"], 0.04);
    let all: Value = serde_json::from_str(&chrono(&["session_patterns", "--db", &db_s], &dir.0)).unwrap();
    let t1 = find(&all["result"]["tasks"], "task", "ABC-1");
    assert_eq!(t1["sessions"], serde_json::json!(["s-1", "ses_oc"]));
    assert_eq!(t1["hosts"], serde_json::json!(["claude", "opencode"]));
    assert_eq!(t1["produced"], serde_json::json!(["https://github.com/o/demo/pull/7"]));
    let soc = find(&all["result"]["sessions"], "session", "ses_oc");
    assert_eq!(soc["remote_session"], "rs-1");

    // --- trace: el commit de git y el vcs.commit de Nod (sha abreviado) se
    // enlazan en los dos sentidos; desde el commit también se llega a la tarea.
    let tr: Value = serde_json::from_str(&chrono(&["trace", &sha1, "--depth", "1", "--db", &db_s], &dir.0)).unwrap();
    let nodes = tr["result"]["chain"].as_array().unwrap();
    // Por el sha, solo su vcs.commit; los demás vcs.commit de ABC-1 llegan por la tarea.
    let by_sha: Vec<&Value> = nodes.iter().filter(|n| n["rel"] == "commit").collect();
    assert_eq!(by_sha.len(), 1);
    assert_eq!(by_sha[0]["kind"], "nod.vcs.commit");
    assert_eq!(by_sha[0]["id"], "nod:personal-0a1b2c3d:aaaaaaaa:27");
    assert!(nodes.iter().any(|n| n["id"] == sha2.as_str() && n["rel"] == "ticket:ABC-1"));
    let back: Value = serde_json::from_str(&chrono(
        &["trace", "nod:personal-0a1b2c3d:aaaaaaaa:27", "--depth", "1", "--db", &db_s],
        &dir.0,
    ))
    .unwrap();
    let back_nodes = back["result"]["chain"].as_array().unwrap();
    assert!(back_nodes.iter().any(|n| n["id"] == sha1.as_str() && n["rel"] == "commit"));

    // correlate sigue siendo solo por tiempo (sin campo `linked`).
    let corr: Value = serde_json::from_str(&chrono(&["correlate", &sha1, "--db", &db_s], &dir.0)).unwrap();
    assert!(corr["result"].get("linked").is_none());
}

#[test]
fn session_patterns_sin_nod_ni_git_avisa() {
    let dir = tempdir("vacio");
    let nod = dir.0.join("events.jsonl");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../chrono-source-nod/testdata/nod-v1-sesion-real.jsonl"),
        &nod,
    )
    .unwrap();
    chrono(&["init", nod.to_str().unwrap()], &dir.0);
    let db = dir.0.join(".chrono/index.db");
    let out: Value =
        serde_json::from_str(&chrono(&["session_patterns", "--db", db.to_str().unwrap()], &dir.0)).unwrap();
    let r = &out["result"];
    assert_eq!(r["basis"]["events_scanned"], 13);
    let s1 = &r["sessions"][0];
    assert_eq!(s1["session"], "de10457d-ca23-4f4b-8dce-e8fff4b4864b");
    // Valores del fixture real: 2 pasos, 1 turno de 26.943 s, 3.962 ms esperando.
    assert_eq!(s1["tokens"]["total"], 2 + 286 + 28897 + 14524 + 2 + 54 + 43421 + 325);
    approx(&s1["cost_usd"], 0.00412558);
    assert_eq!(s1["time"]["agent_ms"], 26_943);
    assert_eq!(s1["time"]["waiting_human_ms"], 3962);
    assert_eq!(s1["commits"]["delivered"], 0);
    assert!(s1["cost_per_commit"].is_null());
    assert!(s1["rework"].is_null());
    assert_eq!(s1["merge"]["status"], "unknown");
    // Sin tarea: un único grupo de tarea con `task: null`.
    assert_eq!(r["tasks_total"], 1);
    assert!(r["tasks"][0]["task"].is_null());
    let notes: Vec<&str> = r["notes"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    assert!(notes.iter().any(|n| n.contains("no git commits")));
}

/// La rama base cuenta como mergeada consigo misma: si una sesión arrancó en `main` y luego trabajó en una
/// rama que no se ha integrado, la tarea no puede salir `merged` por la base. Solo una tarea que vive
/// únicamente en la base (se trabajó directamente sobre ella) sale `merged` por ella.
#[test]
fn merge_no_cuenta_la_base_si_la_tarea_tiene_otra_rama() {
    let dir = tempdir("base");
    let repo = dir.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"], T0);
    write(&repo, "a.txt", "a\n");
    commit(&repo, "inicio", T0);
    git(&repo, &["checkout", "-q", "-b", "feature/EP-1-login"], T0 + D);
    write(&repo, "b.txt", "b\n");
    let sha_feat = commit(&repo, "EP-1 login", T0 + D);
    git(&repo, &["checkout", "-q", "main"], T0 + D + 60);
    write(&repo, "c.txt", "c\n");
    let sha_main = commit(&repo, "EP-2 directo en main", T0 + 2 * D);
    let root = repo.to_string_lossy().to_string();

    let ms = |s: i64| s * 1000;
    let env = |run: &str, session: &str, task: &str, branch: &str, sha: &str, at: i64| {
        format!(
            r#"{{"nod":1,"source":{{"instance":"i-1","run":"{run}","host":"claude","profile":"p","privacy":"completo"}},"events":[{{"seq":1,"ts":{t0},"type":"session.start","session":"{session}","data":{{"interactive":true,"repo":{{"root":"{root}","branch":"main"}}}}}},{{"seq":2,"ts":{t1},"type":"task.set","session":"{session}","task":"{task}","data":{{"task":"{task}","source":"trycore"}}}},{{"seq":3,"ts":{t2},"type":"vcs.commit","session":"{session}","task":"{task}","data":{{"sha":"{sha}","branch":"{branch}"}}}}]}}"#,
            t0 = ms(at), t1 = ms(at) + 1, t2 = ms(at) + 2
        )
    };
    let nod = dir.0.join("events.jsonl");
    std::fs::write(
        &nod,
        format!("{}\n{}\n", env("r1", "s-a", "EP-1", "feature/EP-1-login", &sha_feat, T0 + D), env("r2", "s-b", "EP-2", "main", &sha_main, T0 + 2 * D)),
    )
    .unwrap();
    let db = repo.join(".chrono/index.db");
    let db_s = db.to_string_lossy().to_string();
    chrono(&["init", &root], &dir.0);
    chrono(&["add", nod.to_str().unwrap(), "--as", "nod", "--db", &db_s], &dir.0);
    let out: Value = serde_json::from_str(&chrono(&["session_patterns", "--db", &db_s], &dir.0)).unwrap();
    let r = &out["result"];

    let t1 = find(&r["tasks"], "task", "EP-1");
    assert_eq!(t1["merge"]["branches"], serde_json::json!(["feature/EP-1-login", "main"]));
    assert_eq!(t1["merge"]["status"], "not_merged", "la sesión arrancó en main, pero su rama no está integrada");
    assert_eq!(t1["merge"]["via"], "branch");

    let t2 = find(&r["tasks"], "task", "EP-2");
    assert_eq!(t2["merge"]["branches"], serde_json::json!(["main"]));
    assert_eq!(t2["merge"]["status"], "merged", "trabajada directamente en la base");
}

/// Un turno sin `turn.end` (una sesión autónoma que se analiza desde dentro de su único turno) cuenta
/// como tiempo de agente hasta su último evento, y ese tiempo es de la tarea de ese evento. Un turno
/// cerrado sigue contando su `duration_ms`, sin sumar dos veces.
#[test]
fn turno_abierto_cuenta_hasta_su_ultimo_evento() {
    let dir = tempdir("abierto");
    let ms = |s: i64| s * 1000;
    let src = r#"{"instance":"i-1","run":"r1","host":"claude","profile":"p","privacy":"completo"}"#;
    let abierta = format!(
        r#"{{"nod":1,"source":{src},"events":[{{"seq":1,"ts":{t0},"type":"session.start","session":"s-a","data":{{"interactive":false}}}},{{"seq":2,"ts":{t0},"type":"turn.start","session":"s-a","turn":"u1","data":{{}}}},{{"seq":3,"ts":{t1},"type":"task.set","session":"s-a","task":"EP-9","data":{{"task":"EP-9","source":"trycore"}}}},{{"seq":4,"ts":{t2},"type":"turn.step","session":"s-a","turn":"u1","task":"EP-9","data":{{"index":0,"usage":{{"input":1,"output":1,"cache_read":0,"cache_write":0}}}}}},{{"seq":5,"ts":{t3},"type":"tool.call","session":"s-a","turn":"u1","task":"EP-9","data":{{"tool":"Bash","tool_use_id":"x1"}}}}]}}"#,
        t0 = ms(T0), t1 = ms(T0) + 5, t2 = ms(T0 + 5), t3 = ms(T0 + 40)
    );
    let src2 = src.replace("\"r1\"", "\"r2\"");
    let cerrada = format!(
        r#"{{"nod":1,"source":{src2},"events":[{{"seq":1,"ts":{t0},"type":"session.start","session":"s-b","data":{{"interactive":false}}}},{{"seq":2,"ts":{t0},"type":"turn.start","session":"s-b","turn":"u2","task":"EP-8","data":{{}}}},{{"seq":3,"ts":{t1},"type":"turn.end","session":"s-b","turn":"u2","task":"EP-8","data":{{"outcome":"answer","duration_ms":10000}}}}]}}"#,
        t0 = ms(T0 + 100), t1 = ms(T0 + 110)
    );
    let nod = dir.0.join("events.jsonl");
    std::fs::write(&nod, format!("{abierta}\n{cerrada}\n")).unwrap();
    chrono(&["init", nod.to_str().unwrap()], &dir.0);
    let db = dir.0.join(".chrono/index.db");
    let out: Value =
        serde_json::from_str(&chrono(&["session_patterns", "--db", db.to_str().unwrap()], &dir.0)).unwrap();
    let r = &out["result"];
    assert_eq!(find(&r["tasks"], "task", "EP-9")["time"]["agent_ms"], 40_000, "turno abierto: de turn.start al último evento");
    assert_eq!(find(&r["tasks"], "task", "EP-8")["time"]["agent_ms"], 10_000, "turno cerrado: su duration_ms");
    assert_eq!(find(&r["sessions"], "session", "s-a")["time"]["agent_ms"], 40_000);
}
