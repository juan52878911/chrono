//! Definición de los tools MCP y su ejecución (`tools/call`).
//!
//! Convención de salida v2 (`entity`/`id`, no `path`/`sha`): mismos nombres de
//! campo que `chrono-cli/src/query.rs` para las consultas que comparten
//! (hotspots, coupling, owners, churn, search, similar); el resto sigue el
//! mismo criterio para las que no tiene el CLI.

use std::path::Path;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::since::since_to_epoch;
use crate::{Result, State};

const HOTSPOTS_LIMIT: usize = 25;
const COUPLING_LIMIT: usize = 25;
const CHURN_LIMIT: usize = 25;
const SEARCH_LIMIT: usize = 25;
const SIMILAR_LIMIT: usize = 15;
const BUGS_LIMIT: usize = 15;
const PRS_LIMIT: i64 = 30;
const COUPLING_MIN_SUPPORT: i64 = 2;
const SIMILAR_MAX_DIST: u32 = 4;

fn str_prop(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

/// Texto de la propiedad `since`, compartido por todos los tools que aceptan
/// ventana temporal.
fn since_prop() -> Value {
    str_prop("Ventana temporal ISO, p.ej. 2025-01-01 (opcional)")
}

/// Filtros comunes de las consultas sobre eventos (`top`, `patterns`,
/// `timeline`, `events`, `stats`): las mismas reglas en todas (ver
/// `chrono_metrics::EventScope`), mezcladas con las propiedades propias.
fn with_scope(own: Value) -> Value {
    let mut props = json!({
        "since": str_prop("Inicio de la ventana, ISO (2026-10-13 o 2026-10-13T22:00:00Z) (opcional)"),
        "until": str_prop("Fin de la ventana, ISO, exclusivo (opcional)"),
        "level": str_prop("Nivel o niveles separados por coma, sin distinguir mayúsculas: FATAL,ERROR,CRITICAL (opcional)"),
        "entity": str_prop("Entidad o entidades exactas separadas por coma: el servicio en logs (postgres,product_service), el fichero en git (opcional)"),
        "source": str_prop("Fuente: tipo (git, jsonl, cmd…) o id/alias (cmd:loki) (opcional)"),
        "kind": str_prop("Tipo de evento: commit, log, ci.run… o prefijo con * (ci.*) (opcional)"),
        "grep": str_prop("Subcadena del mensaje/título (opcional)"),
        "infer_level": json!({"type": "boolean", "description": "Incluir las líneas SIN nivel de la fuente cuyo texto lo dice (FATAL:, ERROR:…). Para ENCONTRAR, no para contar: las cifras por nivel se dan sin esto (por defecto false)"}),
        "format": str_prop("text (por defecto: tabla compacta con cabecera total/coverage/truncated/index/q) | json"),
        "depth": str_prop("counts (solo cifras) | summary (sin sub-tablas por fila) | full. Por defecto summary en precede, full en el resto"),
        "attr": str_prop("Atributo clave:valor, p. ej. container:crm_aura_sandbox_postgres (valor con * = prefijo; varios con coma) (opcional)"),
        "join": str_prop("Clave de relación (relations, link o dim): K=valor | K=prefijo* | K=@<id> (comparte valor con ese evento) | K (opcional)")
    });
    if let (Some(p), Some(o)) = (props.as_object_mut(), own.as_object()) {
        for (k, v) in o {
            p.insert(k.clone(), v.clone());
        }
    }
    props
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({"type": "object", "properties": properties});
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    json!({"name": name, "description": description, "inputSchema": schema})
}

/// Los tools expuestos por el servidor (paridad con el Go + `branches`/`prs`).
pub fn tool_defs() -> Vec<Value> {
    vec![
        tool(
            "hotspots",
            "Ficheros que más cambian y más pesan.",
            json!({"since": since_prop()}),
            &[],
        ),
        tool(
            "coupling",
            "Qué ficheros cambian junto a uno dado.",
            json!({"file": str_prop("Ruta del fichero"), "since": since_prop()}),
            &["file"],
        ),
        tool(
            "owners",
            "Propiedad por autor y bus factor de una ruta.",
            json!({"path": str_prop("Prefijo de ruta"), "since": since_prop()}),
            &["path"],
        ),
        tool(
            "churn",
            "Líneas añadidas/borradas por fichero.",
            json!({"since": since_prop()}),
            &[],
        ),
        tool(
            "search",
            "Busca eventos por texto completo (FTS5): commits, logs, despliegues. Solo texto: para filtrar por nivel, servicio o ventana usa events/top/patterns.",
            json!({"query": str_prop("Texto a buscar")}),
            &["query"],
        ),
        tool(
            "similar",
            "Commits casi-duplicados de un SHA (por SimHash).",
            json!({"sha": str_prop("SHA (o id) del commit")}),
            &["sha"],
        ),
        tool(
            "bugs",
            "Zonas donde se concentran los fixes (relacional).",
            json!({"since": since_prop()}),
            &[],
        ),
        tool(
            "tickets",
            "Commits y ficheros ligados a un ticket.",
            json!({"id": str_prop("Id del ticket, p.ej. PROJ-123 o #45")}),
            &["id"],
        ),
        tool(
            "phases",
            "Fases del proyecto (etiquetas/releases).",
            json!({}),
            &[],
        ),
        tool(
            "explain",
            "Por qué un trozo de código está así: commits que escribieron esas líneas (git blame en la revisión indexada), si eran arreglos (yes/maybe/no), tickets/PRs, autores y qué cambia con él. Determinista.",
            json!({"target": str_prop("fichero o fichero:DESDE-HASTA (líneas 1-based, inclusivas)"), "limit": {"type": "integer", "description": "Máximo de commits (por defecto 15)"}}),
            &["target"],
        ),
        tool(
            "branches",
            "Estado de ramas vs base: ahead/behind, mergeada, stale, autores.",
            json!({"base": str_prop("Rama base (opcional; por defecto main/master)")}),
            &[],
        ),
        tool(
            "prs",
            "Pull requests del forge (estado, merge, si es bug).",
            json!({"since": since_prop()}),
            &[],
        ),
        tool(
            "timeline",
            "Cuántos eventos hay por intervalo de tiempo (exacto, sobre todo el índice): «desde cuándo aparece este error», picos, incidentes. Con level/entity/grep cuenta solo esos.",
            with_scope(json!({
                "bucket_secs": json!({"type": "integer", "description": "Tamaño de bucket en segundos (por defecto 3600)"}),
                "by": str_prop("Desglose: level | kind (opcional)"),
                "limit": json!({"type": "integer", "description": "Máximo de buckets (por defecto 500)"})
            })),
            &[],
        ),
        tool(
            "top",
            "Recuento EXACTO de los valores más frecuentes de una dimensión sobre todo el índice (sin tope de líneas): cuántos eventos por nivel, en qué servicios caen los FATAL (dim=entity, level=FATAL), qué tipos dominan.",
            with_scope(json!({
                "dim": str_prop("Dimensión: level | kind | actor | entity | attr:<clave>"),
                "limit": json!({"type": "integer", "description": "Máximo de valores (por defecto 25)"})
            })),
            &["dim"],
        ),
        tool(
            "patterns",
            "Mensajes de log más frecuentes agrupados en plantillas (los números variables se igualan), con su recuento EXACTO: «los 3 errores más repetidos» (level=FATAL,ERROR,CRITICAL), «qué escupe product_service» (entity=product_service).",
            with_scope(json!({
                "limit": json!({"type": "integer", "description": "Máximo de plantillas (por defecto 25)"})
            })),
            &[],
        ),
        tool(
            "trace",
            "Sigue la cadena de un evento por links (reverts, mismo ticket/PR, y el vcs.commit de una sesión de Nod ↔ su commit de git, con sha abreviado).",
            json!({
                "id": str_prop("Id del evento ancla"),
                "depth": json!({"type": "integer", "description": "Profundidad BFS (por defecto 5)"}),
                "limit": json!({"type": "integer", "description": "Máximo de nodos (por defecto 50)"})
            }),
            &["id"],
        ),
        tool(
            "session_patterns",
            "Proceso (sesiones de Nod) × resultado (git), por tarea y por sesión: tokens, costo, tiempo del agente frente a espera del humano, permisos negados, interrupciones, bucles, commits entregados y cuántos tocan hotspots, retrabajo a 30 días, cambios incompletos, merge y costo por commit.",
            json!({
                "since": since_prop(),
                "task": str_prop("Solo esta tarea (opcional)"),
                "session": str_prop("Solo esta sesión (opcional)"),
                "hotspots": json!({"type": "integer", "description": "Top N de hotspots que cuenta como hotspot (por defecto 25)"}),
                "limit": json!({"type": "integer", "description": "Máximo de tareas y de sesiones (por defecto 25)"})
            }),
            &[],
        ),
        tool(
            "events",
            "Los eventos en orden cronológico con todos los filtros: leer los mensajes reales de una ventana (p.ej. lo que pasó justo antes de un reinicio, o una traza completa de product_service).",
            with_scope(json!({
                "limit": json!({"type": "integer", "description": "Máximo de eventos (por defecto 25; la cabecera dice el total)"})
            })),
            &[],
        ),
        tool(
            "correlate",
            "Eventos de OTRAS fuentes cercanos en el tiempo a un evento (±Δt). Con via=K, en vez del tiempo: todo evento de cualquier fuente que comparte un valor de la clave K con el ancla (la misma sesión, el mismo sha, la misma prueba), contado por fuente y por kind/nivel.",
            json!({
                "id": str_prop("Id del evento ancla"),
                "via": str_prop("Clave de relación (relations de la config, un link o una dim); sin ella, ventana temporal (opcional)"),
                "kind": str_prop("Solo estos tipos, con comas; prefijo con * (opcional)"),
                "delta_secs": json!({"type": "integer", "description": "Ventana ±Δ en segundos (por defecto 3600)"}),
                "limit": json!({"type": "integer", "description": "Máximo de correlacionados (por defecto 25)"})
            }),
            &["id"],
        ),
        tool(
            "sources",
            "Qué hay indexado: cada fuente (repo git, logs, comandos como cmd:loki) con su nº de eventos, ventana de tiempo, niveles y entidades principales. Llamar primero para saber si chrono ya tiene los datos que se buscan.",
            json!({}),
            &[],
        ),
        tool(
            "stats",
            "min/p50/p95/max/suma de un atributo numérico (duración, bytes…) por grupo, con los filtros de siempre.",
            with_scope(json!({
                "attr": str_prop("Atributo numérico, p.ej. duration_s"),
                "by": str_prop("Agrupar por: level | kind | entity | attr:<clave> (opcional)"),
                "limit": json!({"type": "integer", "description": "Máximo de grupos (por defecto 25)"})
            })),
            &["attr"],
        ),
        tool(
            "precede",
            "Episodios de lo que casan los filtros (el ancla: p. ej. grep=\"received fast shutdown request\" attr=container:<db>, o un mensaje de error, o level=FATAL) y QUÉ LOS PRECEDE: errores/avisos en la ventana previa a cada episodio (con el nivel deducido del texto aunque la fuente no lo traiga) y los precursores comunes («antes de 6/6» sobre TODOS los episodios, con lo esperado por azar y el lift: >1, más que por azar). Con join=K, un episodio por valor de K y solo precursores con ese valor (la misma sesión o traza). Para «¿cuántas veces pasó X, cuándo, y qué lo provoca?» en una sola llamada.",
            with_scope(json!({
                "gap_secs": json!({"type": "integer", "description": "Hueco máximo entre anclas del mismo episodio (por defecto 120)"}),
                "window_secs": json!({"type": "integer", "description": "Ventana previa a cada episodio que se examina (por defecto 120)"}),
                "before_levels": str_prop("Niveles de los precursores (por defecto FATAL,CRITICAL,ERROR,WARNING; * = cualquiera, también sin nivel)"),
                "before_entity": str_prop("Solo precursores de esta(s) entidad(es) (opcional)"),
                "by_origin": json!({"type": "boolean", "description": "Un episodio por contenedor/servicio de origen en vez de mezclarlos (recomendado si hay preflight/sidecars) (por defecto false)"}),
                "limit": json!({"type": "integer", "description": "Máximo de episodios (por defecto 50)"})
            })),
            &[],
        ),
        tool(
            "gaps",
            "«X sin Y» con cifras exactas: los valores de la clave via del lado from que ningún evento del lado to tiene (pruebas sin resultado, resultados sin decisión). Con by=U, la unidad es el valor de U y cuenta como cubierta si alguna de sus claves está en to (sesiones sin commit: by=sessionId). Un lado es una fuente (nombre, tipo o id) seguida de filtros kind= level= entity= grep= attr= join= separados por espacios.",
            json!({
                "from": str_prop("Lado X, p. ej. \"claude kind=tool_use entity=Edit,Write\""),
                "to": str_prop("Lado Y, p. ej. \"git\""),
                "via": str_prop("Clave que une los dos lados"),
                "by": str_prop("Unidad que se cuenta, si no es la propia clave (p. ej. sessionId) (opcional)"),
                "since": since_prop(),
                "limit": json!({"type": "integer", "description": "Muestra de unidades sin pareja (por defecto 25; las cifras son de todas)"})
            }),
            &["from", "to", "via"],
        ),
        tool(
            "informe",
            "EMPIEZA AQUÍ. Informe completo y exacto en UNA llamada. tipo=logs (por defecto): cobertura, líneas por nivel, en qué servicios/contenedores cae cada nivel, mensajes más repetidos por nivel, picos, episodios de FATAL/CRITICAL con lo que los precede, y las consultas siguientes sugeridas. tipo=codigo con target=fichero[:A-B]: por qué está así (explain), dueños, qué cambia con él y si es hotspot.",
            with_scope(json!({
                "tipo": str_prop("logs (por defecto) | codigo"),
                "target": str_prop("Para tipo=codigo: fichero o fichero:DESDE-HASTA")
            })),
            &[],
        ),
    ]
}

/// Texto devuelto por cualquier tool cuando aún no hay índice: nunca se
/// crashea por falta de `.chrono/index.db`, simplemente se pide `chrono init`.
fn no_index_text() -> String {
    "chrono: este repositorio aún no tiene índice. Ejecuta 'chrono init' en su raíz y reintenta."
        .to_string()
}

fn text_content(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn result_content(result: &Value) -> Result<Value> {
    Ok(text_content(serde_json::to_string_pretty(result)?))
}

/// Ejecuta `tools/call`: despacha por `params.name` sobre `params.arguments`.
pub fn call_tool(state: &State, params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call: falta 'name'")?;
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);

    let store = match state.store() {
        Some(s) => s,
        None => return Ok(text_content(no_index_text())),
    };
    run_tool(store, name, args)
}

/// Texto de la respuesta de una herramienta (lo mismo que ve el modelo por MCP):
/// lo usa el CLI (`chrono informe`) para no tener dos implementaciones.
pub fn run_tool_text(store: &chrono_store::Store, name: &str, args: &Value) -> Result<String> {
    let v = run_tool(store, name, args)?;
    Ok(v["content"][0]["text"].as_str().unwrap_or("").to_string())
}

fn run_tool(store: &chrono_store::Store, name: &str, args: &Value) -> Result<Value> {
    let conn = store.conn();
    let since = args.get("since").and_then(Value::as_str).unwrap_or("");
    let since_epoch = since_to_epoch(since);

    let result = match name {
        "hotspots" => hotspots(conn, since_epoch)?,
        "coupling" => {
            let file = arg_str(args, "file")?;
            coupling(conn, file, since_epoch)?
        }
        "owners" => {
            let path = arg_str(args, "path")?;
            owners(conn, path, since_epoch)?
        }
        "churn" => churn(conn, since_epoch)?,
        "search" => {
            let query = arg_str(args, "query")?;
            search(conn, query)?
        }
        "similar" => {
            let sha = arg_str(args, "sha")?;
            similar(conn, sha)?
        }
        "bugs" => bugs(conn, since_epoch)?,
        "tickets" => {
            let id = arg_str(args, "id")?;
            tickets(conn, id)?
        }
        "phases" => phases(conn)?,
        "branches" => {
            let base = args.get("base").and_then(Value::as_str).unwrap_or("");
            branches(store, base)?
        }
        "explain" => {
            let target = arg_str(args, "target")?;
            let limit = arg_i64(args, "limit", 15).max(1) as usize;
            explain(store, target, limit)?
        }
        "prs" => prs(conn, since)?,
        "timeline" => {
            // El parámetro MCP es en SEGUNDOS (cómodo); `at_epoch` es ms → ×1000.
            let bucket_ms = arg_i64(args, "bucket_secs", 3600).max(1) * 1000;
            let by = args.get("by").and_then(Value::as_str).filter(|s| !s.is_empty());
            let limit = arg_i64(args, "limit", 500).max(1) as usize;
            let scope = scope_from(args);
            let rows = chrono_metrics::timeline_scoped(conn, since_epoch, scope.until_epoch, bucket_ms, by, &scope, limit)?;
            let buckets: Vec<Value> = rows
                .iter()
                .map(|b| json!({"bucket_epoch": b.bucket_epoch, "key": b.key, "count": b.count}))
                .collect();
            json!({"bucket_ms": bucket_ms, "by": by, "buckets": buckets})
        }
        "top" => {
            let dim = arg_str(args, "dim")?;
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            let sc = scope_from(args);
            let filter = chrono_metrics::TopFilter {
                source: sc.source,
                grep: sc.grep,
                level: sc.level,
                kind: sc.kind,
                entity: sc.entity,
                attr: sc.attr,
                join: sc.join,
                infer_level: sc.infer_level,
                until_epoch: sc.until_epoch,
            };
            let rows = chrono_metrics::top_filtered(conn, dim, since_epoch, &filter, limit)?;
            let items: Vec<Value> = rows.iter().map(|v| json!({"value": v.value, "count": v.count})).collect();
            json!({"dim": dim, "top": items})
        }
        "sources" => sources(conn)?,
        "informe" => crate::informe::informe(store, args, since_epoch)?,
        "precede" => {
            let anchor = scope_from(args);
            if anchor.is_empty() {
                return Err("precede necesita un ancla: grep, entity, attr o level (lo que marca cada episodio)".into());
            }
            let o = chrono_metrics::PrecedeOpts {
                since_epoch,
                anchor,
                gap_ms: arg_i64(args, "gap_secs", 120).max(1) * 1000,
                window_ms: arg_i64(args, "window_secs", 120).max(1) * 1000,
                levels: args.get("before_levels").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("FATAL,CRITICAL,ERROR,WARNING"),
                entity: args.get("before_entity").and_then(Value::as_str).filter(|s| !s.is_empty()),
                max_episodes: arg_i64(args, "limit", 50).max(1) as usize,
                by_origin: args.get("by_origin").and_then(Value::as_bool).unwrap_or(false),
                join: join_key(args),
                ..Default::default()
            };
            let r = chrono_metrics::precede(conn, &o)?;
            let n = r.episodes_total;
            let episodes: Vec<Value> = r
                .episodes
                .iter()
                .map(|e| {
                    let pre: Vec<Value> = e
                        .preceding
                        .iter()
                        .map(|p| json!({"level": p.level, "count": p.count, "secs_before": p.last_before_secs, "entity": p.entity, "template": p.template, "example_id": p.example_id}))
                        .collect();
                    json!({"start": e.start_at, "anchors": e.anchors, "origins": e.origins, "first_id": e.first_id, "preceding": pre})
                })
                .collect();
            let commons: Vec<Value> = r.common.iter().map(|c| common_json(c, n)).collect();
            json!({"episodes_total": n, "episodes_returned": r.episodes.len(), "common_precursors": commons, "episodes": episodes})
        }
        "stats" => {
            let attr = arg_str(args, "attr")?;
            let by = args.get("by").and_then(Value::as_str).filter(|s| !s.is_empty());
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            // Aquí `attr` es el atributo numérico, no el filtro `clave:valor`
            // de las demás consultas: tomarlo como filtro no casaba nada.
            let mut filters = args.clone();
            if let Some(o) = filters.as_object_mut() {
                o.remove("attr");
            }
            let rows = chrono_metrics::stats(conn, attr, by, since_epoch, &scope_from(&filters), limit)?;
            let groups: Vec<Value> = rows
                .iter()
                .map(|r| json!({"group": r.group, "count": r.count, "min": r.min, "p50": r.p50, "p95": r.p95, "max": r.max, "sum": r.sum}))
                .collect();
            json!({"attr": attr, "by": by, "groups": groups})
        }
        "correlate" if args.get("via").and_then(Value::as_str).is_some_and(|v| !v.trim().is_empty()) => {
            let id = arg_str(args, "id")?;
            let via = arg_str(args, "via")?;
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            let kinds: Vec<String> = args.get("kind").and_then(Value::as_str).unwrap_or("").split(',').map(str::to_string).collect();
            let v = chrono_metrics::correlate_via(conn, id, via, &kinds, limit)?;
            let ev = |e: &chrono_metrics::EventRef| json!({"id": e.id, "source_id": e.source_id, "kind": e.kind, "at": e.at, "level": e.level, "title": e.title, "delta_ms": e.delta_ms});
            json!({"via": v.key, "values": v.values, "anchor": ev(&v.anchor), "related_total": v.total,
                   "by_source": v.by_source.iter().map(|(s, n)| json!({"source_id": s, "count": n})).collect::<Vec<_>>(),
                   "by_kind_level": v.by_kind_level.iter().map(|(k, l, n)| json!({"kind": k, "level": l, "count": n})).collect::<Vec<_>>(),
                   "related": v.related.iter().map(ev).collect::<Vec<_>>()})
        }
        "correlate" => {
            let id = arg_str(args, "id")?;
            let delta_ms = arg_i64(args, "delta_secs", 3600).max(1) * 1000;
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            correlate(conn, id, delta_ms, limit)?
        }
        "gaps" => {
            let side = |k: &str| -> Result<chrono_metrics::EventScope<'_>> {
                let raw = arg_str(args, k)?;
                parse_side(raw).map_err(|e| format!("{k}: {e}").into())
            };
            let (from, to) = (side("from")?, side("to")?);
            let via = arg_str(args, "via")?;
            let by = args.get("by").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            let g = chrono_metrics::gaps(conn, &from, &to, via, by, since_epoch, limit)?;
            let sample: Vec<Value> =
                g.sample.iter().map(|u| json!({"unit": u.unit, "events": u.events, "first_at": u.first_at, "last_at": u.last_at})).collect();
            json!({"via": via, "by": by, "units": g.units, "covered": g.covered, "missing": g.missing, "to_values": g.to_values, "missing_sample": sample})
        }
        "patterns" => {
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            let rows = chrono_metrics::patterns_scoped(conn, since_epoch, &scope_from(args), limit)?;
            let items: Vec<Value> = rows
                .iter()
                .map(|p| json!({"template": p.template, "count": p.count, "example_id": p.example_id}))
                .collect();
            json!({"patterns": items})
        }
        "trace" => {
            let id = arg_str(args, "id")?;
            let depth = arg_i64(args, "depth", 5).max(1);
            let limit = arg_i64(args, "limit", 50).max(1) as usize;
            trace(conn, id, depth, limit)?
        }
        "events" => {
            let limit = arg_i64(args, "limit", 25).max(1) as usize;
            let sc = scope_from(args);
            let filter = chrono_metrics::EventFilter {
                since_epoch,
                until_epoch: sc.until_epoch,
                source: sc.source,
                kind: sc.kind,
                level: sc.level,
                grep: sc.grep,
                entity: sc.entity,
                attr: sc.attr,
                join: sc.join,
                infer_level: sc.infer_level,
            };
            events(conn, &filter, limit)?
        }
        "session_patterns" => {
            let opts = chrono_metrics::SessionPatternsOpts {
                since_epoch,
                task: args.get("task").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                session: args.get("session").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                hotspot_limit: arg_i64(args, "hotspots", 25).max(1) as usize,
                limit: arg_i64(args, "limit", 25).max(1) as usize,
                ..Default::default()
            };
            session_patterns(store, opts)?
        }
        other => return Err(format!("herramienta desconocida: {other}").into()),
    };
    if crate::contract::main_list(name).is_some() {
        let mut result = result;
        let (head, meta) = crate::contract::header(conn, name, args, &result);
        crate::contract::enrich(name, &mut result, meta["total"].as_i64().unwrap_or(0));
        let depth = args.get("depth").and_then(Value::as_str).unwrap_or(crate::contract::default_depth(name));
        crate::contract::apply_depth(&mut result, depth);
        if args.get("format").and_then(Value::as_str) == Some("json") {
            if let Some(o) = result.as_object_mut() {
                o.insert("meta".into(), meta);
            }
            return Ok(text_content(serde_json::to_string(&result)?));
        }
        return Ok(text_content(format!("{head}\n{}", crate::contract::render(&result))));
    }
    result_content(&result)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("tools/call: falta el argumento requerido '{key}'").into())
}

