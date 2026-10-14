//! Binario `chrono` (port Rust, R2): extrae conocimiento del historial git a
//! un índice SQLite y responde preguntas acotadas en JSON.
//!
//! Cubre `init`/`sync` (con clasificación de eventos y forge de GitHub),
//! las consultas `hotspots`, `coupling`, `owners`, `churn`, `search`,
//! `similar`, `bugs`, `tickets`, `prs`, `phases`, `branches`, y `mcp`
//! (pendiente de cablear). Mensajes de usuario en inglés por defecto,
//! español con `--lang es` / `CHRONO_LANG=es` / locale del SO.

mod glob;
mod i18n;
mod command;
mod index;
mod json;
mod query;
mod symbols;
mod timeutil;

use std::path::{Path, PathBuf};

use i18n::t;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const INDEX_REL: &str = ".chrono/index.db";

const USAGE_EN: &str = "chrono {v} — Git history knowledge, bounded for an AI (Rust port).

Usage:
  chrono init [repo-path]   Build the repo index (creates .chrono/) and ingest.
  chrono <query> [args]     Auto-discovers .chrono/index.db upward from cwd.

Index:
  init [repo]          Zero-config: detect the repo, create .chrono/, ingest all.
                       --symbols also indexes per-hunk function history (git only).
  add <path>           Add another source (git repo or log file) to the index.
                       --as <kind> forces the adapter (e.g. --as template, config-defined;
                       --as nod for a Nod event file that is still empty).
  sync [path]          Process only the delta since the last watermark (all sources).

Queries (JSON, schema_version 2: `entity`/`id`; add --compact for a plain-text table of `result`
only, without metadata: about 2-5x smaller and byte-identical for the same index, meant for agents):
  hotspots             Entities that change most and weigh most.
  coupling <entity>    What changes together with this entity.
  owners <path>        Ownership by author and bus factor.
  bugs                 Where fixes concentrate + bug categories.
  churn                Lines +/- per entity.
  tickets <id>         Commits/files/PRs linked to a ticket.
  prs                  Forge pull requests (state, merge, bug by label).
  branches [base]      Branch status vs base: ahead/behind, merged, stale, authors.
  phases               Project phases (tags/releases).
  search <text>        Search events by text (FTS5, LIKE fallback).
  similar <id>         Near-duplicate events (by SimHash).
  show <id>            Show a commit's diff live (git, bounded; --entity <path>).

Symbols (git, needs 'init --symbols'):
  Add --by symbol to hotspots/coupling/owners/churn to scope to functions
  (entity key 'file#func'), e.g. chrono hotspots --by symbol.

Log-native (multi-source):
  timeline             Event counts per time bucket (--bucket 1h, --by level|kind;
                       --source/--kind/--level/--grep narrow them).
  stats <attr>         min/p50/p95/max/sum of a numeric attribute per group
                       (--by level|kind|entity|attr:<k>; same filters as timeline).
  top <dim>            Most frequent values of a dimension (level|kind|actor|entity|attr:<k>);
                       --source, --grep and --level narrow the events counted.
  patterns             Most frequent log templates (Drain-light clustering);
                       --source <kind|id>, --grep <text> and --level narrow them.
  trace <id>           Follow an event's links chain (reverts, same ticket/PR, vcs.commit ↔ commit).
  events               Unified chronological stream across sources (--source/--kind/--level/--grep).
  correlate <id>       Events from OTHER sources within ±Δt of an event (--delta 1h;
                       --kind commit,ci.* keeps only those kinds).

Process × outcome (Nod sessions + git):
  session_patterns     Per task and per session: tokens, cost, agent vs human
                       wait time, denied permissions, interruptions, loops,
                       commits delivered (vcs.commit) and how many touch hotspots,
                       30-day rework, incomplete changes, merge status, cost per
                       commit. Needs a nod source (chrono add events.jsonl --as nod).
                       --task ID, --session ID, --hotspots N (top N, default 25).
                       A commit's Nod session: chrono trace <sha>.

