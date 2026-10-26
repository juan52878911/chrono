# Plan por fases: chrono como capa de cálculo sobre cualquier compendio de datos

Une la propuesta de optimización ([`PROPUESTA-OPTIMIZACION.md`](PROPUESTA-OPTIMIZACION.md), ids `P-*`) con lo
hablado después: ingerir carpetas enteras (transcripciones de Claude, carpetas de pruebas con PDF, imagen y
audio), extraer contenido con herramientas externas y relacionar fuentes por clave declarada en la config.

**Objetivo:** que con un fichero de config y una llamada MCP se obtenga el panorama exacto de un compendio
grande, y que el detalle se pida por referencia, sin meter los datos en el contexto. El binario sigue siendo un
solo ejecutable Rust estático, sin modelos ni runtimes dentro (hoy pesa 2,2 MB).

**Línea base medida** (fixture sintético de 1M de eventos, 0.2.7): `informe` 8,05 s / 506 MB RSS ·
`top attr:host` 1,87 s / 584 MB (`SCAN events`) · índice de 584 B/evento (`attrs` = 141 de 254 MB) ·
68 plantillas para 12 patrones · 23 tools MCP con 18,8 KB de esquemas (~5,4k tokens por sesión) ·
`events` 28,7 KB y `precede` 22 KB por llamada con los valores por defecto.

Las cifras marcadas como *objetivo* son estimaciones que valida la fase correspondiente.

## Estado (2026-10-26)

**F0 y F1 hechas → 0.3.0.** Medido con `make bench` a 1M frente a 0.2.7:
- RSS de las consultas −62 a −95 % (`patterns`, `precede`, `events`: ~450 → ~25 MB). `informe` +31 % de
  tiempo (8,8 → 11,6 s) hasta F3, que lo saca de recorrer `events`.
- Plantillas 68 → 15. Respuestas MCP: `precede` 20 KB → 1,9 KB, `events` 29 KB → 7,4 KB.
- Ingesta +8 % por la redacción de secretos (era +23 % antes de la comprobación previa sin reservas).

Lo que salió al hacerlo y corrige la propuesta:
- `busy_timeout` ya estaba (5 s en toda apertura); faltaba el cerrojo. Sin él, dos `sync` simultáneos
  **duplicaban los rollups** (420 000 para 220 000 eventos): `patterns` y `timeline` inflados sin aviso.
- El bench de agentes (`bench/agentes/regresion`) **sí mide aciertos** (rúbrica calibrada); falta la elección de
  tool. `hook.rs` tenía 9 tests, no 0.
- El intermediario y `cmd:` guardaban la salida cruda en `.chrono/mcp/` y `.chrono/cmd/`: también se redacta ahí.
- `precede` calcula `episodes_total` y «antes de x/N» sobre los episodios devueltos (tope `limit`, 50): con más
  episodios que el tope, las cifras son parciales. Pendiente en F4 (P-MSR-3).
- No se movió `hook.rs` a otro crate: no aporta por sí solo; se añadieron los tests que faltaban.
- La cuenta de Trycore no tuvo cupo en toda la fase (otra sesión la ocupaba); todo se hizo en esta sesión.

---

## Fase 0 · Medir antes de tocar

Sin esto no se puede saber si una fase mejora o empeora nada.

| Acción | Hecho cuando |
|---|---|
| **P-EVAL-1** Eval del agente: 30-50 preguntas con respuesta conocida sobre 3 fixtures (repo chrono, logs 1M, sesiones de Claude), ejecutadas con `claude -p`. Mide aciertos, tokens consumidos y si el agente eligió la tool correcta. | `bench/eval` reproducible con un comando; informe con la línea base |
| Fixture de 1M en `bench/` (hoy `gen.py` está en un scratchpad) y script que mida tiempo, RSS, tamaño del `.db` y bytes de respuesta MCP | Las cifras de arriba salen de `make bench` |
| Fixture de sesiones: una muestra anonimizada de transcripciones de `~/.claude/projects` | Fichero en `bench/fixtures/`, sin secretos (pasada de `auditor-secretos`) |

## Fase 1 · Ganancias rápidas sin reindexar

Cambios pequeños e independientes, sin tocar el esquema.

