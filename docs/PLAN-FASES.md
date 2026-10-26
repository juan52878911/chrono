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

## Estado (2026-10-26, tras F4 y sus pendientes)

**Pendientes de F2–F4 resueltos → 0.7.0 (esquema v7, sin reindex).** Sin dependencias nuevas. Tests 523 → **583**,
en verde en macOS y en Linux (contenedor `rust:1.90` en trycorepc). Detalle en las decisiones 31 a 36 de
`DECISIONS.md`.
- **Relaciones.** `gaps` por defecto solo cubre con un `to` en o después del primer `from` de la unidad
  (`--any-order` para el criterio de antes); `grep=` admite espacios, `--until` acota los dos lados,
  `from_without_key` cuenta los `from` sin valor de la clave y `units_without_time` las unidades sin ningún `from`
  con tiempo. `precede --join`: un ancla con varios valores cuenta en cada uno y `anchors_without_key` las que no
  tienen ninguno; `anchors_without_time` y `precursors_total` dicen lo que queda fuera. `--join`, `--via` y `--by`
  con una clave que no existe dan error. La frecuencia base del lift sale de las ventanas en que la fuente de la
  plantilla tuvo actividad (`active_episodes`), no de todo el periodo: en las sesiones, con `precede --level ERROR
  --entity Bash`, «File has not been read yet» baja de lift **4,74 a 0,96**. `gaps` tool_use → tool_result por
  tool_use: 133 476 unidades, 133 467 cubiertas y **9** sin resultado, justo la diferencia entre los dos kinds.
- **Ingesta y robustez.** Las opciones de una fuente (`entity_via`, `explode`, `attrs`, `links`…) entran en su
  firma: cambiarlas reindexa como cambiar una dim. Glob: un fichero se compara además por ctime y por una huella de
  lo ya leído (entero hasta 64 MB; por encima, primeros y últimos 64 KB): reescrito con el mismo tamaño y mtime, o
  editado en medio, se relee entero. Cambiar dims u opciones con parte de la fuente irrecuperable no reindexa:
  avisa de cuántos eventos se perderían y `sources` cuenta los que conservan las de antes (`stale_dims_events`).
  `sync` y `tail` siguen con las demás fuentes si una falla, salen con error al final y `sources` enseña
  `sync_error`. `init` no borra las fuentes añadidas con `add` ni un índice que no puede abrir (uno dañado se aparta
  como `.corrupt.bak`); en un repo sin commits deja la fuente git vacía y `sync` la lee al primer commit; en una rama
  huérfana lo dice. Una attr con punto se lee primero como clave literal, como una dim. `patterns` con solo fuente
  o texto sale del cubo o de `events`, exacto, como con nivel.
- **Concurrencia y reindex.** `chrono reindex` rehace el índice con las mismas fuentes (git, logs, globs,
  comandos); el error de esquema viejo da el comando exacto. Va entero bajo el cerrojo de ingesta (reentrante) y deja
  una marca `<db>.reindexing`: si se corta, los demás comandos lo avisan, las ingestas se niegan y el siguiente
  `reindex` reanuda desde la copia, que no se pisa; si salen menos eventos que antes, la copia se conserva. En un
  índice v5 real del repo: 71 → 71 eventos. La migración v6→v7 desde dos procesos: el segundo espera y relee la
  versión. Un índice nuevo crea su esquema en una transacción (sin falso «esquema ?» con aperturas a la vez),
  `open_read` no crea ficheros y la espera agotada de una migración lo dice. `tail` hace un `sync` en cada vuelta
  (rollups, plantillas, búsqueda, reindexado) sin `optimize`, pagina por `rid` y, como el MCP, reabre el índice si
  cambia el fichero. El cerrojo de `loki_index` del hook es un `flock` (antes un directorio que bloqueaba para
  siempre si el proceso moría).