/// Lee un entero de `args[key]`, o `default` si falta o no es entero.
fn arg_i64(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

/// Los filtros comunes de `args` (ver [`with_scope`]). `since` va aparte: cada
/// consulta lo recibe como `since_epoch`.
/// La clave de `join` (`K=v` → `K`) para separar episodios en `precede`.
pub(crate) fn join_key(args: &Value) -> Option<&str> {
    let j = args.get("join").and_then(Value::as_str)?.trim();
    let k = j.split(['=', ':']).next().unwrap_or("").trim();
    (!k.is_empty()).then_some(k)
}

/// Un precursor común: «antes de x/N» de todos los episodios, con lo esperado
/// por azar y el lift (P-MSR-3).
pub(crate) fn common_json(c: &chrono_metrics::Common, total: usize) -> Value {
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    json!({"before_episodes": format!("{}/{}", c.episodes, total), "level": c.level, "template": c.template,
           "expected": c.expected.map(|x| (x * 10.0).round() / 10.0), "lift": c.lift.map(r2)})
}

/// Un lado de `gaps`: `"<fuente> kind=… level=… entity=… grep=… attr=… join=…"`
/// (lo mismo que `--from`/`--to` en el CLI).
fn parse_side(raw: &str) -> std::result::Result<chrono_metrics::EventScope<'_>, String> {
    let mut sc = chrono_metrics::EventScope::default();
    for tok in raw.split_whitespace() {
        match tok.split_once('=') {
            None if sc.source.is_none() => sc.source = Some(tok),
            None => return Err(format!("{tok:?}: una sola fuente por lado")),
            Some(("source", v)) => sc.source = Some(v),
            Some(("kind", v)) => sc.kind = Some(v),
            Some(("level", v)) => sc.level = Some(v),
            Some(("entity", v)) => sc.entity = Some(v),
            Some(("grep", v)) => sc.grep = Some(v),
            Some(("attr", v)) => sc.attr = Some(v),
            Some(("join", v)) => sc.join = Some(v),
            Some((k, _)) => return Err(format!("{k:?}: usa source, kind, level, entity, grep, attr o join")),
        }
    }
    if sc.is_empty() {
        return Err("lado vacío".into());
    }
    Ok(sc)
}