| Acción | Dónde | Hecho cuando |
|---|---|---|
| **P-BIN-1** `Store::open_read` con PRAGMAs ligeros (hoy toda apertura reserva 256 MB de cache + 256 MB de mmap, hooks incluidos) | `chrono-store/src/store.rs:36-92` | RSS de consultas y hooks medido; *objetivo* < 50 MB en el repo real |
| **P-ROB-1** `busy_timeout` en lectura + cerrojo de fichero para `sync` concurrente entre worktrees | `store.rs`, `index.rs` | Test con dos `sync` a la vez; pasada de `auditor-cerrojos` |
| **P-MCP-2 + P-CTX-2** Defaults pequeños en `events`/`precede`/`timeline`, `depth=counts\|summary\|full`, `max_bytes` en el contrato | `chrono-mcp` | `events` y `precede` por defecto < 4 KB a 1M |
| **C6 / P-MSR-4 (parte 1)** Enmascarar hex sin dígitos y tokens de longitud fija; no partir identificadores tipo `pyo3` | `chrono-core` `template.rs:14-27` | Fixture 1M: 68 → ~12 plantillas |
| **P-SEC-1** Redacción de secretos en el `Writer` antes de indexar, con opt-out por fuente | `chrono-store` | Pasada de `auditor-secretos` sobre el `.db` y `.chrono/cmd/*.cmd.jsonl`, con cifra de falsos positivos |
| **P-BIN-2** `Makefile`, `flake.nix` e `install.sh` compilan Rust; retirar el Go v0.1.1 (`cmd/`, `internal/`) | raíz | `make`, `nix build` e `install.sh` producen el binario 0.2.x |
| **P-BIN-4** Sacar `hook.rs` (1 397 líneas, 0 tests) a su crate con tests | `chrono-cli` → crate nuevo | Tests de los hooks y del intermediario; red de seguridad para la fase 5 |

Cierre: versión 0.3.0; eval y bench comparados con la fase 0.

## Fase 2 · Ingesta universal (cualquier carpeta)

Lo que convierte a chrono de «logs y git» en «cualquier compendio».

| Acción | Hecho cuando |
|---|---|
| **P-REL-3** Un solo fichero de config con esquema versionado y `chrono config check` (va primero: las fases 2-4 añaden claves) | `config check` valida y explica cada error con la ruta de la clave |
| **Globs en `add`**: `chrono add '~/.claude/projects/**/*.jsonl' --as jsonl` es **una** fuente con watermark por fichero | Ficheros nuevos o crecidos se ingieren en `sync`; los demás no se releen |
| **Preset `claude-sessions`** para el adaptador `jsonl`: `time_field`, `kind_field`, `entity_field`, `explode` sobre `message.content[]` (una tool = un evento), `links` a ficheros y URLs | Con el fixture de sesiones: `top entity` = tools más usadas, `precede` sobre fallos de Bash, `correlate` con git |
| **Fuente `dir` (inventario)**: un evento por fichero con ruta, tipo (por extensión y número mágico, sin dependencias), tamaño, fechas y hash; la carpeta como entidad | Indexa la carpeta de 2,2 GB sin leer su contenido entero (hash incremental por tamaño + mtime) |

**Diseño concretado al empezar F2** (transcripciones reales: 1 925 ficheros, 2,7 GB en `~/.claude/projects`):
- `explode` ya existe y está probado con una línea de Claude (`message.content`, elemento-luego-raíz): `name` es
  la tool en `tool_use`; `timestamp`, `cwd`, `gitBranch`, `sessionId` salen de la raíz.
- Falta en el adaptador `jsonl`: `explode_where` con varios valores (`type=tool_use|tool_result`); `level_map`
  (`is_error: true` → `ERROR`); y un título de respaldo acotado (hoy, sin `message_field`, el título es el
  elemento serializado: metería comandos y contenido de ficheros en el índice).
- Una fuente glob es **una** fila en `sources` (agrupada por alias), con watermark por fichero; si no, 1 925 filas
  inflan `sources` y cada `sync` las recorre una a una.
- El nombre de la tool no viaja en `tool_result` (solo `tool_use_id`): «qué tool falla más» necesita el enlace
  `tool_use_id` → `id`, que es justo `relations` (F4). En F2 basta con guardar `tool_use_id` como link.