- **UX de CLI y MCP.** `-h`/`--help` en cualquier subcomando sin abrir el índice; comando mal escrito con
  sugerencia; aviso de flags desconocidas o que no aplican (`top --by`, `search --since`). `result.hints` cuando un
  vacío tiene explicación (attr o nivel inexistente, `stats` no numérico, `hotspots` sin git, `correlate` de una
  fuente, `session_patterns` sin Nod, eventos sin tiempo); por MCP salen como `⚠` en la cabecera. `top <dim>` acepta
  una dim declarada sin `attr:` y una desconocida lista las que hay. `events --order desc` (MCP `order=desc`).
  `search --limit` se respeta, `informe --level`, `--compact` usa el alias de la fuente. Por MCP, `total` de `stats`
  correcto (y con `where_attr`), `(resto)` de `top attr:` solo con la attr y `(sin valor)` aparte. `sources`,
  `phases`, `tickets`, `show` y `bench`: `max_items` = lo devuelto, sin `truncated`. Errores de `gaps` y de claves en
  el idioma de `--lang`.
- **Bench en frío a 1M** (trycorepc, mediana de 7, 0.6.0 frente a 0.7.0): ingesta 20,3 → 19,8 s; `informe` 1,76 →
  1,72 s; `precede` FATAL 0,267 → 0,264 s; `events` FATAL 0,210 → 0,201 s; `patterns` sin filtros 12 → 25 ms (ahora
  sale del cubo o de `events`, exacto); el resto igual.
- **Sesiones reales de Claude** (copia congelada, 1 939 ficheros, 266 943 eventos): 0.6.0 y 0.7.0 dan idénticos
  `sources`, `top`, `patterns`, `timeline`, `informe` y `events`. Solo cambia lo buscado: `search` respeta `--limit`,
  `stats --help` es ayuda, el lift de `precede` y el campo `order` en `events`.
- Esquemas MCP 23,1 → 24,6 KB (24 tools; F5 los consolida).
- Auditorías (`auditor-silencios`, `auditor-cerrojos`, `auditor-senales`) sobre la ronda; corregido: `init` borraba
  las fuentes de `add` o un índice que no podía abrir; un fallo en una fuente paraba el `sync` entero; en `gaps`, un
  `from` sin tiempo fijaba el primero; los ficheros de la glob fuera del estado no contaban como irrecuperables; en
  `precede`, las anclas sin tiempo y los precursores fuera de `common` no se contaban y la ventana del último evento
  no salía activa (la tapaba un `max`); el cerrojo de `loki_index`; aperturas simultáneas de un índice nuevo; dos
  `reindex` a la vez se apartaban el índice uno a otro; `tail` seguía escribiendo en la copia tras un `reindex` y
  perdía eventos del mismo instante al paginar; el recuento de `stale_dims_events` recorría la fuente aunque no
  hubiera ninguno (`informe` a 1M).

Sin verificar o pendiente:
- Con `--join` sigue sin lift: falta una frecuencia base por clave.
- Glob por encima de 64 MB: una edición en medio seguida de un append no se ve (la huella no cubre el medio).
- Lo ya indexado de una fuente irrecuperable conserva las dims u opciones de antes (se cuenta, no se arregla).
- `Pattern` hace vuelta atrás sin memo (venía de F4).
- `informe` sigue contando cada episodio con una consulta (1,72 s a 1M): falta el barrido ordenado.
- `token_budget.truncated` sigue siendo `returned >= max` en las listas con `limit`.
- `top attr:k` con filtros sigue uniendo `event_dims` con `events` por rid (venía de F3).
- Un resultado ya agregado en rollups cuyo uso llega en un `sync` posterior sigue en `unknown` (venía de F4).
- Esquemas MCP en ~24,6 KB hasta F5.

**F4 hecha → 0.6.0 (esquema v7, se migra al abrir).** Binario 2 361 120 → 2 427 712 B (+3 %), sin dependencias
nuevas. Detalle en la decisión 30 de `DECISIONS.md`.
- **P-REL-2** `relations` en `.chrono/config.json` (versión 2, `config check` las valida con la ruta de la clave y el
  error del patrón): la clave sale de un campo, de `link:<rel>` o de un patrón sobre la ruta o el título
  (`chrono_core::Pattern`, matcher propio sin `regex`) y se guarda como link al ingerir. Una clave `K` es la unión de
  links `rel = K` y dims `K`, comparada por valor. `events|top|timeline --join K=v|K=pre*|K=@id|K`,
  `correlate <id> --via K`, `precede --join K` y `gaps --from A --to B --via K [--by U]`, también por MCP (tool
  `gaps`, `via` en `correlate`, `join` en los filtros comunes).