pub(crate) fn scope_from(args: &Value) -> chrono_metrics::EventScope<'_> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).filter(|v| !v.trim().is_empty());
    chrono_metrics::EventScope {
        source: s("source"),
        grep: s("grep"),
        level: s("level"),
        kind: s("kind"),
        entity: s("entity"),
        attr: s("attr"),
        join: s("join"),
        infer_level: args.get("infer_level").and_then(Value::as_bool).unwrap_or(false),
        until_epoch: s("until").map_or(0, since_to_epoch),
    }
}

fn sources(conn: &Connection) -> Result<Value> {
    let rows = chrono_metrics::sources(conn)?;
    let vc = |v: &[(String, i64)]| -> Vec<Value> { v.iter().map(|(k, n)| json!({"value": k, "count": n})).collect() };
    let items: Vec<Value> = rows
        .iter()
        .map(|s| {
            json!({
                "source": s.source, "alias": s.alias, "kind": s.kind, "events": s.events,
                "first": s.first, "last": s.last,
                "levels": vc(&s.levels), "inferred_levels_unlabeled": vc(&s.inferred_levels), "kinds": vc(&s.kinds), "top_entities": vc(&s.top_entities),
            })
        })
        .collect();
    // Secretos cambiados por <SECRET> al indexar (acumulado del índice).
    let redacted: i64 = conn
        .query_row("SELECT value FROM meta WHERE key = 'redacted_total'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Ok(json!({"sources": items, "secrets_redacted": redacted}))
}

pub(crate) fn hotspots(conn: &Connection, since_epoch: i64) -> Result<Value> {
    let rows = chrono_metrics::hotspots(conn, since_epoch, HOTSPOTS_LIMIT, false)?;
    let items: Vec<Value> = rows
        .iter()
        .map(
            |h| json!({"entity": h.entity, "changes": h.changes, "size": h.size, "score": h.score}),
        )
        .collect();
    Ok(json!({"hotspots": items}))
}

pub(crate) fn coupling(conn: &Connection, entity: &str, since_epoch: i64) -> Result<Value> {
    let (rows, _total) = chrono_metrics::coupling(conn, entity, COUPLING_MIN_SUPPORT, since_epoch, COUPLING_LIMIT, false)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|c| json!({"b": c.b, "support": c.support, "confidence": c.confidence}))
        .collect();
    Ok(json!({"for": entity, "coupled": items}))
}