Diagnostics:
  tail                 Follow new events live across sources (tail -f; --interval 2s).
  bench                Measure query latencies + index stats on this index.

Integration:
  mcp                  MCP server (wired separately).

Options:
  --db PATH            Explicit index (otherwise auto-discovers .chrono/index.db).
  --since DATE         Time window (e.g. 2025-01-01).
  --lang en|es         Message language (default: auto from OS locale).

Other: version, help
";

const USAGE_ES: &str = "chrono {v} — conocimiento del historial de Git, acotado para una IA (port Rust).

Uso:
  chrono init [ruta-repo]   Prepara el índice del repo (crea .chrono/) e ingiere.
  chrono <consulta> [args]  Auto-descubre .chrono/index.db subiendo desde el cwd.

Índice:
  init [repo]          Cero-config: detecta el repo, crea .chrono/ e ingiere todo.
  add <ruta>           Añade otra fuente (repo git o fichero de log) al índice.
                       --as <kind> fuerza el adaptador (p.ej. --as template, definido por config;
                       --as nod para un fichero de eventos de Nod aún vacío).
  sync [ruta]          Procesa solo el delta desde la última marca (todas las fuentes).

Consulta (JSON, schema_version 2: `entity`/`id`):
  hotspots             Entidades que más cambian y más pesan.
  coupling <entity>    Qué cambia junto a esta entidad.
  owners <ruta>        Propiedad por autor y bus factor.
  bugs                 Dónde se concentran los fixes + categorías.
  churn                Líneas +/- por entidad.
  tickets <id>         Commits/ficheros/PRs ligados a un ticket.
  prs                  PRs del forge (estado, merge, si es bug por label).
  branches [base]      Estado de ramas vs base: ahead/behind, mergeada, stale, autores.
  phases               Fases del proyecto (etiquetas).
  search <texto>       Busca eventos por texto (FTS5, con fallback a LIKE).
  similar <id>         Eventos casi-duplicados (por SimHash).
  show <id>            Muestra el diff de un commit en vivo (git, acotado; --entity <ruta>).

Símbolos (git, requiere 'init --symbols'):
  Añade --by symbol a hotspots/coupling/owners/churn para acotar a funciones
  (clave de entidad 'fichero#func'), p.ej. chrono hotspots --by symbol.

Log-native (multi-fuente):
  timeline             Conteo de eventos por bucket temporal (--bucket 1h, --by level|kind;
                       --source/--kind/--level/--grep los acotan).
  stats <attr>         min/p50/p95/max/suma de un atributo numérico por grupo
                       (--by level|kind|entity|attr:<c>; mismos filtros que timeline).
  top <dim>            Valores más frecuentes de una dimensión (level|kind|actor|entity|attr:<c>);
                       --source, --grep y --level acotan los eventos que cuenta.
  patterns             Plantillas de log más frecuentes (clustering Drain-light);
                       --source <tipo|id>, --grep <texto> y --level las acotan.
  trace <id>           Sigue la cadena de links de un evento (reverts, mismo ticket/PR, vcs.commit ↔ commit).
  events               Stream cronológico unificado de todas las fuentes (--source/--kind/--level/--grep).
  correlate <id>       Eventos de OTRAS fuentes en ±Δt de un evento (--delta 1h;
                       --kind commit,ci.* se queda solo con esos tipos).

Proceso × resultado (sesiones de Nod + git):
  session_patterns     Por tarea y por sesión: tokens, costo, tiempo del agente
                       frente a espera del humano, permisos negados,
                       interrupciones, bucles, commits entregados (vcs.commit) y
                       cuántos tocan hotspots, retrabajo a 30 días, cambios
                       incompletos, estado de merge, costo por commit. Requiere
                       una fuente nod (chrono add events.jsonl --as nod).
                       --task ID, --session ID, --hotspots N (top N, por defecto 25).
                       La sesión de Nod de un commit: chrono trace <sha>.

Diagnóstico:
  tail                 Sigue eventos nuevos en vivo (tail -f; --interval 2s).
  bench                Mide latencias de consulta + estadísticas del índice.