- **Links internados**: `link_targets` + `event_links` y vista `links`. Sesiones de Claude (copia congelada, 1 930
  ficheros, 265 240 eventos): índice 166,7 → **144,6 MB** (`links` 54 → 29,5 MB tras migrar un v6), ingesta 28,3 →
  26,8 s y 354 → 324 MB de RSS. 0.5.0 y 0.6.0 dan lo mismo en `top entity|level|kind`, `top entity --level ERROR`,
  `top attr:sessionId`, `patterns --level ERROR` y `sources`.
- **P-MSR-3** `precede`: `episodes_total` y «antes de x/N» salen de TODOS los episodios (`limit` solo acota los
  devueltos) y cada precursor común trae `expected` y `lift` frente a la frecuencia base de `rollups`; se ordenan por
  exceso sobre lo esperado (un `heartbeat` cada 5 min deja de salir primero; test). En las sesiones, antes de un fallo
  de Bash (60 s, `before_levels=*`): 0.5.0 decía 50 episodios y «43/50»; ahora **1 760** episodios, Bash 1 594/1 760
  (lift 5,2) en 0,52 s; con `--join sessionId`, 2 012 episodios (uno por sesión) en 2,05 s.
- **`entity_via=tool_use`** sustituye a `entity_join` en `claude-sessions`: el resultado toma la tool de su uso por el
  link en el índice, con una segunda pasada al acabar la ingesta para el resultado leído antes que su uso (sobre lo que
  aún no cuentan los rollups). Fallos por tool idénticos a 0.5.0 (Bash 3 784, Edit 186, Read 83…; 0 en `unknown`), y
  ahora también con el `sync` cortado entre uso y resultado, en ficheros distintos o en orden inverso (tests).
- **Sesiones ↔ git** (índice con este repo y las sesiones; relación `ruta`: links `file` de la sesión sin el prefijo del
  repo o del worktree ↔ ficheros del commit): 10 sesiones editaron (Edit/Write) ficheros de chrono, 9 con algún fichero
  que entra en un commit y 1 sin ninguno; 7 de 150 rutas editadas nunca se commitearon (`gaps`, 1,4 s). `correlate`
  sesión ↔ git, por tiempo (`--kind commit --delta 2h`) y por clave (`--via ruta`: 14 commits tocan el fichero de un
  Edit; el de 8 min después sale en las dos).
- **`~/archivo-pruebas-oriuno`** (Trycore; solo cifras): 6 324 ficheros en 1,0 s. 291 rutas con palabras de prueba,
  1 371 de resultado y 25 de decisión; de 179 claves específicas (≤ 20 ficheros), 4 aparecen en pruebas y resultados
  (13 de 291 pruebas emparejadas, 3 de ellas copias del mismo fichero), 2 en resultados y decisiones y **0 en los
  tres**. Con `relations` + `gaps` por patrón, 0 de 3 y 1 de 9 cubiertas. **No hay clave común en rutas ni nombres**:
  relacionar prueba → resultado → decisión necesita el contenido (F6) o un manifiesto.
- **Bench a 1M** (`bench/escala/f4-0.6.0.json` frente a `f3-0.5.0.json`, load 3-4): ingesta 28,5 → 23,1 s, `.db` igual
  (321 MB), consultas iguales o más rápidas (−8 a −33 %). `informe` 0,77 → **1,86 s**: sus 1 750 episodios por origen
  ya cuentan todos (antes «x/24» sobre los 24 devueltos). `events` +30 % de bytes es la ruta del fichero del bench, más
  larga en el scratchpad. Esquemas MCP 20 147 → 23 078 B (24 tools; F5 los consolida).
- Delegación en Trycore: 2 trabajadores (tests de ingesta de `relations`/`entity_via` y de `gaps`/`--via`/`--join`,
  0,33 $ + 0,79 $). Los nodos no tienen toolchain de Rust: escribieron los tests sin compilar; aquí compilaron y
  destaparon 3 fallos reales (`field: id` con prefijo de fichero, `K=pre*` con `LIKE`: `_` comodín y sin distinguir
  mayúsculas).

- Auditorías (`auditor-senales`, `auditor-silencios`) sobre el diff; corregido: `tail` escribía una relación nueva
  en `source_dims` y el `sync` siguiente ya no reindexaba (ahora `tail` se salta la fuente y avisa); el lift con
  rollups atrasados salía inflado y con `--join` no correspondía a la población (ahora `null` en los dos casos);
  `gaps` aplicaba `--since` al lado `to` y una fuente mal escrita daba «todo es hueco» (ahora error); el resultado leído
  antes que su uso; tests que pasaban con el código viejo (orden del lift, prefijo `_`, «sin cambios no reindexa») y
  un test MCP que ejecuta `gaps`, `correlate via`, `precede` con tope y `events join`.

