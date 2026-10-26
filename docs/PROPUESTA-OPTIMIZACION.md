# Propuesta de optimización de chrono (revisión contra el código, 0.2.7)

Revisado sobre el worktree `chrono-ingesta-distribucion-filosofia-0c7de5` (commit `1fcd549`, versión 0.2.7).
Todo lo marcado **[medido]** se ejecutó aquí (comandos al final); lo marcado **[estimado]** es juicio sin medir.
Fixture de escala: 1 000 000 de líneas JSONL sintéticas (151 MB; 20 servicios, 8 hosts, 12 plantillas de mensaje,
5 niveles), ingeridas con `chrono init events.jsonl` del binario release compilado desde este worktree.

## Resumen ejecutivo

- **El encuadre «computation-augmented, no RAG» es correcto** y el código lo sostiene: índice SQLite de eventos
  unificado (`rust/crates/chrono-store/schema.sql`), agregados precalculados (`templates`/`rollups`), clasificación
  por reglas, y una superficie MCP con cabecera de contrato (`total/coverage/truncated/index/q`). Las cinco
  disciplinas están bien nombradas; lo que falla en la propuesta previa es el **diagnóstico de varios huecos que ya
  están cerrados** (coupling ya trae support/confidence; rollups ya son incrementales; bugs no clasifica solo por
  mensaje) y el **olvido de los que sí están abiertos** (rollups sin dimensiones, `attrs` como JSON escaneado,
  plantillas que se fragmentan, PRAGMAs de ingesta aplicados a toda lectura, 23 tools en el prompt).
- **Dato nuevo que cambia prioridades:** el binario ya es un único ejecutable Rust estático de **2,2 MB** [medido]
  (el Go de `cmd/`/`internal/` es el v0.1.1 congelado desde 2026-09-19 y el `Makefile`/`flake.nix` aún lo compilan:
  deuda de distribución, no de arquitectura).
- **Cinco acciones de mayor impacto** (detalle en §3):
  1. `dim_rollups` (fuente × plantilla × nivel × entidad × bucket) + perfil de PRAGMAs de solo-lectura: `informe`
     pasa de **8,05 s / 506 MB RSS** a 1M eventos [medido] a sub-segundo y decenas de MB [estimado].
  2. Dejar de duplicar el registro en `events.attrs`: 141 MB de 254 MB de la tabla `events` a 1M son `attrs` que
     repiten `msg/level/ts/service` ya promovidos a columnas [medido]; columnas de dimensión declaradas por config
     (`dims`) con índice en vez de `json_extract` (hoy `SCAN events`, 1,9 s por `top attr:`).
  3. Plantillas: arreglar la fragmentación medida (68 plantillas para 12 patrones; `retry …` genera 32 variantes
     porque un id hex de 8 letras sin dígitos no se enmascara) antes que cambiar de algoritmo.
  4. Superficie MCP: 23 tools = **18,8 KB (~5,4k tokens) de esquemas + 2 KB de instrucciones en cada sesión**
     [medido]; consolidar en ~8 tools (`informe`, `query`, `events`, `precede`, `explain`, `trace`, `sources`,
     `git` con `op=`) y recortar los defaults que inflan (`events` por defecto devuelve 28,7 KB, `precede` 22 KB).
  5. Relaciones por configuración: `links` ya existe como mecanismo de unión por clave (`rel → campo`), pero solo lo
     consumen `trace`/`tickets`; falta declararlas como `relations` de primera clase con su índice y exponerlas a
     `correlate`/`events`/`precede` (unir por `trace_id`, `sha`, `host`, además de por tiempo).

## 1. Correcciones a la propuesta previa