pub(crate) fn owners(conn: &Connection, prefix: &str, since_epoch: i64) -> Result<Value> {
    let (rows, bus_factor) = chrono_metrics::owners(conn, prefix, since_epoch, false)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|o| json!({"name": o.name, "changes": o.commits, "share": o.share}))
        .collect();
    Ok(json!({"owners": items, "bus_factor": bus_factor}))
}

fn churn(conn: &Connection, since_epoch: i64) -> Result<Value> {
    let rows = chrono_metrics::churn(conn, since_epoch, CHURN_LIMIT, false)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| json!({"entity": r.entity, "added": r.added, "deleted": r.deleted}))
        .collect();
    Ok(json!({"churn": items}))
}

fn search(conn: &Connection, query: &str) -> Result<Value> {
    let rows = chrono_metrics::search(conn, query, SEARCH_LIMIT)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|h| json!({"id": h.id, "title": h.title, "at": h.at}))
        .collect();
    Ok(json!({"query": query, "results": items}))
}

fn similar(conn: &Connection, id: &str) -> Result<Value> {
    let rows = chrono_metrics::similar(conn, id, SIMILAR_MAX_DIST, SIMILAR_LIMIT)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|s| json!({"id": s.id, "title": s.title, "distance": s.distance}))
        .collect();
    Ok(json!({"of": id, "similar": items}))
}