Integración:
  mcp                  Servidor MCP (se cablea aparte).

Opciones:
  --db RUTA            Índice explícito (si no, se auto-descubre .chrono/index.db).
  --since FECHA        Ventana temporal (p.ej. 2025-01-01).
  --lang en|es         Idioma de los mensajes (por defecto: auto según el SO).

Otros: version, help
";

#[derive(Default)]
struct Flags {
    db: Option<String>,
    since: Option<String>,
    lang: Option<String>,
    until: Option<String>,
    bucket: Option<String>,
    delta: Option<String>,
    by: Option<String>,
    limit: Option<String>,
    entity: Option<String>,
    symbols: bool,
    source: Option<String>,
    kind: Option<String>,
    level: Option<String>,
    grep: Option<String>,
    depth: Option<String>,
    interval: Option<String>,
    as_kind: Option<String>,
    task: Option<String>,
    session: Option<String>,
    hotspots: Option<String>,
    compact: bool,
    pos: Vec<String>,
}

/// Saca el comando (1er positional) y su argumento (2º), aceptando flags en
/// cualquier posición: `chrono --since X hotspots` o `chrono hotspots --since X`.
fn parse_args(args: &[String]) -> Flags {
    let mut f = Flags::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" if i + 1 < args.len() => {
                i += 1;
                f.db = Some(args[i].clone());
            }
            "--since" if i + 1 < args.len() => {
                i += 1;
                f.since = Some(args[i].clone());
            }
            "--lang" if i + 1 < args.len() => {
                i += 1;
                f.lang = Some(args[i].clone());
            }
            "--until" if i + 1 < args.len() => {
                i += 1;
                f.until = Some(args[i].clone());
            }
            "--bucket" if i + 1 < args.len() => {
                i += 1;
                f.bucket = Some(args[i].clone());
            }
            "--delta" if i + 1 < args.len() => {
                i += 1;
                f.delta = Some(args[i].clone());
            }
            "--by" if i + 1 < args.len() => {
                i += 1;
                f.by = Some(args[i].clone());
            }
            "--limit" if i + 1 < args.len() => {
                i += 1;
                f.limit = Some(args[i].clone());
            }
            "--entity" if i + 1 < args.len() => {
                i += 1;
                f.entity = Some(args[i].clone());
            }
            "--symbols" => f.symbols = true,
            "--compact" => f.compact = true,
            "--source" if i + 1 < args.len() => {
                i += 1;
                f.source = Some(args[i].clone());
            }
            "--kind" if i + 1 < args.len() => {
                i += 1;
                f.kind = Some(args[i].clone());
            }
            "--level" if i + 1 < args.len() => {
                i += 1;
                f.level = Some(args[i].clone());
            }
            "--grep" if i + 1 < args.len() => {
                i += 1;
                f.grep = Some(args[i].clone());
            }
            "--depth" if i + 1 < args.len() => {
                i += 1;
                f.depth = Some(args[i].clone());
            }
            "--interval" if i + 1 < args.len() => {
                i += 1;
                f.interval = Some(args[i].clone());
            }
            "--as" if i + 1 < args.len() => {
                i += 1;
                f.as_kind = Some(args[i].clone());
            }
            "--task" if i + 1 < args.len() => {
                i += 1;
                f.task = Some(args[i].clone());
            }
            "--session" if i + 1 < args.len() => {
                i += 1;
                f.session = Some(args[i].clone());
            }
            "--hotspots" if i + 1 < args.len() => {
                i += 1;
                f.hotspots = Some(args[i].clone());
            }
            a if a.starts_with("--") && a != "--version" && a != "--help" => {}
            a => f.pos.push(a.to_string()),
        }
        i += 1;
    }
    f
}

fn usage() -> String {
    let raw = t(USAGE_EN, USAGE_ES);
    raw.replace("{v}", VERSION)
}