Sin verificar o pendiente:
- La frecuencia base es temporal sobre todo el periodo: en fuentes a ráfagas (sesiones) casi todo sale con lift > 1.
  Con `--join` no hay lift (falta una base por clave). (La base, resuelta en 0.7.0 con ventanas activas; con `--join` sigue)
- `gaps` no mira el orden (un commit anterior a la edición también cubre), ni admite espacios en `grep=` de un lado,
  ni `--until`; un evento de `from` sin valor de `--by` no es unidad (sin aviso). (resuelto en 0.7.0)
- `precede --join` agrupa un ancla con varios valores por el menor y descarta sin contar las anclas sin clave. (resuelto en 0.7.0)
- `--join` con una clave que no existe devuelve vacío sin avisar (`gaps`, `--via` y `precede` sí dan error). (resuelto en 0.7.0)
- Activar `entity_via` a mano en una fuente ya indexada no reindexa; un resultado ya agregado en rollups cuyo uso llega
  en un `sync` posterior sigue en `unknown`. La migración v6→v7 de dos procesos a la vez: el segundo falla (con error).
  (Resueltos en 0.7.0 el reindexado por `entity_via` y la migración a la vez; lo de `unknown` sigue)
- `Pattern` hace vuelta atrás sin memo: un patrón como `.*.*.*x` sobre un título largo es polinómico.
- `informe` paga +1,1 s por contar todos los episodios: un solo barrido ordenado en vez de una consulta por episodio.
- `token_budget.truncated` es `returned >= max`: con exactamente `limit` unidades dice truncado sin estarlo (venía de antes).

**F3 hecha → 0.5.0 (esquema v6, reindex avisado).** Binario 2 311 152 → 2 361 120 B (+2 %), sin dependencias
nuevas. Detalle en la decisión 29 de `DECISIONS.md`.
- **P-ESC-3** `events.rid` entero es la clave de `touches`, `labels` y `links` (`WITHOUT ROWID`); `rollups` por
  `source_ref`. **P-ESC-2** `at` es columna virtual de `at_epoch` (0 diferencias de formato en tres índices reales);
  `jsonl` no copia a `attrs` tiempo, título, nivel, entidad, kind ni ids. **P-ESC-4** índice de actor y bandas
  SimHash parciales, sin `idx_events_kind`.
- **P-REL-1** `dims` en `source_options` (o en el preset; `config check` las valida): `event_dims(dim_id,
  event_rid)` + `dim_values(key, value)`; `file` es dim en las fuentes glob; `attr_keys` dice qué fuentes guardan
  cada clave en el JSON. Cambiar las dims reindexa esa fuente en el siguiente `sync`. `top attr:host` sin filtros:
  `SEARCH d USING PRIMARY KEY` + subconsulta correlacionada, sin `SCAN events` (test con `EXPLAIN QUERY PLAN`).
- **P-ESC-1** `dim_rollups` (fuente × plantilla × kind × nivel × nivel deducido × entidad × 10 min), en la misma
  pasada y con el mismo watermark que `rollups` (dentro de `lock_ingest`; el test de dos `sync` simultáneos sigue
  verde). Lo leen `top level|kind|entity`, `patterns`, `timeline`, `sources`, la cabecera `total` del MCP y por
  tanto `informe`. Exacto o nada: con `grep`/`attr`, rollups atrasados o un evento multi-touch en una consulta por
  entidad cae a `events`; una ventana sin alinear toma los extremos de `events`. Un test compara 330 consultas por
  los dos caminos. Borrar eventos invalida el watermark.