fn bugs(conn: &Connection, since_epoch: i64) -> Result<Value> {
    let b = chrono_metrics::bugs(conn, since_epoch, BUGS_LIMIT)?;
    let categories: Vec<Value> = b
        .categories
        .iter()
        .map(|c| {
            json!({
                "category": c.category,
                "fixes": c.fixes,
                "top_files": c.top_files,
                "examples": c.examples,
            })
        })
        .collect();
    let hot_areas: Vec<Value> = b
        .hot_areas
        .iter()
        .map(|a| {
            json!({
                "dir": a.dir,
                "fixes": a.fixes,
                "recent_fixes": a.recent_fixes,
                "examples": a.examples,
            })
        })
        .collect();
    Ok(json!({"total_fixes": b.total_fixes, "categories": categories, "hot_areas": hot_areas}))
}

fn tickets(conn: &Connection, id: &str) -> Result<Value> {
    let t = chrono_metrics::ticket(conn, id)?;
    Ok(json!({"ticket": t.ticket, "commits": t.commits, "files": t.files, "prs": t.prs}))
}

fn phases(conn: &Connection) -> Result<Value> {
    let rows = chrono_metrics::phases(conn)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|p| json!({"name": p.name, "date": p.date}))
        .collect();
    Ok(json!({"phases": items}))
}

