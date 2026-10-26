//! Binario `chrono` (port Rust, R2): extrae conocimiento del historial git a
//! un índice SQLite y responde preguntas acotadas en JSON.
//!
//! Cubre `init`/`sync` (con clasificación de eventos y forge de GitHub),
//! las consultas `hotspots`, `coupling`, `owners`, `churn`, `search`,
//! `similar`, `bugs`, `tickets`, `prs`, `phases`, `branches`, y `mcp`
//! (pendiente de cablear). Mensajes de usuario en inglés por defecto,
//! español con `--lang es` / `CHRONO_LANG=es` / locale del SO.

mod glob;
mod hook;
mod i18n;
mod command;
mod config;
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
                       add cmd:<name> runs a command from .chrono/config.json (commands)
                       and indexes its JSON output; sync re-runs it and keeps only what is new.
                       --as <kind> forces the adapter (e.g. --as template, config-defined;
                       --as nod for a Nod event file that is still empty).
  sync [path]          Process only the delta since the last watermark (all sources).
  config check         Validate .chrono/config.json: each problem with its key path.

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
  sources              What is indexed: per source, events, time window, levels, top entities.
  search <text>        Search events by text (FTS5, LIKE fallback).
  similar <id>         Near-duplicate events (by SimHash).
  show <id>            Show a commit's diff live (git, bounded; --entity <path>).
  explain <file>[:A-B] Why this code is like this: commits that wrote those lines (git blame at the
                       indexed revision), fix certainty, tickets/PRs, authors and what changes with it.

Symbols (git, needs 'init --symbols'):
  Add --by symbol to hotspots/coupling/owners/churn to scope to functions
  (entity key 'file#func'), e.g. chrono hotspots --by symbol.

Log-native (multi-source). timeline, stats, top, patterns and events share the same filters:
  --since/--until (window; until exclusive), --level FATAL,ERROR (list, any case),
  --entity svc1,svc2 (the service in logs, the file in git), --source <kind|id|cmd:name>,
  --kind <kind|prefix*>, --grep <text>, --attr key:value (e.g. container:db-1), --infer-level (also lines with no level whose text says it),
  --join K=value | K=@<id> | K (relation key: a link rel or a dim; @<id> = sharing a value with that event).
  timeline             Event counts per time bucket (--bucket 1h, --by level|kind).
  stats <attr>         min/p50/p95/max/sum of a numeric attribute per group
                       (--by level|kind|entity|attr:<k>).
  top <dim>            Exact counts of the most frequent values of a dimension
                       (level|kind|actor|entity|attr:<k>), e.g. top entity --level FATAL.
  patterns             Most frequent log templates (Drain-light clustering),
                       e.g. patterns --level FATAL,ERROR,CRITICAL --limit 3.
  trace <id>           Follow an event's links chain (reverts, same ticket/PR, vcs.commit ↔ commit).
  events               Unified chronological stream across sources.
  informe [logs|codigo <file[:A-B]>]
                       START HERE: the whole picture in one call. logs: coverage, per level counts, services
                       and containers per level, top messages per level, peaks, FATAL/CRITICAL episodes and
                       what precedes them, and the next queries. codigo: explain + owners + coupling + hotspot.
  precede              Episodes of what the filters match (e.g. --grep 'fast shutdown') and what
                       precedes each one (errors/warnings in --window 2m, levels inferred from the
                       text), plus the precursors common to several episodes. --gap 2m splits
                       episodes; --before-levels FATAL,ERROR narrows the precursors (* = any, even none).
                       --join K: only precursors sharing the episode's value of K (same session, trace…).
                       Each common precursor brings expected (base rate) and lift; totals cover every episode.
  correlate <id>       Events from OTHER sources within ±Δt of an event (--delta 1h;
                       --kind commit,ci.* keeps only those kinds). --via K: instead of time, every
                       event (any source) sharing a value of relation key K, counted by source and kind.
  gaps --from A --to B --via K [--by U]
                       «X without Y»: K values on side A that no event on side B has (tests without
                       a result, sessions without a commit with --by sessionId). A side is a source
                       (name, kind or id) plus kind=, level=, entity=, grep=, attr= (space separated).
                       Only a B event at or after the unit's first A event covers it (--any-order: any).

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
                       add cmd:<nombre> ejecuta un comando de .chrono/config.json (commands)
                       e indexa su salida JSON; sync lo vuelve a ejecutar y se queda con lo nuevo.
                       --as <kind> fuerza el adaptador (p.ej. --as template, definido por config;
                       --as nod para un fichero de eventos de Nod aún vacío).
  sync [ruta]          Procesa solo el delta desde la última marca (todas las fuentes).
  config check         Valida .chrono/config.json: cada problema con la ruta de su clave.

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
  sources              Qué hay indexado: por fuente, eventos, ventana, niveles y entidades principales.
  search <texto>       Busca eventos por texto (FTS5, con fallback a LIKE).
  similar <id>         Eventos casi-duplicados (por SimHash).
  show <id>            Muestra el diff de un commit en vivo (git, acotado; --entity <ruta>).
  explain <fich>[:A-B] Por qué este código está así: commits que escribieron esas líneas (git blame en
                       la revisión indexada), certeza de arreglo, tickets/PRs, autores y qué cambia con él.

