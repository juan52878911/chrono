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
mod index;
mod json;
mod query;
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
  sync [repo]          Process only the delta since the last watermark.

Queries (JSON, schema_version 2: `entity`/`id`):
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
  sync [repo]          Procesa solo el delta desde la última marca de agua.

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
    let f = parse_args(&args);
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
            if let Err(e) = index::init(&start) {
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
    if cmd == "sync" {
        let repo = f.pos.get(1).map(Path::new);
        if let Err(e) = index::sync(&db, repo) {
            fatal(e);
        }
        return;
    }

    let store = chrono_store::Store::open(&db).unwrap_or_else(|e| fatal(e));
    let window = query::Window::from_flag(f.since.as_deref()).unwrap_or_else(|e| fatal(e));

    let out = match cmd {
        "hotspots" => query::hotspots(&store, &window),
        "coupling" => {
            let entity = require_pos(&f, "coupling needs an <entity>", "coupling necesita una <entity>");
            query::coupling(&store, &window, &entity)
        }
        "owners" => {
            let prefix = require_pos(&f, "owners needs a <path>", "owners necesita una <ruta>");
            query::owners(&store, &window, &prefix)
        }
        "bugs" => query::bugs(&store, &window),
        "churn" => query::churn(&store, &window),
        "tickets" => {
            let id = require_pos(&f, "tickets needs an <id>", "tickets necesita un <id>");
            query::ticket(&store, &id)
        }
        "prs" => query::prs(&store),
        "branches" => {
            let base = f.pos.get(1).cloned().unwrap_or_default();
            let repo_path = match store.meta("repo_path") {
                Ok(Some(rp)) if !rp.is_empty() => rp,
                Ok(_) => fatal(t(
                    "no repo_path in the index; run 'chrono init' first",
                    "no hay repo_path en el índice; ejecuta 'chrono init' primero",
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
