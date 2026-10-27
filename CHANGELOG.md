# Cambios

## 0.7.1

Arreglos sobre 0.7.0 y la fuente de delega. El índice no cambia de esquema (v7): no hace falta reindexar (una
fuente `delega` indexada antes se reindexa sola una vez, ver abajo).

**Nuevo**
- Fuente `delega` (`chrono add <state_dir> --as delega`): carpetas de trabajo y `audit.jsonl` de delega. `chrono
  delega` (tool MCP `delega`) da gasto por día, perfil, nodo, dueño y modelo cuadrado contra el audit, fallos de
  infraestructura, latencia pregunta → respuesta y turnos vacíos del orquestador. `chrono tail --source delega,nod
  --level WARN,ERROR` sirve de monitor único. Ver `docs/DELEGA.md`.
- `precede --join`: el lift ya no sale `null`; se calcula sobre las ventanas de cada valor de la clave. Cada
  precursor dice de dónde sale su base en `lift_base`: `active_windows` (sin `--join`), `key_windows` (con
  `--join`, con `lift_base_units`, `lift_base_events` y `lift_base_templated`) o `none` (rollups atrasados o
  demasiados eventos que leer; un hint dice cuál).
- `correlate` por tiempo filtra por `kind` también por MCP (antes se ignoraba).

**Cambia (contrato)**
- MCP: el error de una tool (argumento mal, id o clave de `join` que no existe) llega como resultado de
  `tools/call` con `isError: true` y el motivo en `content[0].text`, no como error JSON-RPC `-32000`. Solo los
  errores de protocolo (sin `name`, método desconocido) siguen siendo errores JSON-RPC. Un cliente que miraba el
  código `-32000` tiene que mirar `isError`.
- MCP: todas las tools comprueban sus argumentos contra su `inputSchema`, con las reglas del CLI. Un argumento que
  la tool no tiene (`lvl` por `level`), de otro tipo (`level: 5`), un entero que no es > 0, un valor fuera de su
  lista (`format`, `order`, `tipo`…), un `since`/`until` que no es una fecha o una ventana vacía son error con el
  argumento y lo que espera; antes se descartaban en silencio (un `since` mal escrito consultaba toda la
  historia). Sigue valiendo un número o booleano en una cadena (`"10"`, `"true"`) y `null` como «no lo paso».
- `truncated` es exacto, en el CLI y en la cabecera MCP: `true` solo si hay más de lo devuelto (un total conocido
  mayor, o una fila de más pedida al consultar). Antes una lista con exactamente `limit` elementos salía truncada.
  `owners` y `branches` van enteras.
- CLI: un argumento posicional de más (`tail x`, `search dos palabras` sin comillas) es error en vez de ignorarse;
  `tail`, `add` y `correlate --via` avisan de las flags que no les aplican.
- Fechas: un solo parser para todas las fuentes. Una fecha u hora imposible (31 de febrero, `25:00`, zona de más
  de 23 h) deja el evento sin tiempo en todas las fuentes; antes alguna la convertía en otra fecha (el 30 de
  febrero como 1 o 2 de marzo).

**Escala** (medianas de 5 en Linux a 10M de eventos; mismas cifras, solo cambia el orden de empates)
- `informe` 27,5 → 6,9 s: los precursores de los episodios salen de un barrido (una consulta por tramo de la unión
  de ventanas, no una por episodio), las anclas y los precursores con nivel usan `idx_events_level` y los
  servicios por nivel salen del cubo en una pasada.
- `top attr:host --level ERROR` 5,93 → 0,13 s y con `--entity svc-3` 9,92 → 0,08 s: tabla nueva `attr_rollups`
  (claves de hasta 10 000 valores), creada en el build completo de rollups, al día en cada `sync` y corregida
  también por `entity_via` tardío.
- `precede --level FATAL` 2,63 → 0,47 s.
- Coste: ingesta +5 % (4:25 → 4:40) e índice +0,8 %.
- `precede`: dos precursores que solo difieren en nivel o entidad salen en un orden fijo (antes, el del HashMap).