| # | Se dijo | Lo que dice el código | Corrección |
|---|---|---|---|
| C1 | «chrono es un binario Go con un núcleo en Rust» | `rust/crates/chrono-cli` es el binario completo (0.2.7, 2 227 776 B, solo enlaza `libSystem`) [medido: `otool -L`]. El Go (`cmd/chrono/main.go`, `internal/*`, 2 827 líneas) es el v0.1.1: último commit 2026-09-19 (`git log -1 -- internal cmd`), `docs/DECISIONS.md:14` lo declara superado. No hay cgo ni dos binarios. | Es un binario Rust. El Go es deuda: `Makefile:9` y `flake.nix:12` (`buildGoModule`) compilan el binario viejo; `install.sh:121` cae a `go build` si no hay release. La release real sí es Rust (`.github/workflows/release.yml:42`). |
| C2 | (c) «coupling: pasar a support/confidence de reglas de asociación» | `chrono-metrics/src/coupling.rs:12-13,42-64`: `support = COUNT(*)`, `confidence = support/total`, `HAVING support >= min_support`, excluye `is_bulk`. | Ya está. Lo que falta es **lift** (confidence / P(B)) y el **sentido** (A→B ≠ B→A); ver P-MSR-1. |
| C3 | (b) «finalize rehace todo O(eventos) (DECISIONS.md:50)» | La línea 50 es la decisión 18 (histórica). La decisión 25 y `chrono-cli/src/index.rs:743-745` + `store.build_rollups_sync` agregan solo `rowid > meta.rollup_max_rowid`; FTS igual (`fts_sync`). `sync` sin cambios: **0,00 s a 1M** [medido]. | Los rollups **ya son incrementales**; DBSP/differential dataflow sobra. El hueco real es que **los rollups no tienen dimensiones** (solo `source × template × bucket`, `schema.sql:163-171`), así que `patterns --level`, `--entity`, `timeline --by` y todo `informe` vuelven a escanear `events` (`patterns.rs:85-111` lo documenta). |
| C4 | (d) «bugs: si solo clasifica por mensaje, pasar a SZZ» | `bugs.rs:46-68`: lee `labels(task='is_fix')`; `classify-rules/src/lib.rs:415-430`: `is_fix` por prefijo conventional `fix:`, por `Link{rel:"reverts"}` (señal git nativa) o por `fix_keywords`; además `labels.source='forge'` desde etiquetas de PR/issue (`index.rs:733-735`, `bug_labels`). Es «dónde se concentran los arreglos», no «qué commit introdujo el bug». | No es solo mensaje. SZZ (Śliwerski/Zimmermann/Zeller 2005) aquí sería **bug-introducing** (blame de las líneas borradas por cada fix hacia el commit origen): se puede hacer barato porque `explain` ya hace blame a la revisión indexada (`blob_lines`) y está en `ROADMAP.md` punto 4 («incremental blame only on hotspot files»). Es otra métrica (`introducers`), no un reemplazo de `bugs`. Coste M, valor alto para «qué es peligroso tocar». |
| C5 | (e) «precede con lift/significancia frente a frecuencia base» | `precede.rs:150-215`: por episodio agrega (nivel, plantilla, entidad) en la ventana previa; `Common.episodes` = en cuántos episodios apareció. No hay frecuencia base de la plantilla fuera de las ventanas. | Correcto y sigue pendiente. Una plantilla que aparece cada minuto «precede» a todo. Añadir `baseline = rollups de esa plantilla / nº de ventanas del mismo tamaño en el periodo` y `lift = P(plantilla|ventana previa) / baseline`; con `rollups` por plantilla y bucket de 60 s el baseline es una suma, O(buckets). Coste S. |
| C6 | (a) «Drain-light; considerar Drain completo/Brain/LILAC; `<NUM>` agresivo» | `chrono-core/src/template.rs:14-27`: no hay árbol de prefijos ni umbral de similitud; es **enmascarado por token** (`<UUID>/<IP>/<TS>/<HEX>/<NUM>` y tiras de dígitos dentro de palabras, línea 12). No es Drain-light; es el paso de *preprocessing* de Drain. | El problema no es «demasiado agresivo» sino **inconsistente**: en el fixture, `pyo3` → `pyo<NUM>` (confirmado) y además un id de 8 hex sin dígitos (`aebaadfc`) **no** se enmascara, uno con dígitos sí (`<HEX>`), y uno solo de dígitos va a `<NUM>` → `retry <NUM>/<NUM> for job …` produce **32 plantillas** y `auth token … expired` **25**, 68 en total para 12 patrones reales [medido]. Antes de Drain completo: (i) posición/longitud fija del token (8/12/16/32/40 hex) como señal, (ii) un paso de fusión posterior tipo Drain (plantillas con mismo nº de tokens y ≥ k iguales → wildcard `<*>`), determinista y O(plantillas²) sobre decenas-miles, no sobre eventos. Evaluar con Loghub-2.0 sigue siendo buena idea pero es evaluación, no arreglo. |
| C7 | «rollups = vistas materializadas OLAP» | Cierto para `patterns`/`timeline` sin filtros; **falso** para cualquier consulta con `level`/`entity`/`attr`/`until`: `top entity` 1,56 s/134 MB, `top attr:host` 1,87 s/584 MB (`EXPLAIN`: `SCAN events`), `patterns --level` 0,38 s/419 MB, `informe` 8,05 s/506 MB a 1M [medido]. | La capa OLAP existe solo en una dimensión. Ver P-ESC-1. |
| C8 | «bench/ ya mide tokens» | `bench/agentes/simular.py` + `RESULTADOS.md`: mide bytes/4 de salida cruda vs `--compact` por caso, sobre AuraCRM. No mide aciertos ni elección de herramienta. | Correcto a medias: hay medición de tokens, no de calidad. Ver P-EVAL-1. |
| C9 | «hooks de Claude Code y un intermediario» como pieza menor | `chrono-cli/src/hook.rs` son **1 397 líneas** (el fichero más grande del repo): `read` (deniega leer ficheros de datos ≥ 5 MB y da el resumen, `hook.rs:832`), `after-read`, `post` (adaptadores `loki` con *prefetch* y re-petición hasta 50k líneas, `records`), `prompt`, `after-edit`, `stop` (`verify_numbers`: coteja cifras de la transcripción con el índice). | El intermediario es una de las piezas con más lógica y menos tests (0 `#[test]` en `hook.rs`; 443 en el workspace). Merece su crate y sus tests. |

## 2. Lo que la propuesta previa acertó (y está en el código)

- Modelo de eventos unificado con `touches`/`links`/`labels`/`markers` (`schema.sql`), adaptadores aislados por
  crate con `detect()` conservador (decisión 15), `Source` como puerto (`chrono-core/src/source.rs:26`).
- Determinismo como contrato: salida ordenada con desempates, `.db` byte-idéntico con `CHRONO_NOW`, `index=` hash en
  cada cabecera MCP (`contract.rs:35-45`) → el agente puede saber si dos respuestas vienen del mismo estado.
- Economía de contexto ya pensada: `--compact` (2-5×: `top entity` 1 644 B → 284 B [medido]), `token_budget`,
  `truncated`, avisos de cobertura (`contract.rs:117-160`), `informe` con «consultas siguientes» ya escritas.