/// Ejecuta las fuentes de comando de la config (todas, o solo `only`) y
/// devuelve el caché de `only`. Un comando que falla avisa y no para el resto:
/// el sync de las demás fuentes no debe depender de que `gh` tenga red.
fn refresh_commands(db: &Path, only: Option<&str>) -> Option<PathBuf> {
    let root = index::index_root_of(db);
    let cfg = chrono_classify_rules::load(&root);
    let mut found = None;
    for (name, spec) in &cfg.commands {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        // En un sync completo solo se refrescan los comandos ya añadidos.
        if only.is_none() && !command::cache_path(db, name).exists() {
            continue;
        }
        match command::materialize(db, &root, name, spec) {
            Ok(m) => {
                eprintln!("{}", t(
                    format!("  cmd:{name}: {} records, {} new", m.records, m.added),
                    format!("  cmd:{name}: {} registros, {} nuevos", m.records, m.added),
                ));
                if only.is_some() {
                    found = Some(m.cache);
                }
            }
            Err(e) => {
                eprintln!("chrono: cmd:{name}: {e}");
                if only.is_some() {
                    fatal(e.to_string());
                }
            }
        }
    }
    found
}

fn fatal(msg: impl std::fmt::Display) -> ! {
    // "chrono: error:" no se traduce (paridad literal con el Go).
    eprintln!("chrono: error: {msg}");
    std::process::exit(1)
}