**Arreglado**
- Glob: la huella de lo ya leído es completa a cualquier tamaño; una edición en medio de un fichero de más de 64 MB
  entre los bloques de la muestra ya se relee. Los estados de 0.7.0 pasan solos a la huella nueva.
- `entity_via`: un resultado cuyo uso llega en un `sync` posterior se corrige también en los rollups (`top entity` y
  `patterns` ya no lo siguen contando en `unknown`).
- `redact`: una credencial `Basic` corta tras `Authorization` (`dTpw` es `u:p`) quedaba a la vista.
- delega: ids de audit únicos por fuente (dos `audit.jsonl` ya no se pisan; la fuente de antes se reindexa una vez),
  un `utc_offset` mal escrito es error, una fecha imposible o un epoch fuera de 0000–9999 no cuenta, y entradas
  raras (nombres con caracteres de varios bytes) ya no hacen panic. Usa el parser de fechas común.

## 0.7.0

Cierra los pendientes de 0.4.0–0.6.0. El índice no cambia de esquema (v7): no hace falta reindexar.

**Nuevo**
- `chrono reindex`: rehace el índice con las mismas fuentes (git, logs, globs, comandos). Es lo que propone el
  error de un índice de otro esquema. Si se corta, se vuelve a lanzar y reanuda desde la copia.
- `events --order desc` (MCP `order=desc`): lo más reciente primero.
- `top <dim>` sin `attr:` para una dim declarada (`top cwd`); una dim desconocida lista las que hay.
- `gaps --any-order` y `--until`; `grep=` con espacios en `--from`/`--to`.
- `-h`/`--help` en cualquier subcomando, sugerencia ante un comando mal escrito y aviso de flags desconocidas o
  que no aplican.
- `hints` en el resultado (y `⚠` en la cabecera MCP) cuando un vacío tiene explicación: attr o nivel que no
  existe, `stats` sin valores numéricos, `hotspots` sin git, eventos sin tiempo…

**Cambia**
- `gaps` solo da por cubierta una unidad con un `to` en o después de su primer `from` (antes, en cualquier
  orden; `--any-order` lo recupera). Cuenta aparte `from_without_key` y `units_without_time`.
- `precede`: el lift se calcula sobre las ventanas con actividad de la fuente (`active_episodes`), no sobre todo
  el periodo; en fuentes a ráfagas, como las sesiones de Claude, baja mucho (4,74 → 0,96 en un caso medido). Un
  ancla con varios valores de `--join` cuenta en cada uno; `anchors_without_key`, `anchors_without_time` y
  `precursors_total` dicen lo que queda fuera.
- Cambiar las opciones de una fuente (`entity_via`, `explode`, `attrs`, `links`…) la reindexa en el siguiente
  `sync`, como las dims. Si parte de la fuente ya no se puede releer, no se reindexa: avisa y `sources` cuenta los
  eventos que conservan las de antes (`stale_dims_events`).
- `--join`, `--via` y `--by` con una clave que no existe dan error en vez de vacío.

**Arreglado**
- Un fichero de una glob reescrito con el mismo tamaño y fecha, o editado en medio, se relee.
- `sync` y `tail` siguen con las demás fuentes si una falla; `sources` enseña el `sync_error`.
- `init` ya no borra las fuentes añadidas con `add` ni un índice que no puede abrir (uno dañado queda como
  `.corrupt.bak`); funciona en un repo sin commits y explica una rama huérfana.
- `tail` deja el índice como un `sync` (plantillas, rollups, búsqueda) y no pierde eventos del mismo instante.
- Dos procesos que abren o migran el mismo índice a la vez ya no fallan; una consulta sobre una ruta sin índice no
  crea un `.db` vacío; el cerrojo del hook de Loki no se queda bloqueado si el proceso muere.
- `search --limit`, `informe --level`, el `total` de `stats` por MCP, attrs con punto (`message.model`) y
  `patterns` con solo fuente o texto (ahora exacto). `--compact` usa el alias de la fuente.