- Escala de ingesta resuelta razonablemente: **39k ev/s, 25,6 s para 1M, RSS pico 618 MB** [medido] (coincide con
  `docs/BENCHMARKS.md`), lotes de 100k, FTS y rollups incrementales.

## 3. Propuestas

Formato: problema (evidencia) → propuesta → referencia → impacto → coste S/M/L → riesgo para la ligereza.

### Eje A. Relaciones dinámicas por configuración

**Qué existe hoy** (`chrono-classify-rules/src/lib.rs:47-60`, único `Config`):
`fix_keywords`, `ticket_patterns`, `exclude_globs`, `bug_labels`, `bug_categories`, `source_options`
(por fuente: `time_field`, `message_field`, `level_field`, `entity_field`, `kind`, `id_field`, `links`, `explode`,
`explode_where`, rutas anidadas; csv `delimiter/time_column`; textlog `preset/year`; `template` con marcadores),
`body_max_bytes`, `max_events_per_source`, `commands` (fuentes `cmd:` con argv sin shell, `records`, `options`).
El `.chrono/intermediario.json` es un segundo fichero con `rules[]` por tool MCP.
**Qué falta**: no hay forma de declarar una dimensión indexable, una clave de unión entre fuentes, ni una relación
temporal con nombre. `event_dims` está diseñado (`DESIGN-GENERAL-CORE.md:153,164-166`) y nunca se construyó.

**P-REL-1. `dims` declaradas por fuente → tabla `event_dims` indexada.**
- Problema: `attrs` es JSON TEXT; filtrar/agrupar por `attr` es `json_extract` en `SCAN events`
  (`top.rs:123`, `timeline.rs:228-230`, `stats.rs:53-61`; `EXPLAIN` [medido]: `SCAN events`, 1,87 s/1M).
- Propuesta: en `source_options` (y en `commands[].options`) una clave `dims: "host,container,status"`; el `Writer`
  escribe `event_dims(key, value, event_id)` solo para esas claves (baja cardinalidad) con índice
  `(key, value, event_id)` y las retira de `attrs`. `top attr:k`, `--attr k:v`, `stats --by attr:k` pasan a
  `SEARCH event_dims`. Validación: cardinalidad máxima configurable (p. ej. 10k valores) con aviso en `sources`.
- Referencia: EAV acotado / «dimension tables» de modelado en estrella; ya previsto en el diseño propio.
- Impacto: alto (toda consulta por atributo, y es la base de P-REL-2 y P-ESC-1). Coste M (reindex, `schema_version` 6).
  Riesgo ligereza: nulo en binario; el índice crece ~20-30 B por (evento, dim) [estimado].

**P-REL-2. `relations` por clave entre fuentes (joins declarativos).**
- Problema: la única unión entre fuentes es temporal (`correlate` ±Δt, `correlate.rs:95-96`) o por `links`
  (`rel,target`) que solo consumen `trace`/`tickets`/`session_patterns`. Un `trace_id` en logs y el mismo en
  trazas de otra fuente, o `sha` en CI y en git, no se pueden pedir desde `events`/`precede`.
- Propuesta: en `config.json`:
  ```json
  "relations": [
    {"name": "by_trace", "key": "trace_id", "sources": ["cmd:loki", "traces.jsonl"]},
    {"name": "ci_commit", "from": {"source": "cmd:gh-runs", "field": "headSha"}, "to": {"source": "git", "id_prefix": true}},
    {"name": "near_deploy", "kind": "temporal", "anchor": "kind=deploy", "window": "-10m..+30m"}
  ]
  ```
  Implementación: una relación por clave es exactamente un `Link{rel: name, target: valor}` emitido por el `Writer`
  a partir de `dims`/campos (reutiliza `links(rel,target)` e `idx_links_rel`, `schema.sql:79-85,132`); no hay
  tabla nueva. Lo nuevo es la **superficie**: `events --join by_trace=<valor>`, `correlate <id> --via ci_commit`,
  `precede … --join by_trace` (precursores del mismo trace, no solo de la misma ventana), e `informe` que liste las
  relaciones disponibles. Las temporales con nombre son azúcar sobre `correlate` con `kind` y ventana asimétrica.
- Referencia: event sourcing con *correlation id*; OpenTelemetry `trace_id/span_id` como clave de unión canónica;
  reglas de asociación entre fuentes (Gall/Zimmermann) pero por clave explícita.
- Impacto: alto para la visión («relacionar entre sí de forma dinámica… desde config»). Coste M (escritura S porque
  `links` ya existe; consultas M). Riesgo ligereza: nulo.

**P-REL-3. Un solo fichero de configuración con esquema y `chrono config check`.**
- Problema: `config.json` (reglas + fuentes + comandos) e `intermediario.json` (reglas por tool) se cargan con
  `serde_json::Value` a mano en el hook (`hook.rs:81-83`), sin validación ni mensajes de clave desconocida; las
  opciones por adaptador no están enumeradas en ningún sitio del binario (solo en docs/skills).
- Propuesta: `chrono config check` (valida, lista claves desconocidas, muestra qué opciones acepta cada adaptador
  vía `Source::options_spec()` en el trait) y `chrono config example <kind>`; mover `intermediario` bajo
  `config.json#intermediario` (manteniendo lectura del fichero viejo).
- Impacto: medio (menos sesiones perdidas en «por qué no coge mi opción»). Coste S. Riesgo: nulo.

### Eje B. Superficie MCP

