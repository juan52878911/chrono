//! Binario `chrono` (port Rust, R1): extrae conocimiento del historial git a
//! un índice SQLite y responde preguntas acotadas en JSON.
//!
//! R1 cubre `init`, `sync` y las consultas `hotspots`, `coupling`, `owners`,
//! `churn`, `search`, `similar`. Quedan para R2: `bugs`, `tickets`, `prs`
//! (forge), `phases`, `branches`, `mcp` e `i18n`.

mod glob;
mod index;
mod json;
mod query;
mod timeutil;

use std::path::{Path, PathBuf};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const INDEX_REL: &str = ".chrono/index.db";

const USAGE: &str = "chrono {v} — conocimiento del historial de Git, acotado para una IA (port Rust, R1).

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
  churn                Líneas +/- por entidad.
  search <texto>       Busca eventos por texto (FTS5, con fallback a LIKE).
  similar <id>         Eventos casi-duplicados (por SimHash).

Opciones:
  --db RUTA            Índice explícito (si no, se auto-descubre .chrono/index.db).
  --since FECHA        Ventana temporal (p.ej. 2025-01-01).

Pendiente para R2: bugs, tickets, prs (forge), phases, branches, mcp, --lang.

Otros: version, help
";

#[derive(Default)]
struct Flags {
    db: Option<String>,
    since: Option<String>,
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
            a if a.starts_with("--") && a != "--version" && a != "--help" => {}
            a => f.pos.push(a.to_string()),
        }
        i += 1;
    }
    f
}

fn usage() -> String {
    USAGE.replace("{v}", VERSION)
}

fn fatal(msg: impl std::fmt::Display) -> ! {
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
        fatal(
            "no encontré un índice (.chrono/index.db) subiendo desde aquí.\n\
             Ejecuta 'chrono init' dentro del repo, o pasa --db RUTA",
        )
    })
}

fn require_pos(f: &Flags, msg: &str) -> String {
    f.pos.get(1).cloned().unwrap_or_else(|| fatal(msg))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let f = parse_args(&args);
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
            let entity = require_pos(&f, "coupling necesita una <entity>");
            query::coupling(&store, &window, &entity)
        }
        "owners" => {
            let prefix = require_pos(&f, "owners necesita una <ruta>");
            query::owners(&store, &window, &prefix)
        }
        "churn" => query::churn(&store, &window),
        "search" => {
            let text = require_pos(&f, "search necesita un <texto>");
            query::search(&store, &text)
        }
        "similar" => {
            let id = require_pos(&f, "similar necesita un <id>");
            query::similar(&store, &id)
        }
        "bugs" | "tickets" | "prs" | "phases" | "branches" | "mcp" => {
            fatal(format!("'{cmd}' no está en R1 del port Rust (llega en R2); usa el binario Go mientras tanto"))
        }
        other => {
            eprint!("chrono: comando desconocido {other:?}\n\n{}", usage());
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
        let f = parse_args(&args(&["hotspots", "--lang", "es"]));
        assert_eq!(f.pos, vec!["hotspots", "es"]);
    }
}