- **Bench a 1M** (`bench/escala/f3-0.5.0.json` frente a `f2-0.4.0.json`, con `host` como dim y la máquina cargada,
  load 7): `.db` 584 → **321 MB** (−45 %; `events` 266 → 122 MB); `informe` 10,7 s / 77 MB → **0,77 s / 32 MB**;
  `top attr:host` 1,04 → 0,03 s; `top entity` 0,54 → 0,02 s; `patterns --entity` 1,36 → 0,01 s; `timeline --by
  level` 0,56 → 0,06 s. Salidas byte a byte iguales a 0.4.0 en 18 consultas (incluido `informe`) salvo el
  `example_id` de `patterns`, que ahora es el último evento ingerido de la plantilla (lo que decía su doc), no el
  mayor id alfabético.
  La ingesta del bench marcó +14 % (24,9 → 28,5 s) con load 7; alternando los dos binarios con la misma carga,
  0.4.0 tardó 38,7 y 33,4 s y 0.5.0 24,4 y 23,3 s (la primera de 0.4.0 coincidió con una compilación): no empeora.
- **Sesiones de Claude** (copia congelada de `~/.claude/projects`, 1 927 ficheros, 264 642 eventos): índice
  395 → **166 MB** (1 492 → 629 B/evento; `events` 159 → 26 MB, `rollups` 43 → 1 MB), ingesta 31 → 26 s y
  645 → 357 MB de RSS, `sync` sin cambios 0,05 s. Usos y fallos por tool, kinds, niveles,
  `top attr:cwd|gitBranch|sessionId`, `--attr cwd:…`, `timeline`, `patterns` y `sources`: idénticos a 0.4.0.
- Auditorías (`auditor-senales`, `auditor-silencios`) sobre el diff; corregido lo que encontraron: valores a 0 en
  `top attr` tras borrar una fuente, `jsonl` perdía el tiempo ilegible, el mensaje cuando el título sale de otro
  campo y las claves pedidas en `attrs`, y tests que podían pasar sin probar el cubo o la relectura.

Sin verificar o pendiente:
- Los tiempos son de una máquina con load 4-7 (otras sesiones): repetir `make bench` en frío para cifras finas.
  (resuelto en 0.7.0: bench en frío en trycorepc)
- `top attr:k` con filtros (p. ej. `--level ERROR`) une `event_dims` con `events` por rid: 0,5 s a 1M, sin cubo.
- `links` sigue en texto (54 MB de 166 en las sesiones): candidato a internar targets en F4 junto a `relations`.
  (resuelto en 0.6.0)
- De las auditorías, sin tocar (venían de antes o son raros): `patterns` con solo fuente/`grep` lee `rollups`, que
  no comprueban su watermark ni cuentan eventos sin tiempo, y puede no coincidir con `patterns` con nivel; `tail`
  ingiere sin `finalize` y sin mirar si cambiaron las dims; cambiar las dims de una fuente que ya no se puede
  releer entera (log rotado, caché `cmd` truncada) pierde lo que no se relea, con solo un aviso; una clave con
  punto (`message.model`) como attr se lee como ruta anidada (como dim funciona). (resueltos los cuatro en 0.7.0)
- `correlate` entre sesiones y git sigue sin probar (venía de F2). (resuelto en 0.6.0)
- La cuenta de Trycore no tuvo cupo (tope de 5 h a 0); todo se hizo en esta sesión.

**F2 hecha → 0.4.0.** Binario 2 244 368 → 2 311 152 B (+3 %), sin dependencias nuevas.
- **Config:** `.chrono/config.json` con `version` (1) e `intermediario` dentro (el `intermediario.json`
  antiguo se sigue leyendo). `chrono config check` da cada problema con la ruta de la clave
  (`source_options["app.log"].tim_field … (¿time_field?)`) y valida las opciones de cada adaptador
  (`Source::options()`/`validate()`). Antes, un tipo mal puesto tumbaba el fichero entero a los defaults en
  silencio; ahora `load` lo avisa por stderr.
- **Glob:** `chrono add '~/.claude/projects/**/*.jsonl' --as jsonl --name claude --preset claude-sessions`
  es una fila en `sources`; el estado por fichero va en `meta` (415 KB para 1 926 ficheros) y en
  `sources.watermark` un resumen. 2,7 GB → 30 s y 644 MB de RSS; índice de 376 MB (1 420 B/evento por las
  attrs `cwd`/`sessionId` repetidas: candidato a `event_dims` en F3). `sync` sin cambios: 0,38 s.
- **jsonl:** `explode_where` con `a|b`, `where` (filtro de la línea), `level_map`, `error_message_field`,
  `attrs` (lista blanca), `entity_join` (el resultado hereda la tool de su uso, dentro del fichero),
  `uid_field` (id global entre ficheros) y presets. Sin `message_field`, el título de un elemento expandido
  es su tipo o sus claves, nunca sus valores.