/// Sube desde el cwd buscando `.chrono/index.db` (como git con `.git`).
fn discover_db() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let p = dir.join(INDEX_REL);
        if p.is_file() {
            return Some(p);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn resolve_db(f: &Flags) -> PathBuf {
    if let Some(db) = &f.db {
        return PathBuf::from(db);
    }
    discover_db().unwrap_or_else(|| {
        fatal(t(
            "no index (.chrono/index.db) found upward from here.\n\
             Run 'chrono init' inside the repo, or pass --db PATH",
            "no encontré un índice (.chrono/index.db) subiendo desde aquí.\n\
             Ejecuta 'chrono init' dentro del repo, o pasa --db RUTA",
        ))
    })
}

fn require_pos(f: &Flags, msg_en: &str, msg_es: &str) -> String {
    f.pos.get(1).cloned().unwrap_or_else(|| fatal(t(msg_en, msg_es)))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut f = parse_args(&args);
    i18n::init(f.lang.as_deref());

    let Some(cmd) = f.pos.first().map(String::as_str) else {
        eprint!("{}", usage());
        std::process::exit(2);
    };

    match cmd {
        "version" | "--version" | "-v" => {
            println!("chrono {VERSION}");
            return;
        }
        "help" | "--help" | "-h" => {
            print!("{}", usage());
            return;
        }
        "init" => {
            let start = f
                .pos
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|e| fatal(e)));
            if let Err(e) = index::init(&start, f.symbols, f.as_kind.as_deref(), f.db.as_deref().map(Path::new)) {
                fatal(e);
            }
            return;
        }
        "mcp" => {
            // Servidor MCP por stdio: usa --db si se pasó, o autodescubre.
            if let Err(e) = chrono_mcp::serve(f.db.as_ref().map(PathBuf::from)) {
                fatal(e);
            }
            return;
        }
        _ => {}
    }

    // Comandos que necesitan un índice existente.
    let db = resolve_db(&f);
    // `--source cmd:<nombre>` es el alias de la fuente de un comando: su id real
    // es la ruta absoluta del caché (`jsonl:/…/cmd/<nombre>.cmd.jsonl`).
    if let Some(name) = f.source.as_deref().and_then(|s| s.strip_prefix(command::PREFIX)) {
        let cache = command::cache_path(&db, name);
        let abs = std::fs::canonicalize(&cache).unwrap_or(cache);
        f.source = Some(format!("jsonl:{}", abs.display()));
    }
    if cmd == "sync" {
        // Las fuentes de comando se refrescan ANTES del sync: vuelven a
        // ejecutarse y añaden a su caché lo nuevo, que el sync procesa como el
        // delta de cualquier fichero. `sync cmd:<nombre>` refresca solo esa.
        let arg = f.pos.get(1).cloned();
        let only = arg.as_deref().and_then(|a| a.strip_prefix(command::PREFIX));
        let synced_path;
        let repo = match only {
            Some(name) => {
                synced_path = refresh_commands(&db, Some(name));
                synced_path.as_deref()
            }
            None => {
                refresh_commands(&db, None);
                arg.as_deref().map(Path::new)
            }
        };
        if let Err(e) = index::sync(&db, repo) {
            fatal(e);
        }
        return;
    }
    if cmd == "add" {
        let path = require_pos(&f, "add needs a <path> (a repo or a log file)", "add necesita una <ruta> (un repo o un fichero de log)");
        // `add cmd:<nombre>`: ejecuta el comando de la config y añade su caché.
        let (path, as_kind) = match path.strip_prefix(command::PREFIX) {
            Some(name) => match refresh_commands(&db, Some(name)) {
                Some(cache) => (cache.to_string_lossy().into_owned(), Some("jsonl".to_string())),
                None => fatal(t(
                    format!("no command {name:?} in .chrono/config.json (\"commands\")"),
                    format!("no hay un comando {name:?} en .chrono/config.json (\"commands\")"),
                )),
            },
            None => (path, f.as_kind.clone()),
        };
        if let Err(e) = index::add(&db, Path::new(&path), f.symbols, as_kind.as_deref()) {
            fatal(e);
        }
        return;
    }
    if cmd == "tail" {
        let interval_ms = match f.interval.as_deref() {
            None | Some("") => 2000,
            Some(s) => timeutil::parse_duration(s)
                .unwrap_or_else(|| fatal(t(format!("invalid --interval: {s:?}"), format!("--interval no válido: {s:?}")))),
        };
        if let Err(e) = index::tail(
            &db,
            interval_ms,
            f.source.as_deref(),
            f.kind.as_deref(),
            f.level.as_deref(),
            f.grep.as_deref(),
        ) {
            fatal(e);
        }
        return;
    }

    let store = chrono_store::Store::open(&db).unwrap_or_else(|e| fatal(e));
    let window = query::Window::from_flag(f.since.as_deref()).unwrap_or_else(|e| fatal(e));

    // `--by symbol` cambia el alcance de las consultas de entidades a símbolos
    // (S1); por defecto (o `--by file`) se excluyen los símbolos.
    let symbols_only = f.by.as_deref() == Some("symbol");
    // Filtros comunes de las consultas log-native que los aceptan (timeline, stats).
    let scope = chrono_metrics::EventScope {
        source: f.source.as_deref(),
        grep: f.grep.as_deref(),
        level: f.level.as_deref(),
        kind: f.kind.as_deref(),
    };
    let out = match cmd {
        "hotspots" => query::hotspots(&store, &window, symbols_only),
        "coupling" => {
            let entity = require_pos(&f, "coupling needs an <entity>", "coupling necesita una <entity>");
            query::coupling(&store, &window, &entity, symbols_only)
        }
        "owners" => {
            let prefix = require_pos(&f, "owners needs a <path>", "owners necesita una <ruta>");
            query::owners(&store, &window, &prefix, symbols_only)
        }
        "bugs" => query::bugs(&store, &window),
        "churn" => query::churn(&store, &window, symbols_only),
        "tickets" => {
            let id = require_pos(&f, "tickets needs an <id>", "tickets necesita un <id>");
            query::ticket(&store, &id)
        }
        "prs" => query::prs(&store),
        "branches" => {
            let base = f.pos.get(1).cloned().unwrap_or_default();
            let repo_path = match query::git_repo_path(&store) {
                Ok(Some(rp)) => rp,
                Ok(None) => fatal(t(
                    "no git source in the index; 'branches' needs a git repo",
                    "no hay fuente git en el índice; 'branches' necesita un repo git",
                )),
                Err(e) => fatal(e),
            };
            query::branches(&store, Path::new(&repo_path), &base)
        }
        "phases" => query::phases(&store),
        "search" => {
            let text = require_pos(&f, "search needs a <text>", "search necesita un <texto>");
            query::search(&store, &text)
        }
        "similar" => {
            let id = require_pos(&f, "similar needs an <id>", "similar necesita un <id>");
            query::similar(&store, &id)
        }
        "timeline" => query::timeline(&store, &window, f.until.as_deref(), f.bucket.as_deref(), f.by.as_deref(), &scope, f.limit.as_deref()),
        "stats" => {
            let attr = require_pos(&f, "stats needs an <attr> (a numeric attribute, e.g. duration_s)", "stats necesita un <attr> (un atributo numérico, p.ej. duration_s)");
            query::stats(&store, &window, &attr, f.by.as_deref(), &scope, f.limit.as_deref())
        }
        "top" => {
            let dim = require_pos(
                &f,
                "top needs a <dim> (level|kind|actor|entity|attr:<key>)",
                "top necesita una <dim> (level|kind|actor|entity|attr:<clave>)",
            );
            query::top(&store, &window, &dim, f.source.as_deref(), f.grep.as_deref(), f.level.as_deref(), f.limit.as_deref())
        }
        "patterns" => query::patterns(
            &store,
            &window,
            f.source.as_deref(),
            f.grep.as_deref(),
            f.level.as_deref(),
            f.limit.as_deref(),
        ),
        "events" => query::events(
            &store,
            &window,
            f.until.as_deref(),
            f.source.as_deref(),
            f.kind.as_deref(),
            f.level.as_deref(),
            f.grep.as_deref(),
            f.limit.as_deref(),
        ),
        "bench" => query::bench(&store, &db),
        "correlate" => {
            let id = require_pos(&f, "correlate needs an <id>", "correlate necesita un <id>");
            query::correlate(&store, &id, f.delta.as_deref(), f.kind.as_deref(), f.limit.as_deref())
        }
        "trace" => {
            let id = require_pos(&f, "trace needs an <id>", "trace necesita un <id>");
            query::trace(&store, &id, f.depth.as_deref(), f.limit.as_deref())
        }
        "session_patterns" => query::session_patterns(
            &store,
            &window,
            f.task.as_deref(),
            f.session.as_deref(),
            f.hotspots.as_deref(),
            f.limit.as_deref(),
        ),
        "show" => {
            let id = require_pos(&f, "show needs an <id>", "show necesita un <id>");
            query::show(&store, &id, f.entity.as_deref())
        }
        other => {
            eprint!(
                "{}",
                t(
                    format!("chrono: unknown command {other:?}\n\n{}", usage()),
                    format!("chrono: comando desconocido {other:?}\n\n{}", usage()),
                )
            );
            std::process::exit(2);
        }
    };
    match out {
        Ok(j) if f.compact => print!("{}", j.compact()),
        Ok(j) => print!("{}", j.pretty()),
        Err(e) => fatal(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_en_cualquier_posicion() {
        let f = parse_args(&args(&["--since", "2025-01-01", "coupling", "src/a.rs", "--db", "x.db"]));
        assert_eq!(f.pos, vec!["coupling", "src/a.rs"]);
        assert_eq!(f.since.as_deref(), Some("2025-01-01"));
        assert_eq!(f.db.as_deref(), Some("x.db"));
    }

    #[test]
    fn flags_desconocidas_se_ignoran() {
        let f = parse_args(&args(&["hotspots", "--foo", "bar"]));
        assert_eq!(f.pos, vec!["hotspots", "bar"]);
    }

    #[test]
    fn lang_se_reconoce_en_cualquier_posicion() {
        let f = parse_args(&args(&["--lang", "es", "hotspots"]));
        assert_eq!(f.pos, vec!["hotspots"]);
        assert_eq!(f.lang.as_deref(), Some("es"));

        let f = parse_args(&args(&["branches", "main", "--lang", "en"]));
        assert_eq!(f.pos, vec!["branches", "main"]);
        assert_eq!(f.lang.as_deref(), Some("en"));
    }
}