**P-MCP-1. Consolidar 23 tools en ~8 y adelgazar esquemas.**
- Problema [medido]: `tools/list` = **18 761 caracteres (~5,4k tokens)**; `instructions` 1 958 (~0,56k). Siete tools
  (`precede`, `informe`, `timeline`, `top`, `patterns`, `events`, `stats`) repiten los mismos 11-16 filtros
  (`since/until/level/entity/attr/source/kind/grep/infer_level/format/limit`) → 13,5 KB solo ellas. Ocho tools de
  git (`hotspots/coupling/owners/churn/bugs/tickets/phases/similar/branches/prs`) pesan poco pero ocupan ranuras y
  decisiones. Cada sesión paga esto antes de la primera pregunta.
- Propuesta:
  - `informe` (como hoy, puerta de entrada), `query` (`op: top|patterns|timeline|stats` + filtros comunes **una
    sola vez**), `events` (lectura con filtros + `join`), `precede`, `correlate` (absorbe `similar` y `trace` con
    `mode`), `explain` (absorbe `informe codigo`), `git` (`op: hotspots|coupling|owners|churn|bugs|tickets|phases|
    branches|prs`), `sources` (absorbe `stats` de índice). Objetivo: ≤ 7 KB de esquemas [estimado], mismas
    capacidades.
  - Descripciones: las de `precede` (408) e `informe` (395) son párrafos; mover ejemplos a `instructions` o a la
    skill y dejar una frase + un ejemplo.
  - Mantener nombres viejos un tiempo como alias en `call_tool` (sin exponerlos en `tools/list`).
- Referencia: guías de *tool design* para LLM (pocas herramientas ortogonales > muchas específicas; el coste de
  selección crece con el nº de tools y con esquemas repetidos).
- Impacto: alto en «consumir todo de forma simple» y ~4-5k tokens por sesión. Coste M (plugin skills y
  `bench/agentes` referencian nombres). Riesgo: nulo en binario.

**P-MCP-2. Defaults que caben en contexto.**
- Problema [medido, 1M]: `events level=ERROR` sin `limit` → **28 678 B** (100 eventos, `contract.rs:23`);
  `precede level=ERROR grep=refused` → **22 136 B** (50 episodios × hasta N precursores); `timeline by=level` →
  4 270 B; en cambio `informe` 5 294 B y `top` 536 B están bien.
- Propuesta: `events` por defecto 25 y en `--compact` una línea por evento (`at level entity title` truncado a 160
  caracteres, `id` corto); `precede` por defecto 10 episodios y 3 precursores por episodio, con
  `episodes_total` y `common_precursors` íntegros (son lo que importa); `timeline` recorta a buckets no vacíos y
  marca huecos. Añadir `max_bytes` opcional al contrato: si la respuesta supera el presupuesto, chrono recorta la
  lista y lo dice en la cabecera (`truncated=yes budget=…`).
- Impacto: alto (es el caso donde hoy chrono llena contexto). Coste S. Riesgo: nulo.

### Eje C. Economía de contexto

**P-CTX-1. Handles de resultado y cursores.**
- Problema: no hay `cursor`/`offset` en ninguna tool (grep en `chrono-mcp`/`chrono-metrics`: ninguno); paginar es
  repetir la consulta con `since` ajustado. Los resultados repiten `template` completas (hasta 200 caracteres) en
  `patterns`, `precede` e `informe`.
- Propuesta: (i) cursor opaco = `(at_epoch, id)` del último elemento, aceptado por `events` y `precede`;
  (ii) **ids de plantilla** (`templates.id` ya existe) en toda salida: `t:4812` en vez del texto tras la primera vez
  dentro de la misma respuesta, y aceptados como filtro (`template=t:4812`) en `events`/`timeline`/`precede`;
  (iii) `q=` canónica ya se devuelve (`contract.rs:49`): aceptar `q` como argumento para re-ejecutar/paginar sin
  reescribir filtros.
- Referencia: *context engineering* (referenciar en vez de repetir; «just-in-time retrieval»).
- Impacto: medio-alto. Coste S-M. Riesgo: nulo.

**P-CTX-2. Niveles de respuesta explícitos (`depth`).**
- Problema: `informe` ya es el nivel «panorama», pero las demás tools devuelven siempre el nivel «detalle»; el
  agente no puede pedir «solo cifras» de `precede` (hoy hace falta leer 22 KB para saber que hubo 50 episodios).
- Propuesta: `depth: counts|summary|full` común: `counts` = cabecera + totales (≈ 150 B), `summary` = el
  `--compact` recortado, `full` = JSON. Es un parámetro en `call_tool` que trunca antes de serializar.
- Impacto: medio. Coste S. Riesgo: nulo.

**P-CTX-3. El hook `stop` (`verify_numbers`) como contrato de calidad, no solo aviso.**
- Problema: `hook.rs:1108-1206` coteja números de la transcripción con el índice por heurística de dígitos; sin
  tests y sin medir su tasa de falsos positivos.
- Propuesta: registrar en `.chrono/avisos/` (ya existe) cada aviso con el número, la tool de origen y si el modelo
  corrigió; dar `chrono avisos --stats`. Es el dato que falta para P-EVAL-1.
- Coste S. Riesgo: nulo.

### Eje D. Ligereza y sofisticación del binario

**Medido** (release, `opt-level=z`, LTO, `strip`, `panic=abort`, `rust/Cargo.toml:26-31`):
binario **2 227 776 B** (el instalado en `~/.cargo/bin` es 0.2.0-dev, 2 027 760 B; el plugin no lo refresca),
enlaza solo `libSystem.B.dylib`, SQLite 3.46 *bundled* con FTS5; árbol de dependencias de **61 crates**, de ellos
**23 externos** (`rusqlite/libsqlite3-sys`, `serde/serde_json`, `hashbrown/indexmap`, `syn/quote/proc-macro2` solo
en compilación, `zerocopy`, `ahash`…): sin `regex`, sin `tokio`, sin `clap`. Arranque `--version`: 0,00 s
(≤ 10 ms). Consulta trivial sobre índice pequeño: 3,6-4 MB RSS. 25 788 líneas Rust, 443 tests.