- **dir:** `chrono add <carpeta> --as dir`. `rust/target` (1,2 GB, 22 592 ficheros) en 3,7 s y 87 MB de
  RSS, leyendo como mucho 512 B por fichero; un `touch` → `sync` de 0,57 s con 1 evento.
- **Bench a 1M** (`bench/escala/f2-0.4.0.json` frente a `f1-0.3.0.json`): el `.db` sale idéntico byte a
  byte y las respuestas MCP, iguales; los tiempos quedan dentro del ruido (el mismo binario 0.3.0 midió
  ±15-20 % entre dos pasadas).

Validación con las sesiones reales, por MCP y sin leer transcripciones: 132 208 usos de tool (Bash 94 682,
Read 9 206, Edit 8 537, Write 5 660); 4 527 fallos (Bash 3 780, Edit 186, Read 82, Write 76); por tipo,
«Exit code N» 2 286, el aislamiento de worktree de los agentes 760, «File has not been read yet» 98 y
denegaciones (usuario 72, modo don't-ask 69, clasificador 67). Antes de un fallo de Bash, en 60 s
(`precede … before_levels=*`, 1 756 episodios, 1,2 s): otro Bash en 1 590, Write 273, Read 222, Edit 190.

Lo que salió al hacerlo:
- Las líneas `api-request-blob` repiten la petición entera a la API sin `timestamp` (1 515 usos fantasma):
  el preset filtra con `where=type=assistant|user`.
- Una sesión reanudada o bifurcada copia el historial en otro fichero: 17 360 usos aparecían dos veces
  (~15 % de inflado). `uid_field=id|tool_use_id` los cuenta una vez.
- `add_event` añadía touches y links aunque el evento ya existiera: un duplicado con otra entidad lo contaba
  dos veces. Ahora un evento ignorado no añade nada, y `add`/`sync` cuentan los guardados, no los leídos.
- `precede` solo miraba precursores con nivel; con `before_levels=*` vale para fuentes sin nivel.
- `stats` por MCP tomaba su `attr` numérico también como filtro `clave:valor` y no devolvía nada.

Sin verificar o pendiente:
- La carpeta real de 2,2 GB no se identificó (`~/archivo-pruebas-oriuno` pesa 279 MB y es de Trycore: no
  se usó); `dir` se midió con `rust/target` y `~/.claude/projects`.
- `correlate` entre sesiones y git (fila del preset) no se probó. (resuelto en 0.6.0)
- `precede` sigue topando en 50 episodios por defecto (P-MSR-3, F4); con `limit` alto y `depth=counts`
  o `summary` las cifras salen completas. (resuelto en 0.6.0)
- La cabecera de `stats` por MCP dice `total=0` aunque haya grupos. (resuelto en 0.7.0)
- `entity_join` vive en el cursor: un resultado cuyo uso quedó antes del watermark (un `sync` a mitad de
  un turno) se queda en `unknown`. En la ingesta completa hubo 0. (resuelto en 0.6.0 con `entity_via`)
- El cambio se detecta por tamaño + mtime: un fichero reescrito con el mismo tamaño y mtime no se relee.
  (resuelto en 0.7.0: ctime y huella)
- Los títulos de error son el único contenido de las sesiones que entra en el índice: en los 1 075
  distintos no hubo credenciales (un falso positivo `sk-…` dentro de una ruta). No hizo falta fixture real.
- La cuenta de Trycore no tuvo cupo (0 hasta las 12:40); todo se hizo en esta sesión.

**F0 y F1 hechas → 0.3.0.** Medido con `make bench` a 1M frente a 0.2.7:
- RSS de las consultas −62 a −95 % (`patterns`, `precede`, `events`: ~450 → ~25 MB). `informe` +31 % de
  tiempo (8,8 → 11,6 s) hasta F3, que lo saca de recorrer `events`.
- Plantillas 68 → 15. Respuestas MCP: `precede` 20 KB → 1,9 KB, `events` 29 KB → 7,4 KB.
- Ingesta +8 % en mediciones alternas con el binario previo (+15 % en la pasada del bench, con la máquina cargada) por la redacción de secretos; era +23 % antes de la comprobación previa sin reservas.

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

Cierre: migración con aviso de reindex (como en las versiones 3 y 4 del esquema), versión 0.5.0, eval y bench.

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