/// `explain`: mismo contenido y orden que `chrono explain` (las reglas viven en
/// `chrono_metrics::explain`; aquí solo se ejecuta el blame y se serializa).
pub(crate) fn explain(store: &chrono_store::Store, target: &str, limit: usize) -> Result<Value> {
    let repo = match store.meta("repo_path")?.filter(|s| !s.is_empty()) {
        Some(rp) => rp,
        None => store.list_sources()?.into_iter().find(|s| s.kind == "git").map(|s| s.path).ok_or("no git source in the index")?,
    };
    let rev = store
        .list_sources()?
        .into_iter()
        .find(|s| s.kind == "git")
        .map(|s| s.watermark)
        .or(store.meta("last_watermark")?)
        .and_then(|w| w.strip_prefix("sha:").map(str::to_string))
        .filter(|s| !s.is_empty())
        .ok_or("the index has no git revision; run 'chrono sync'")?;
    let (file, range) = chrono_metrics::parse_target(target)?;
    let b = chrono_source_git::blame(Path::new(&repo), &rev, &file, range).map_err(|e| format!("explain {file}: {e}"))?;
    let blamed: Vec<chrono_metrics::Blamed> = b
        .commits
        .iter()
        .map(|c| chrono_metrics::Blamed { sha: c.sha.clone(), author: c.author.clone(), author_time: c.author_time, summary: c.summary.clone(), lines: c.lines })
        .collect();
    let x = chrono_metrics::explain(store.conn(), &file, &blamed, limit)?;
    let commits: Vec<Value> = x
        .commits
        .iter()
        .map(|ec| {
            let (c, f) = (&ec.blamed, &ec.facts);
            let mut row = json!({
                "id": &c.sha[..10.min(c.sha.len())],
                "date": chrono_metrics::utc_date(c.author_time),
                "author": c.author,
                "lines": c.lines,
                "fix": f.fix.as_str(),
            });
            if !f.category.is_empty() {
                row["category"] = json!(f.category);
            }
            row["title"] = json!(c.summary);
            if !f.tickets.is_empty() {
                let t: Vec<String> = f
                    .tickets
                    .iter()
                    .map(|tk| if tk.title.is_empty() { tk.id.clone() } else { format!("{} {} {}: {}", tk.id, tk.kind, tk.state, tk.title) })
                    .collect();
                row["tickets"] = json!(t);
            }
            if f.is_bulk {
                row["bulk"] = json!(true);
            }
            row
        })
        .collect();
    let changes_with: Vec<Value> = x
        .co_changes
        .iter()
        .map(|c| {
            let mut o = json!({"entity": c.entity, "commits": c.commits, "of": x.co_base});
            if !c.symbols.is_empty() {
                o["symbols"] = json!(c.symbols.iter().map(|(n, k)| format!("{n} ({k})")).collect::<Vec<_>>());
            }
            o
        })
        .collect();
    Ok(json!({
        "file": file,
        "from": b.from,
        "to": b.to,
        "file_lines": b.total_lines,
        "revision": &rev[..12.min(rev.len())],
        "summary": {
            "commits": x.total_commits,
            "fixes_yes": x.fixes_yes,
            "fixes_maybe": x.fixes_maybe,
            "not_indexed": x.not_indexed,
            "oldest": x.oldest.map(chrono_metrics::utc_date),
            "newest": x.newest.map(chrono_metrics::utc_date),
        },
        "commits": commits,
        "authors": x.authors.iter().map(|(a, n)| json!({"author": a, "lines": n})).collect::<Vec<_>>(),
        "changes_with": changes_with,
        "truncated": x.total_commits > x.commits.len(),
    }))
}