**P-BIN-1. Perfil de PRAGMAs de solo-lectura.** *(la mayor incoherencia con «ligero»)*
- Problema: `Store::open` (`store.rs:36-92`) es el único `open` y aplica a **toda** apertura
  `synchronous=OFF`, `cache_size=-262144` (256 MB), `temp_store=FILE`, `mmap_size=256 MB`, `foreign_keys=OFF`. Lo
  usan igual la ingesta, cada consulta CLI, el servidor MCP y **los hooks que corren en cada prompt/Read/Edit**.
  Medido a 1M: `top attr:host` 584 MB RSS, `patterns --level` 419 MB, `informe` 506 MB. Es la page cache que crece
  hasta el tope, no una fuga; pero un hook de 10 s de timeout no debería poder tocar medio giga.
- Propuesta: `Store::open_read(path)` con `SQLITE_OPEN_READONLY`, `cache_size=-16384` (16 MB), `mmap_size` 64 MB,
  `query_only=1`; la ingesta conserva el perfil actual. Los hooks y `mcp` abren en lectura salvo `sync`.
- Impacto: alto en RSS de consultas y hooks (de cientos de MB a decenas [estimado]); latencia casi igual porque
  los índices covering caben en 16-64 MB. Coste S. Riesgo: ninguno; mejora la ligereza.

**P-BIN-2. Retirar el Go y alinear la distribución.**
- Problema: `Makefile` (`build`/`install`/`release` con `CGO_ENABLED=0 go build`), `flake.nix:12`
  (`buildGoModule`), `install.sh:121` (fallback `go build`) y `go.mod` empaquetan el v0.1.1; el README cita
  `v0.1.0`; el `ensure-chrono.sh` del plugin puede acabar instalando un binario distinto al de la release.
- Propuesta: `Makefile` → `cargo build --release -p chrono-cli` (y `make go-legacy` si se quiere conservar un
  ciclo), `flake.nix` → `rustPlatform.buildRustPackage`, borrar `cmd/`/`internal/`/`go.mod` (están en git si hacen
  falta) o moverlos a `legacy/`. Añadir `cargo bloat --release` al `bench` de release para vigilar el tamaño.
- Impacto: medio (claridad, menos superficie), coste S. Riesgo: nulo.

**P-BIN-3. Qué se puede recortar del binario [estimado, sin `cargo bloat`].**
- SQLite bundled es ~1,2-1,4 MB de los 2,2; FTS5 y JSON son necesarios; se pueden quitar con `SQLITE_OMIT_*`
  las partes no usadas (`-DSQLITE_OMIT_DEPRECATED`, `OMIT_SHARED_CACHE`, `OMIT_LOAD_EXTENSION`,
  `OMIT_AUTHORIZATION`, `OMIT_PROGRESS_CALLBACK`) vía `libsqlite3-sys` `bundled` + `cc` flags: 100-200 KB.
- `serde_json` con `preserve_order` si no se usa; `hashbrown`/`indexmap` solo donde haga falta; `session_patterns.rs`
  (1 086 líneas) e `hook.rs` como *features* de Cargo (`--no-default-features` para un `chrono-lite` de consulta).
- Impacto: bajo (ya es pequeño); coste S-M. Hacerlo solo si se mide con `cargo bloat`.

**P-BIN-4. `hook.rs` y el intermediario a su crate con tests.**
- Problema: 1 397 líneas, cero tests, ejecuta `sh -c` para `token_cmd` (`hook.rs:361-375`, solo desde config, bien),
  habla HTTP con Loki a mano. Es el componente con más riesgo de regresión y el que corre más veces.
- Propuesta: `chrono-intermediary` crate con adaptadores `loki`/`records` como trait y fixtures de respuesta; medir
  el tiempo de cada hook en `.chrono/avisos` (p50/p95) y exponerlo en `chrono bench --hooks`.
- Coste M. Riesgo: nulo.

### Eje E. Rendimiento de ingesta y consultas a escala

**Medido a 1M eventos (JSONL, 151 MB):** `init` 25,6 s (39k ev/s), RSS pico 618 MB, `.db` **584 MB (584 B/evento,
3,9× el input)**. Desglose `dbstat`: `events` 254 MB (de ellos `attrs` 141 MB, `title` 31 MB, `at` TEXT 24 MB,
`id` 16 MB), `touches` + sus 2 índices 88 MB, `autoindex_events_1` (PK TEXT) 26 MB, `idx_events_level` 22 MB,
`idx_events_kind` 21 MB, `idx_events_actor` 18 MB (actor vacío en logs), FTS 19 MB, 4 bandas SimHash ~50 MB.
Consultas: `sync` no-op 0,00 s; `patterns` 0,02 s; `timeline` 0,02 s; `search` 0,09 s; `top level` 0,07 s;
`top entity` 1,56 s; `top attr:host` 1,87 s; `patterns --level` 0,38 s; `precede` 0,23 s; `timeline --by` 0,28 s;
**`informe` 8,05 s**.