**Validación de la fase:** caso real de tus sesiones de Claude, respondido por MCP sin leer transcripciones.
Si no aporta lo esperado, se replantean las fases 4 y 6 antes de seguir.

## Fase 3 · Un solo reindex: esquema v6

Todo lo que cambia el esquema entra junto, para reindexar una sola vez.

| Acción | Hecho cuando |
|---|---|
| **P-REL-1** `dims` declaradas por fuente en la config → tabla `event_dims` indexada (diseñada en DESIGN-GENERAL-CORE, nunca construida) | `top attr:host` sin `SCAN events` (verificado con `EXPLAIN QUERY PLAN`) |
| **P-ESC-2** No duplicar el registro en `attrs`; `at` en TEXT derivado de `at_epoch` | *Objetivo* −25-30 % de bytes por evento |
| **P-ESC-3** Claves enteras (rowid) para `touches`, `labels` y `links` | Medido con `dbstat` |
| **P-ESC-4** Índices parciales: `actor`, `kind` y SimHash solo donde se usan (~90 MB inútiles a 1M) | Medido con `dbstat` |
| **P-ESC-1** `dim_rollups` (fuente × plantilla × nivel × entidad × bucket); `informe` lee de rollups | *Objetivo* `informe` a 1M < 1 s y < 100 MB RSS |

Cierre: migración con aviso de reindex (como en las versiones 3 y 4 del esquema), versión 0.4.0, eval y bench.

## Fase 4 · Relaciones por clave entre fuentes

Depende de la fase 3: los joins sin `event_dims` vuelven a recorrer `events`.

| Acción | Hecho cuando |
|---|---|
| **P-REL-2** `relations` en la config (`trace_id`, `sha`, `ticket`, `host`, `id_prueba`…), materializadas en `links` al ingerir; también con extracción por patrón en ruta o título (p. ej. `prueba-(\d+)` del nombre del fichero, sin `regex`: matcher propio como en `textlog`) | `events --join`, `correlate --via` y `precede --join` responden con cifras exactas |
| **P-MSR-3** `precede` con frecuencia base y lift (barato desde rollups) | Cada precursor trae `lift`; los ubicuos dejan de salir arriba |
| Huecos como consulta: «X sin Y» (pruebas sin resultado, resultados sin decisión, sesiones sin commit) | `chrono gaps --from a --to b --via clave` |

## Fase 5 · Superficie MCP simple

Al final de las fases de datos, para consolidar sobre la forma definitiva de las consultas.

| Acción | Hecho cuando |
|---|---|
| **P-MCP-1** De 23 tools a ~8 (`informe`, `query`, `events`, `precede`, `correlate`, `explain`, `git`, `sources`), con filtros declarados una vez y alias para no romper skills | Esquemas ≤ 7 KB; eval de la fase 0 sin pérdida de aciertos |
| **P-CTX-1** Cursores `(at_epoch, id)`, ids de plantilla (`t:4812`) usables como filtro, `q=` canónica reutilizable | Una consulta se puede continuar o refinar sin repetir resultados |
| **P-CTX-3** Hook `stop` (`verify_numbers`) como contrato: una cifra citada que no sale de chrono se marca | Test con transcripciones de ejemplo |
| Skills y `instructions` del MCP reescritas sobre la nueva superficie | Instrucciones < 1 KB; skills sin nombres de tools viejos |

Cierre: versión 0.5.0, eval comparada con la fase 0 (aciertos, tokens y elección de tool).

## Fase 6 · Contenido de ficheros con extractores externos

Para la carpeta de pruebas. El binario no incorpora OCR, audio ni PDF: llama a lo que el usuario declare.

| Acción | Hecho cuando |
|---|---|
| **Extractores por tipo en la config** (generalización de `cmd:`): `pdf → pdftotext`, `image → exiftool -json`, `audio → whisper` u Ollama; salida JSONL normalizada | Un fichero de la fuente `dir` genera eventos hijos (páginas, metadatos, segmentos de transcripción) enlazados al fichero |
| **Caché por hash** de contenido + tiempos de espera + registro de fallos por fichero | Un `sync` sin cambios no reextrae nada; un extractor colgado no bloquea la ingesta |
| **Plantillas y FTS sobre el texto extraído** | `search` y `patterns` sobre el contenido de los PDF |