fn branches(store: &chrono_store::Store, base: &str) -> Result<Value> {
    // `meta.repo_path` (fuente primaria git de `init`); si no, la primera
    // fuente git de `sources` (git añadido con `chrono add`).
    let repo_path = match store.meta("repo_path")?.filter(|s| !s.is_empty()) {
        Some(rp) => rp,
        None => store
            .list_sources()?
            .into_iter()
            .find(|s| s.kind == "git")
            .map(|s| s.path)
            .ok_or("no git source in the index")?,
    };
    let (base, current, rows) = chrono_source_git::branches(Path::new(&repo_path), base)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|b| {
            let authors: Vec<Value> = b
                .authors
                .iter()
                .map(|a| json!({"name": a.name, "commits": a.commits}))
                .collect();
            json!({
                "name": b.name,
                "current": b.current,
                "tip": {
                    "sha": b.tip.sha,
                    "author": b.tip.author,
                    "date": b.tip.date,
                    "subject": b.tip.subject,
                },
                "age_days": b.age_days,
                "ahead": b.ahead,
                "behind": b.behind,
                "merged": b.merged,
                "stale": b.stale,
                "authors": authors,
                "bus_factor": b.bus_factor,
            })
        })
        .collect();
    Ok(json!({"base": base, "current": current, "branches": items}))
}

/// PRs del forge: lee la tabla `issues` directamente (no hay `chrono-metrics`
/// equivalente en el port Rust). Réplica de `PullRequests` en
/// `internal/mcp/mcp.go` / `internal/metrics/metrics.go`: comparación textual
/// de `closed_at` contra `since` ("0000" si no hay ventana, menor que
/// cualquier fecha ISO), `closed_at=''` siempre pasa (PRs aún abiertos).
fn prs(conn: &Connection, since: &str) -> Result<Value> {
    let since_arg = if since.is_empty() { "0000" } else { since };
    let mut stmt = conn.prepare(
        "SELECT number, title, state, merged, is_bug, labels FROM issues
         WHERE kind = 'pr' AND (closed_at >= ?1 OR closed_at = '')
         ORDER BY number DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_arg, PRS_LIMIT], |r| {
        Ok(json!({
            "number": r.get::<_, i64>(0)?,
            "title": r.get::<_, String>(1)?,
            "state": r.get::<_, String>(2)?,
            "merged": r.get::<_, i64>(3)? == 1,
            "is_bug": r.get::<_, i64>(4)? == 1,
            "labels": r.get::<_, String>(5)?,
        }))
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(json!({"pull_requests": items}))
}

fn trace(conn: &Connection, id: &str, depth: i64, limit: usize) -> Result<Value> {
    let tr = chrono_metrics::trace(conn, id, depth, limit)?;
    let event_json = |e: &chrono_metrics::EventRow| {
        json!({"id": e.id, "source_id": e.source_id, "kind": e.kind, "at": e.at, "level": e.level, "title": e.title})
    };
    let chain: Vec<Value> = tr
        .nodes
        .iter()
        .map(|n| {
            let mut v = event_json(&n.event);
            v["depth"] = json!(n.depth);
            v["rel"] = json!(n.rel);
            v
        })
        .collect();
    Ok(json!({"anchor": event_json(&tr.anchor), "chain": chain}))
}

fn events(conn: &Connection, filter: &chrono_metrics::EventFilter, limit: usize) -> Result<Value> {
    let rows = chrono_metrics::events(conn, filter, limit)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|e| json!({
            "id": e.id, "source_id": e.source_id, "kind": e.kind,
            "at": e.at, "level": e.level, "title": e.title,
        }))
        .collect();
    Ok(json!({"events": items}))
}

fn correlate(conn: &Connection, id: &str, delta_ms: i64, limit: usize) -> Result<Value> {
    let corr = chrono_metrics::correlate(conn, id, delta_ms, limit)?;
    let event_json = |e: &chrono_metrics::EventRef| {
        json!({
            "id": e.id, "source_id": e.source_id, "kind": e.kind, "at": e.at,
            "level": e.level, "title": e.title, "delta_ms": e.delta_ms,
        })
    };
    let related: Vec<Value> = corr.related.iter().map(event_json).collect();
    Ok(json!({"delta_ms": delta_ms, "anchor": event_json(&corr.anchor), "related": related}))
}