**P-ESC-1. `dim_rollups` + `informe` sobre rollups.**
- Problema: `informe` (`informe.rs:30-46`) hace por cada uno de 4 niveles `top entity` (JOIN a `touches`),
  `top attr:container` (SCAN), `patterns_scoped` ×2 (SCAN con `templatize` por fila) y luego `precede`: 8 s a 1M.
  La propia documentación lo señala como pendiente (`BENCHMARKS.md:74`, decisión 27 «aún O(N)»).
- Propuesta: tabla `dim_rollups(source_ref, template_id, level, entity_id, bucket_epoch, count, last_id)` con PK
  en ese orden e índices `(level, bucket_epoch)` y `(entity_id, bucket_epoch)`; mantenida con el mismo watermark
  incremental que `rollups` (`build_rollups_sync`). `top level|entity`, `patterns --level/--entity`, `timeline
  --by level|entity` e `informe` leen de ahí cuando la ventana se alinea al bucket (misma regla de paridad que
  decisión 26c). Las dims de P-REL-1 pueden entrar como una fila más por `(dim_key, dim_value)` si se quiere
  `top attr:` precalculado (cuidado con cardinalidad).
- Referencia: cubos OLAP parciales / *summary tables*; mantenimiento incremental por watermark (ya en uso).
- Impacto: alto (informe sub-segundo [estimado]; todas las consultas por nivel/entidad O(buckets)). Coste M
  (reindex). Riesgo ligereza: el índice crece ~5-10 % [estimado]; ninguno en binario.

**P-ESC-2. Dejar de duplicar el registro en `attrs` y comprimir lo que queda.**
- Problema [medido]: la primera fila del fixture guarda `attrs = {"host","level","msg","service","trace_id","ts"}`
  completo aunque `msg→title`, `level→level`, `ts→at/at_epoch`, `service→touches` ya están en columnas; `at` TEXT
  repite `at_epoch`. 141 + 24 MB de 584.
- Propuesta: el `Writer` quita de `attrs` las claves ya promovidas (`time_field`, `message_field`, `level_field`,
  `entity_field`, `id_field`) y las de `dims`; `at` TEXT se deriva al leer (`iso()` ya existe en `contract.rs:93`).
  Opcional: `attrs` como JSONB (SQLite ≥ 3.45, el bundled es 3.46: `jsonb()` al escribir, 5-10 % menos y
  `json_extract` más rápido). Esperable: −25-30 % de `.db` en logs [estimado].
- Coste S-M (reindex). Riesgo: nulo.

**P-ESC-3. Claves enteras para `events` (rowid como clave de `touches`/`labels`/`links`).**
- Problema: `events.id TEXT PRIMARY KEY` aleatorio (16 hex para logs, sha para git) y `touches(event_id TEXT,…)`,
  `labels`, `links`, `rollups.first_id/last_id` repiten ese texto; `BENCHMARKS.md` atribuye a esto la degradación
  residual de ingesta; `autoindex_events_1` 26 MB + `autoindex_touches_1` 29 MB + `idx_touches_ent` 28 MB.
- Propuesta: `touches/labels/links` referencian `events.rowid` (INTEGER) y `events.id` pasa a `UNIQUE`; las
  salidas siguen enseñando `id`. Es el mismo movimiento que la decisión 27 hizo con `source_ref` (−36,5 %).
- Impacto: −15-25 % de `.db` y mejor localidad [estimado]. Coste M (reindex; toca a todos los lectores).
  Riesgo: nulo.

**P-ESC-4. Índices que no pagan en logs.**
- `idx_events_actor` (18 MB) y `idx_events_kind` (21 MB) en una fuente con un solo `kind` y sin actor; las 4 bandas
  SimHash (~50 MB) solo sirven a `similar` con `max_dist ≤ 3` (decisión 26d: el default no las usa).
- Propuesta: índices parciales (`WHERE actor_id IS NOT NULL`, `WHERE kind <> 'log'`), y bandas SimHash opt-in por
  fuente (`similar` es de commits). Coste S. Impacto: −10 % `.db` [estimado].

**P-ESC-5. Retención/TTL y particionado por tiempo (hoy inexistente).**
- Problema: no hay `retention`, `prune` ni nada parecido (grep: 0 resultados). Con `cmd:loki` sincronizando cada
  sesión, el índice solo crece; la decisión 25 descarta el *sharding* por romper `correlate`/`trace`.
- Propuesta mínima: `retention_days` por fuente en `source_options`; `chrono prune` borra `events` (y sus hijos)
  anteriores al corte **conservando `rollups`/`dim_rollups`** de ese periodo (los agregados sobreviven al detalle:
  es lo que un agente pregunta sobre lo viejo), seguido de `wal_checkpoint` y `VACUUM` opcional. Particionado
  físico no: una tabla por mes multiplicaría los JOIN; la PK `(source_ref, at_epoch)` ya ordena bien.
- Referencia: *downsampling* de TSDB (Prometheus/Thanos: raw → 5m → 1h con retención distinta).
- Impacto: alto para uso continuo. Coste M. Riesgo: nulo.

**P-ESC-6. Ingesta: paralelizar parseo, no SQLite.**
- Problema: `init` 25,6 s/1M a 150 % CPU [medido: `time` del build muestra el mismo patrón]; el parseo JSON y el
  `templatize`/SimHash por línea van en el hilo del escritor.
- Propuesta: pipeline de 2 etapas (N hilos de parseo+clasificación → 1 escritor con lotes), con orden preservado
  por índice de línea para mantener el determinismo (`rowid` por orden de entrada). `std::thread` + canal acotado;
  sin `rayon`/`tokio`.