Símbolos (git, requiere 'init --symbols'):
  Añade --by symbol a hotspots/coupling/owners/churn para acotar a funciones
  (clave de entidad 'fichero#func'), p.ej. chrono hotspots --by symbol.

Log-native (multi-fuente). timeline, stats, top, patterns y events comparten filtros:
  --since/--until (ventana; until exclusivo), --level FATAL,ERROR (lista, sin mayúsculas),
  --entity svc1,svc2 (el servicio en logs, el fichero en git), --source <tipo|id|cmd:nombre>,
  --kind <tipo|prefijo*>, --grep <texto>, --attr clave:valor (p.ej. container:db-1), --infer-level (también líneas sin nivel cuyo texto lo dice),
  --join K=valor | K=@<id> | K (clave de relación: un rel de links o una dim; @<id> = comparte un valor con ese evento).
  timeline             Conteo de eventos por bucket temporal (--bucket 1h, --by level|kind).
  stats <attr>         min/p50/p95/max/suma de un atributo numérico por grupo
                       (--by level|kind|entity|attr:<c>).
  top <dim>            Recuento exacto de los valores más frecuentes de una dimensión
                       (level|kind|actor|entity|attr:<c>), p.ej. top entity --level FATAL.
  patterns             Plantillas de log más frecuentes (clustering Drain-light),
                       p.ej. patterns --level FATAL,ERROR,CRITICAL --limit 3.
  trace <id>           Sigue la cadena de links de un evento (reverts, mismo ticket/PR, vcs.commit ↔ commit).
  events               Stream cronológico unificado de todas las fuentes.
  informe [logs|codigo <fichero[:A-B]>]
                       EMPIEZA AQUÍ: el panorama en una llamada. logs: cobertura, cifras por nivel, servicios y
                       contenedores por nivel, mensajes más repetidos por nivel, picos, episodios FATAL/CRITICAL
                       y lo que los precede, y las consultas siguientes. codigo: explain + dueños + acople + hotspot.
  precede              Episodios de lo que casan los filtros (p.ej. --grep 'fast shutdown') y qué
                       precede a cada uno (errores/avisos en --window 2m, con el nivel deducido del
                       texto), más los precursores comunes a varios episodios. --gap 2m separa
                       episodios; --before-levels FATAL,ERROR acota los precursores (* = cualquiera, también sin nivel).
                       --join K: solo precursores con el mismo valor de K que el episodio (sesión, traza…).
                       Cada precursor común trae lo esperado (frecuencia base) y el lift; las cifras, de todos los episodios.
  correlate <id>       Eventos de OTRAS fuentes en ±Δt de un evento (--delta 1h;
                       --kind commit,ci.* se queda solo con esos tipos). --via K: en vez del tiempo,
                       todo evento (de cualquier fuente) que comparte un valor de la clave K, contado.
  gaps --from A --to B --via K [--by U]
                       «X sin Y»: los valores de K del lado A que ningún evento del lado B tiene
                       (pruebas sin resultado; sesiones sin commit con --by sessionId). Un lado es una
                       fuente (nombre, tipo o id) más kind=, level=, entity=, grep=, attr= (con espacios).
                       Solo cubre un evento de B en o después del primero de A en esa unidad (--any-order: cualquiera).

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
    gap: Option<String>,
    window: Option<String>,
    before_levels: Option<String>,
    by: Option<String>,
    limit: Option<String>,
    entity: Option<String>,
    attr: Option<String>,
    join: Option<String>,
    via: Option<String>,
    from: Option<String>,
    to: Option<String>,
    infer_level: bool,
    any_order: bool,
    symbols: bool,
    source: Option<String>,
    kind: Option<String>,
    level: Option<String>,
    grep: Option<String>,
    depth: Option<String>,
    interval: Option<String>,
    as_kind: Option<String>,
    preset: Option<String>,
    name: Option<String>,
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
            "--gap" if i + 1 < args.len() => {
                i += 1;
                f.gap = Some(args[i].clone());
            }
            "--window" if i + 1 < args.len() => {
                i += 1;
                f.window = Some(args[i].clone());
            }
            "--before-levels" if i + 1 < args.len() => {
                i += 1;
                f.before_levels = Some(args[i].clone());
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
            "--infer-level" => {
                f.infer_level = true;
            }
            "--any-order" => f.any_order = true,
            "--attr" if i + 1 < args.len() => {
                i += 1;
                f.attr = Some(args[i].clone());
            }
            "--join" if i + 1 < args.len() => {
                i += 1;
                f.join = Some(args[i].clone());
            }
            "--via" if i + 1 < args.len() => {
                i += 1;
                f.via = Some(args[i].clone());
            }
            "--from" if i + 1 < args.len() => {
                i += 1;
                f.from = Some(args[i].clone());
            }
            "--to" if i + 1 < args.len() => {
                i += 1;
                f.to = Some(args[i].clone());
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
            "--preset" if i + 1 < args.len() => {
                i += 1;
                f.preset = Some(args[i].clone());
            }
            "--name" if i + 1 < args.len() => {
                i += 1;
                f.name = Some(args[i].clone());
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
        "config" => {
            if f.pos.get(1).map(String::as_str) != Some("check") {
                fatal(t("usage: chrono config check", "uso: chrono config check"));
            }
            // Raíz: la del índice si lo hay (o --db); si no, el directorio actual.
            let root = f
                .db
                .as_ref()
                .map(PathBuf::from)
                .or_else(discover_db)
                .map(|db| index::index_root_of(&db))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|e| fatal(e)));
            if config::run_check(&root, &index::build_registry()) {
                std::process::exit(1);
            }
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
        "hook" => {
            // Hooks de Claude Code (intermediario): ver `hook.rs`.
            let mode = f.pos.get(1).cloned().unwrap_or_default();
            std::process::exit(hook::run(&mode));
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
        // Con comodines (entre comillas para que no los expanda la shell): una
        // fuente glob, una sola fila para todos los ficheros que casan.
        let r = if glob::is_pattern(&path) {
            index::add_glob(&db, &path, as_kind.as_deref(), f.name.as_deref(), f.preset.as_deref())
        } else {
            index::add(&db, Path::new(&path), f.symbols, as_kind.as_deref(), f.preset.as_deref())
        };
        if let Err(e) = r {
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

    let store = chrono_store::Store::open_read(&db).unwrap_or_else(|e| fatal(e));
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
        entity: f.entity.as_deref(),
        attr: f.attr.as_deref(),
        join: f.join.as_deref(),
        infer_level: f.infer_level,
        until_epoch: 0,
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
        "sources" => query::sources(&store),
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
            query::stats(&store, &window, &attr, f.by.as_deref(), f.until.as_deref(), &scope, f.limit.as_deref())
        }
        "top" => {
            let dim = require_pos(
                &f,
                "top needs a <dim> (level|kind|actor|entity|attr:<key>)",
                "top necesita una <dim> (level|kind|actor|entity|attr:<clave>)",
            );
            query::top(&store, &window, &dim, f.until.as_deref(), &scope, f.limit.as_deref())
        }
        "patterns" => query::patterns(&store, &window, f.until.as_deref(), &scope, f.limit.as_deref()),
        "events" => query::events(&store, &window, f.until.as_deref(), &scope, f.limit.as_deref()),
        "bench" => query::bench(&store, &db),
        "explain" => {
            let target = require_pos(&f, "explain needs a <file>[:FROM-TO]", "explain necesita un <fichero>[:DESDE-HASTA]");
            query::explain(&store, &target, f.limit.as_deref())
        }
        "informe" => {
            // La misma implementación que la tool MCP (texto con cabecera).
            let mut a = serde_json::Map::new();
            let put = |a: &mut serde_json::Map<String, serde_json::Value>, k: &str, v: &Option<String>| {
                if let Some(v) = v {
                    a.insert(k.into(), serde_json::Value::String(v.clone()));
                }
            };
            put(&mut a, "tipo", &f.pos.get(1).cloned());
            put(&mut a, "target", &f.pos.get(2).cloned());
            put(&mut a, "since", &f.since);
            put(&mut a, "until", &f.until);
            put(&mut a, "source", &f.source);
            put(&mut a, "entity", &f.entity);
            put(&mut a, "attr", &f.attr);
            put(&mut a, "grep", &f.grep);
            put(&mut a, "kind", &f.kind);
            match chrono_mcp::run_tool_text(&store, "informe", &serde_json::Value::Object(a)) {
                Ok(t) => {
                    println!("{t}");
                    return;
                }
                Err(e) => fatal(e),
            }
        }
        "precede" => query::precede(
            &store,
            &window,
            f.until.as_deref(),
            &scope,
            f.gap.as_deref(),
            f.window.as_deref(),
            f.before_levels.as_deref(),
            f.limit.as_deref(),
        ),
        "correlate" => {
            let id = require_pos(&f, "correlate needs an <id>", "correlate necesita un <id>");
            match f.via.as_deref() {
                Some(via) => query::correlate_via(&store, &id, via, f.kind.as_deref(), f.limit.as_deref()),
                None => query::correlate(&store, &id, f.delta.as_deref(), f.kind.as_deref(), f.limit.as_deref()),
            }
        }
        "gaps" => query::gaps(
            &store,
            &window,
            f.from.as_deref(),
            f.to.as_deref(),
            f.via.as_deref(),
            f.by.as_deref(),
            f.any_order,
            f.limit.as_deref(),
        ),
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