/// Horizonte en ms (último `last_sync_at` de las fuentes git, o ahora) y ramas
/// locales → mergeada (git en vivo). Mismo criterio que el CLI.
fn session_git_context(store: &chrono_store::Store) -> Result<(i64, std::collections::BTreeMap<String, bool>, String)> {
    let last_sync: Option<String> = store
        .conn()
        .query_row("SELECT MAX(last_sync_at) FROM sources WHERE kind = 'git'", [], |r| r.get(0))
        .unwrap_or(None);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // `last_sync_at` puede llevar `.mmm`: `since` entiende hasta el segundo.
    let horizon = last_sync
        .as_deref()
        .map(|s| since_to_epoch(s.split('.').next().unwrap_or(s)))
        .filter(|e| *e > 0)
        .unwrap_or(now_ms);
    let mut merged = std::collections::BTreeMap::new();
    let mut base = String::new();
    // Sin repo legible (sin fuente git, o fuentes aún sin ruta) no hay datos de
    // ramas: el merge sale solo de PRs y la consulta no falla.
    let repo = match store.meta("repo_path")?.filter(|s| !s.is_empty()) {
        Some(rp) => Some(rp),
        None => store.list_sources().ok().and_then(|v| v.into_iter().find(|s| s.kind == "git").map(|s| s.path)),
    };
    if let Some(repo) = repo {
        if let Ok((b, _cur, rows)) = chrono_source_git::branches(Path::new(&repo), "") {
            base = b;
            for r in rows {
                merged.insert(r.name, r.merged);
            }
        }
    }
    Ok((horizon, merged, base))
}

fn group_json(g: &chrono_metrics::Group, key_name: &str, related_name: &str) -> Value {
    const MAX: usize = 5;
    let take = |v: &[String]| v.iter().take(MAX).cloned().collect::<Vec<_>>();
    let t = &g.tokens;
    let c = &g.commits;
    let m = &g.merge;
    let mut out = json!({
        "profiles": take(&g.profiles),
        "hosts": take(&g.hosts),
        "tokens": {"input": t.input, "output": t.output, "cache_read": t.cache_read,
                   "cache_write": t.cache_write, "total": t.total(), "subagents": t.subagents},
        "cost_usd": g.cost_usd,
        "time": {"agent_ms": g.agent_ms, "waiting_human_ms": g.waiting_ms,
                 "waiting_decisions": g.waiting_decisions, "waiting_upper_bounds": g.waiting_upper_bounds},
        "permissions": {"asked": g.permissions_asked, "denied": g.denied, "denied_by_user": g.denied_by_user,
                        "denied_by_tool": g.denied_by_tool.iter().take(MAX)
                            .map(|d| json!({"tool": d.tool, "denied": d.count})).collect::<Vec<_>>()},
        "interruptions": g.interruptions,
        "loops": {"count": g.loops.len(), "top": g.loops.iter().take(MAX)
            .map(|l| json!({"tool": l.tool, "failures": l.failures, "session": l.session,
                            "agent": if l.agent.is_empty() { Value::Null } else { json!(l.agent) }}))
            .collect::<Vec<_>>()},
        "commits": {"delivered": c.delivered.len(), "resolved": c.resolved.len(), "ids": take(&c.resolved),
                    "unresolved": take(&c.unresolved), "touching_hotspots": c.touching_hotspots,
                    "hotspots_touched": take(&c.hotspots_touched)},
        "produced": take(&g.produced),
        "rework": g.rework.as_ref().map(|r| json!({
            "window_days": r.window_days, "files_delivered": r.files_delivered, "files_reworked": r.files_reworked,
            "file_ratio": r.file_ratio, "lines_delivered": r.lines_delivered, "lines_reworked": r.lines_reworked,
            "open_windows": r.open_windows, "reworked": take(&r.reworked)})),
        "incomplete": g.incomplete.as_ref().map(|i| json!({
            "edited_files": i.edited_files, "uncommitted": i.uncommitted.len(),
            "examples": take(&i.uncommitted), "outside_repo": i.outside_repo})),
        "merge": {"status": m.status, "via": if m.via.is_empty() { Value::Null } else { json!(m.via) },
                  "branches": take(&m.branches),
                  "prs": m.prs.iter().take(MAX).map(|(n, st, mg)| json!({"number": n, "state": st, "merged": mg})).collect::<Vec<_>>(),
                  "reason": if m.reason.is_empty() { Value::Null } else { json!(m.reason) }},
        "cost_per_commit": g.cost_per_commit,
        "tokens_per_commit": g.tokens_per_commit,
    });
    out[key_name] = json!(g.key);
    out[related_name] = json!(take(&g.related));
    if key_name == "session" {
        out["remote_session"] = json!(g.remote_session);
    }
    out
}

fn session_patterns(store: &chrono_store::Store, mut opts: chrono_metrics::SessionPatternsOpts) -> Result<Value> {
    let (horizon, merged, base) = session_git_context(store)?;
    opts.horizon_epoch = horizon;
    opts.branch_merged = merged;
    opts.base_branch = base.clone();
    let sp = chrono_metrics::session_patterns(store.conn(), &opts)?;
    let s = &sp.summary;
    Ok(json!({
        "summary": {"sessions": s.sessions, "tasks": s.tasks, "merge_rate": s.merge_rate,
                    "tasks_merged": s.tasks_merged, "tasks_merge_known": s.tasks_merge_known,
                    "totals": group_json(&s.all, "scope", "tasks")},
        "basis": {"events_scanned": sp.events_scanned, "hotspots": sp.hotspots,
                  "rework_window_days": opts.rework_days, "horizon_ms": horizon,
                  "base_branch": if base.is_empty() { Value::Null } else { json!(base) }},
        "tasks_total": sp.tasks_total,
        "tasks": sp.tasks.iter().map(|g| group_json(g, "task", "sessions")).collect::<Vec<_>>(),
        "sessions_total": sp.sessions_total,
        "sessions": sp.sessions.iter().map(|g| group_json(g, "session", "tasks")).collect::<Vec<_>>(),
        "notes": sp.notes,
    }))
}