- Impacto: 2-3× en ingesta en máquinas de 4+ núcleos [estimado]. Coste M. Riesgo: +0 KB si es `std`.

### Eje F. Minería (MSR / AIOps)

**P-MSR-1. Coupling con lift y dirección; hotspots con cambio reciente ponderado.**
- `coupling.rs` ya da support/confidence; añadir `lift = confidence / (changes(B)/commits)` y la confianza inversa
  (`B→A`) separada; `hotspots.rs:73` pondera `changes × size`; añadir opción `--decay 90d` (peso exponencial por
  antigüedad) que es lo que Tornhill usa de verdad para «hot» y evita que un fichero grande y muerto quede arriba.
  Coste S cada uno.

**P-MSR-2. `introducers` (SZZ acotado).** Ver C4: para cada `is_fix`, blame de las líneas eliminadas (ya hay
`blob_lines` y el camino de `explain`) → commit introductor; métricas «fix-inducing rate» por fichero/autor. Solo
sobre hotspots para acotar el coste (ROADMAP 4). Coste M.

**P-MSR-3. Precede con baseline/lift** (ver C5) y **episodios por clave** (P-REL-2): «qué precede a los FATAL de
este `trace_id`» es más preciso que «±2 minutos en todo el índice». Coste S + lo de P-REL-2.

**P-MSR-4. Fusión de plantillas (post-paso Drain determinista).** Ver C6. Además de medir con Loghub-2.0, añadir
`chrono patterns --explain t:ID` que enseñe 3 ejemplos crudos de la plantilla (verificabilidad). Coste S-M.

### Eje G. Otros de alto impacto

**P-SEC-1. Redacción de secretos antes de indexar.**
- Problema: no hay ninguna redacción (grep `redact|secret|password|api_key` en `rust/`: solo fixtures de tests y el
  `Authorization: Bearer` del cliente Loki). Logs y salidas de `cmd:` (gh, kubectl, curl a APIs) entran a `.chrono/
  index.db` y a `.chrono/cmd/*.cmd.jsonl` (`command.rs:43`) tal cual; el `.db` es portable y se comparte.
- Propuesta: en `chrono-core` un `redact()` determinista sin regex (patrones de alta precisión: `Bearer …`,
  `AKIA…`, `ghp_/github_pat_`, `-----BEGIN … PRIVATE KEY`, `password=`/`pwd=` en query strings, JWT de 3 partes
  base64) aplicado a `title/body/attrs` en el `Writer`, con `redact: false` por fuente para quien no lo quiera y el
  conteo de redacciones en `sources`. `.chrono/.gitignore` ya existe; añadir `cmd/` si no está.
- Impacto: alto (riesgo real). Coste S-M. Riesgo ligereza: nulo.

**P-ROB-1. Consultas en lectura con `busy_timeout` y `sync` concurrente.** Los hooks lanzan `spawn_index`
(`hook.rs:527`) en segundo plano mientras otras consultas abren el mismo `.db`; con `synchronous=OFF` y un único
escritor va bien, pero `sync` desde dos sesiones (worktrees con raíz común, commit `c41c245`) puede solaparse.
Propuesta: lock de fichero `.chrono/sync.lock` (`flock`) alrededor de `sync/add/init` y salida clara «otro sync en
curso». Coste S.

**P-MULTI-1. Multi-repo sin `serve`.** `--db` ya permite apuntar a cualquier índice; falta `chrono add
../otro-repo` documentado como camino multi-repo (ya funciona por `sources` multi-fuente) y que `hotspots/owners`
acepten `--source git:<path>` para no mezclar. Coste S.

**P-EVAL-1. Eval del agente: aciertos + tokens + elección de tool.**
- `bench/agentes/simular.py` mide tokens; falta un conjunto de 20-30 preguntas con respuesta esperada (cifra exacta
  del índice) y la traza de tools usadas, ejecutable con `claude -p` sobre el fixture de AuraCRM y el de 1M.
  Métrica: acierto numérico, nº de llamadas, tokens de entrada, tasa de avisos de `verify_numbers`. Es la única
  forma de saber si P-MCP-1 mejora o empeora la elección de herramienta. Coste M.

## 4. Hoja de ruta priorizada

**Ahora (S, sin reindex, ganancias medibles de inmediato)**
1. P-BIN-1 perfil de PRAGMAs de solo-lectura (RSS de consultas/hooks).
2. P-MCP-2 defaults de `events`/`precede`/`timeline` + `depth=counts`.
3. C6/P-MSR-4 arreglo de enmascarado hex/longitud fija en `templatize` (reindex solo si se quiere recalcular
   plantillas viejas; se puede migrar con `build_rollups` completo).
4. P-BIN-2 Makefile/flake/install a Rust; retirar Go.
5. P-SEC-1 redacción en `Writer` (opt-out por fuente).

**Siguiente (M, un reindex a `schema_version` 6 que agrupe todo lo de esquema)**
6. P-REL-1 `dims` → `event_dims`; P-ESC-2 `attrs` sin duplicados y `at` derivado; P-ESC-3 claves enteras;
   P-ESC-4 índices parciales. Un solo reindex para los cuatro.
7. P-ESC-1 `dim_rollups` + `informe` sobre rollups.
8. P-MCP-1 consolidación de tools (con alias) + P-CTX-1 cursores e ids de plantilla.
9. P-REL-2 `relations` por clave sobre `links`, expuestas en `events/correlate/precede`.
10. P-EVAL-1 eval de aciertos (antes de cerrar P-MCP-1, para comparar).

