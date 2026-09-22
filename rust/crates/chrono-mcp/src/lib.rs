//! `chrono-mcp` — servidor MCP por stdio (JSON-RPC 2.0, una línea = un
//! mensaje). Paridad de protocolo con el Go `internal/mcp/mcp.go`: lo lanza
//! el cliente (Claude) y muere con la sesión, no es un daemon.
//!
//! Framing: cada línea de stdin es un objeto JSON-RPC 2.0 completo; cada
//! respuesta se escribe como una única línea JSON en stdout (sin pretty
//! print del sobre; el `content[].text` de `tools/call` sí va indentado, para
//! que lo lea cómodamente un humano/LLM). Los logs y errores de E/S van a
//! stderr, nunca a stdout (contaminaría el framing).
//!
//! El servidor arranca aunque no haya índice: `tools/list` funciona siempre y
//! `tools/call` responde con un texto pidiendo `chrono init` en vez de
//! fallar. Si `db` es `None`, se autodescubre `.chrono/index.db` subiendo
//! desde el cwd (igual que el CLI).

mod since;
mod tools;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use chrono_store::Store;

/// Error de `chrono-mcp`: no exige un tipo concreto a los llamadores.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "chrono";

/// Nombre del fichero de índice relativo a la raíz del repo, igual que el CLI.
const INDEX_REL: &str = ".chrono/index.db";

/// Estado del servidor a lo largo de la sesión: el índice (si lo hay).
/// Se resuelve una vez al arrancar; `tools/call` lo consulta en cada llamada.
pub struct State {
    store: Option<Store>,
}

impl State {
    /// Para tests: arranca ya con (o sin) un `Store` construido a mano.
    fn with_store(store: Option<Store>) -> State {
        State { store }
    }

    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }
}

/// Corre el bucle de lectura/respuesta sobre stdin/stdout hasta EOF o error
/// de E/S. Nunca falla por falta de índice: `db` ausente o inválido deja
/// `State` sin store y el servidor sigue sirviendo `tools/list` etc.
pub fn serve(db: Option<PathBuf>) -> Result<()> {
    let state = State::with_store(open_or_discover(db));

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle(&line, &state) {
            stdout.write_all(response.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Abre el índice explícito, o lo autodescubre subiendo desde el cwd. Nunca
/// propaga error: si no hay fichero, o si `Store::open` falla, se queda sin
/// store (se loguea a stderr) en vez de abortar el arranque del servidor.
fn open_or_discover(db: Option<PathBuf>) -> Option<Store> {
    let path = db.or_else(discover_db)?;
    if !path.is_file() {
        return None;
    }
    match Store::open(&path) {
        Ok(store) => Some(store),
        Err(err) => {
            eprintln!(
                "chrono-mcp: no se pudo abrir el índice {}: {err}",
                path.display()
            );
            None
        }
    }
}

/// Sube desde el cwd buscando `.chrono/index.db` (mismo algoritmo que
/// `discoverDB` en `cmd/chrono/main.go`).
fn discover_db() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(INDEX_REL);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Despacha un mensaje JSON-RPC ya leído (una línea de stdin). Devuelve
/// `None` cuando no hay que responder: JSON ilegible (se ignora la línea,
/// igual que el Go) o notificación (`notifications/initialized`, o un método
/// desconocido sin `id`).
fn handle(line: &str, state: &State) -> Option<String> {
    let req: Value = serde_json::from_str(line).ok()?;
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let empty_params = json!({});
    let params = req.get("params").unwrap_or(&empty_params);

    match method {
        "initialize" => Some(respond(id, initialize_result())),
        "notifications/initialized" => None,
        "ping" => Some(respond(id, json!({}))),
        "tools/list" => Some(respond(id, json!({"tools": tools::tool_defs()}))),
        "tools/call" => Some(match tools::call_tool(state, params) {
            Ok(result) => respond(id, result),
            Err(err) => respond_err(id, -32000, &err.to_string()),
        }),
        _ => {
            // Solo se responde si es una request de verdad (trae `id`); una
            // notificación desconocida se ignora en silencio.
            id.map(|id| respond_err(Some(id), -32601, &format!("método no soportado: {method}")))
        }
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
    })
}

fn respond(id: Option<Value>, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result}).to_string()
}

fn respond_err(id: Option<Value>, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {"code": code, "message": message},
    })
    .to_string()
}

#[cfg(test)]
mod tests;