**Validación de la fase:** la carpeta real de 2,2 GB. Pregunta tipo: «pruebas sin decisión, y qué documentos
mencionan cada una». Depende de que exista una clave (fase 4); si no existe, se decide si pasar a la fase 8.

## Fase 7 · Escala y minería

| Acción |
|---|
| **P-ESC-5** Retención/TTL que conserva rollups y borra eventos crudos viejos |
| **P-ESC-6** Ingesta en dos etapas: parseo en hilos (`std::thread`), escritura SQLite en uno |
| **P-MSR-1** Coupling con lift y dirección; hotspots con decaimiento temporal |
| **P-MSR-2** `introducers` (SZZ acotado a hotspots, sobre el blame de `explain`) |
| **P-MSR-4 (parte 2)** Fusión de plantillas con un paso tipo Drain determinista; evaluación con Loghub-2.0 |
| **P-MULTI-1** Multi-repo con `add` + `--source`, sin servidor |
| **P-BIN-3** Recortes del binario guiados por `cargo bloat` (opciones `OMIT_*` de SQLite) |

## Fase 8 (opcional) · Relaciones por significado

Solo si la fase 6 demuestra que hay relaciones sin clave que importan. Embeddings con proveedor externo
(Ollama o API), vectores int8 en el `.db` y búsqueda por fuerza bruta (roadmap, fase 5). Nunca el modelo dentro
del binario.

---

## Dependencias

```
F0 ──► F1 ──► F2 ──► F3 ──► F4 ──► F5
                │             │
                └────────────►F6 (necesita la fuente dir de F2 y las relations de F4)
F7 después de F3 (sus piezas son independientes entre sí)
F8 solo si F6 lo justifica
```

## Cómo se ejecuta: sesión única con agentes de verificación, no un grafo de implementadores

**Decisión:** una sesión principal por fase que implementa en secuencia, con agentes de tarea única **solo para
verificar y medir**. No se monta un grafo de agentes implementadores que solo saben hacer su parte.

**Por qué no un grafo de implementadores:**
- El núcleo es compartido. `schema.sql`, `store.rs`, `index.rs`, la config y `chrono-mcp` los tocan casi todas
  las fases. Agentes aislados que no ven el resto escriben piezas que encajan mal; el coste de integrarlas
  supera lo que se ahorra.
- Las fases dependen unas de otras (F3 → F4 → F5). Paralelizar implementación no acorta el camino crítico.
- Cada fase es del tamaño de una sesión; con un resumen al cerrar fase el contexto no se desborda.

**Dónde sí sirven agentes de tarea única** (y ya existen):

| Papel | Agente | Cuándo |
|---|---|---|
| Reglas de trabajo de Juan | `estilo-juan` | Al arrancar cada fase |
| Pérdidas silenciosas (topes, incrementales) | `auditor-silencios` | F2, F3, F6 (ingesta y migración) |
| Señales que no pueden fallar | `auditor-senales` | Tras cada fase (tests, bench, eval) |
| Secretos y falsos positivos | `auditor-secretos` | F0 (fixtures), F1 (P-SEC-1), F6 (texto extraído) |
| Exclusión mutua | `auditor-cerrojos` | F1 (P-ROB-1), F7 (ingesta en hilos) |
| Entorno y CLIs externos | `auditor-entorno` | F6 (extractores) |
| Revisión del diff | `/code-review` | Antes de cada merge |

Estos agentes solo leen, así que no chocan entre sí ni con la sesión, y no confirman su propio trabajo: el
que implementa no es el que verifica.

**Paralelo, solo donde los ficheros no se cruzan:** en la fase 1, `P-BIN-2` (Makefile/flake/install) y
`P-BIN-4` (crate de hooks) pueden ir en sesiones aparte con su propio worktree. El preset `claude-sessions` de
la fase 2 es solo configuración y se puede preparar en paralelo con la fase 1.

**Puerta de cada fase:** tests verdes → bench y eval comparados con la fase anterior → auditores de la tabla →
`/code-review` → subir versión → commit. Si una cifra objetivo no se cumple, se dice y se decide antes de seguir.