**Después (L o dependientes de lo anterior)**
11. P-ESC-5 retención con agregados supervivientes.
12. P-ESC-6 ingesta en dos etapas.
13. P-MSR-2 `introducers` (SZZ acotado a hotspots); P-MSR-1 lift/decay; P-MSR-3 baseline en precede.
14. P-BIN-4 crate del intermediario con tests; P-BIN-3 recortes con `cargo bloat`.

## 5. Lo que no se pudo verificar

- No se ejecutó `cargo test` ni `cargo bloat`; los tamaños por crate dentro del binario son estimados.
- No se midió el fixture git grande (AuraCRM) ni `session_patterns`/Nod; las cifras de git provienen de
  `bench/agentes/RESULTADOS.md` y `docs/BENCHMARKS.md`.
- La ganancia de `dim_rollups` y del perfil de solo-lectura está estimada, no medida (requiere implementar).
- No se comprobó el comportamiento real de los hooks en una sesión de Claude Code (tiempos p50/p95, tasa de avisos);
  solo su código.
- El fixture de 1M es sintético (12 plantillas, niveles uniformes): la fragmentación de plantillas y la
  proporción `attrs/title` pueden diferir con logs reales (en logs reales `body` no está vacío y la proporción
  de `attrs` será menor en términos relativos).
- Loghub-2.0 no se descargó ni se evaluó `templatize` contra él.

## 6. Mediciones (comandos)

```sh
# Binario (worktree, release)
CARGO_TARGET_DIR=$S/target cargo build --release -q           # 24,6 s wall
ls -la $S/target/release/chrono                                # 2 227 776 B
otool -L $S/target/release/chrono                              # solo /usr/lib/libSystem.B.dylib
cargo tree --manifest-path rust/Cargo.toml -e normal --prefix none | sort -u | wc -l   # 61 crates (23 externos)
/usr/bin/time -p chrono --version                              # real 0.00 (×3)

# Superficie MCP
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
              '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | chrono mcp
# → 23 tools, 18 761 chars de esquemas (~5,4k tokens a 3,5 chars/token); instructions 1 958 chars
# por tool (chars): precede 2591, informe 2019, timeline 1888, top 1865, patterns 1770, events 1721, stats 1682, …

# Fixture 1M (gen.py en el scratchpad; 151 414 893 B)
/usr/bin/time -l chrono init events.jsonl     # 25,62 s real; RSS máx 618 446 848 B; .db 584 179 712 B
sqlite3 .chrono/index.db "select name,sum(pgsize)/1048576.0 from dbstat group by name order by 2 desc"
#   events 253,9 MB · touches 30,8 · autoindex_touches 28,7 · idx_touches_ent 28,5 · autoindex_events 26,5
#   idx_events_level 21,9 · idx_events_kind 20,7 · fts 19,1 · idx_events_actor 17,9 · srcref_at 17,4 · at 16,4 · sh1 12,4
sqlite3 .chrono/index.db "select sum(length(attrs)),sum(length(title)),sum(length(body)),sum(length(id)),sum(length(at)) from events"
#   141 414 893 · 30 864 700 · 0 · 16 000 000 · 23 992 000
sqlite3 .chrono/index.db "select count(*) from templates"                 # 68 (12 patrones generados)
sqlite3 .chrono/index.db "select count(*) from templates where template like 'retry%'"       # 32
sqlite3 .chrono/index.db "select count(*) from templates where template like 'auth token%'"  # 25

# Consultas a 1M (/usr/bin/time -l; salida JSON vs --compact)
informe                      8,05 s  506 MB   4 999 B
top entity                   1,56 s  134 MB   1 644 B / 284 B
top attr:host                1,87 s  584 MB     860 B / 148 B   (EXPLAIN: SCAN events)
top level                    0,07 s   60 MB     576 B /  80 B
patterns --level ERROR,FATAL 0,38 s  419 MB   2 601 B / 945 B
patterns --entity svc-3      0,99 s  439 MB   2 730 B / 992 B
patterns                     0,02 s    9 MB   3 641 B
timeline --bucket 1h         0,02 s   10 MB   3 763 B / 1 123 B
timeline --bucket 1h --by level 0,28 s 100 MB 14 314 B / 4 625 B
precede --grep refused --level ERROR 0,23 s 432 MB 50 077 B / 22 334 B
precede --level FATAL        0,24 s  432 MB   1 342 B / 564 B
events --level FATAL --limit 50 0,00 s 5 MB  20 744 B / 5 132 B
search refused               0,09 s   22 MB   4 215 B / 2 223 B
sync (no-op)                 0,00 s
chrono bench --compact       hotspots 48 ms · churn 495 ms · timeline 25 ms · top_kind 66 ms · patterns 2,8 ms · search 184 ms · similar 232 ms

# Tamaño de respuestas MCP (texto por defecto) a 1M
informe {}                       5 294 B
top {dim:entity}                   536 B
events {level:ERROR}            28 678 B   (limit por defecto 100)
precede {level:ERROR,grep:refused} 22 136 B
patterns {level:ERROR,FATAL}     1 223 B
timeline {by:level}              4 270 B

# Índice real del repo chrono (.chrono/index.db del checkout principal, 2026-10-16)
577 536 B, 71 eventos, 617 touches, 71 plantillas; informe 4,0 MB RSS, hotspots 3,7 MB RSS

# Código
go (cmd+internal, sin tests): 2 827 líneas, último commit 2026-09-19; rust: 25 788 líneas, 443 #[test]
hook.rs: 1 397 líneas, 0 tests
```
