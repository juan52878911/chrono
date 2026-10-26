# delega: chrono como memoria operativa de la orquestación

[delega](https://github.com/juan52878911/dotfiles-ia/tree/main/delega) lanza trabajadores de Claude Code en otras
cuentas y máquinas y deja su rastro en un directorio de estado (`~/.local/state/delega/` por defecto, el `state_dir`
de su config). chrono lo indexa como una fuente más (`delega`) y saca de ahí, y de los eventos de Nod si están en el
mismo índice, las métricas de las retrospectivas con cifras exactas, sin agentes leyendo transcripciones.

Solo en el port Rust (`rust/`), como todo lo nuevo desde la decisión 14.

```sh
chrono add ~/.local/state/delega --as delega   # carpetas de trabajo + audit.jsonl
chrono add ~/.local/state/nod/events.jsonl --as nod        # sesiones del orquestador y sobres de delega
chrono delega                                              # el informe (texto); --format json
chrono delega --since 2026-10-20 --until 2026-10-27
chrono tail --source delega,nod --level WARN,ERROR         # monitor único para el orquestador
chrono sync                                                # incremental
```

`--as delega` hace falta solo si el directorio vive dentro de un repo git (entonces `detect` de git también lo
reclama); fuera de un repo, `detect` lo reconoce solo (85: tiene `audit.jsonl` o una carpeta con `meta` y
`brief.md`). También por MCP: tool `delega` (mismos argumentos que el CLI: `since`, `until`, `orchestrators`,
`stale_hours`, `limit`, `format`).

## 1. La fuente `delega` (`chrono-source-delega`)

Cada carpeta cuyo nombre empieza por un dígito es un trabajo; su nombre (`20261026-122251-88099-chrono`) es la
**entidad** de todos sus eventos (tipo `job`). Se leen los nombres nuevos y los legados en español, igual que
`delega.util` (`summary.json`/`resumen.json`, `result.json`/`salida.json`, `state.json`/`estado.json`,
`progress.log`/`progreso.log`, `notices.jsonl`/`avisos.jsonl`; en `meta`, `HOST`→`NODE`, `DUENO`→`OWNER`,
`RAMA`→`BRANCH`, `FIN`→`END`, y la última línea gana).

| Evento (`kind`) | De dónde | Hora | Nivel |
| --- | --- | --- | --- |
| `delega.job.start` | `meta` | `START` del progreso → `start` del resumen → `start` del estado → hora del id | INFO |
| `delega.job.end` | resumen, resultado, estado, `rc`, `meta` | `end` del resumen → `END` de `meta` → `last_event` del estado → última línea del progreso | ERROR si no dejó salida o `rc` de arranque; WARN si `rc ≠ 0` o tope; INFO |
| `delega.job.result` | `result` de `result.json` (o la última línea de `results.jsonl`) | la del cierre | INFO |
| `delega.notice` | `notices.jsonl` (`attrs.notice` = `milestone`, `question`, `blocker`) | `ts` | question WARN, blocker ERROR, milestone INFO |
| `delega.alert` | líneas `ALERT:*` del progreso (`attrs.alert` = `stall`, `loop`, `errors`, `undelivered`, `no_answer`, `cap`, `dead`, `cap_cut`) | la de la línea | WARN; `dead`/`cap_cut` ERROR |
| `delega.message` | líneas `MESSAGE` del progreso (el orquestador escribió al trabajador) | la de la línea | INFO |
| `delega.audit.<event>` | `audit.jsonl` (`launch`, `end`, `refused`, `integrate`, `scan`, `golden_*`, `sandbox_*`…) | `ts` | `refused` ERROR; `end` con `rc ≠ 0` e `integrate` con `conflicts`/`verify_failed` WARN |

Atributos de inicio y cierre: `job`, `profile`, `node`, `owner`, `repo`, `branch`, `project`, `account`, `base`,
`model`; en el cierre además `cost_usd` y `cost_from` (`summary`, `result` o `state`: **el mismo orden que
`delega spend`**), `duration_min`, `duration_ms`, `messages` (turnos), `tools`, `alerts`, `rc`, `cap` (el texto del
tope, si lo hubo), `output` (`yes`/`no`) y `session_id`. Los de audit van tal cual (escalares; las listas no). audit
guarda el id corto del trabajo (`20261026-122251-88099`): se resuelve a la carpeta (`job`) y el corto queda en
`job_id`.

- **Sin texto libre.** `brief.md` no se abre nunca. Del resultado, de los avisos y del `reason` de audit entra solo
  la primera línea, sin marcas de Markdown, recortada a 160 caracteres y con los términos del `forbidden.txt` del
  trabajo (y del fichero de la opción `forbidden_terms`) sustituidos por `[redacted]`, sin distinguir mayúsculas,
  los largos primero, como `delega scan`. El texto de las alertas (cita la última acción, a menudo un comando) y de
  los mensajes no entra. Los eventos `delega` no pasan por el clasificador de fixes.
- **Hora.** delega escribe hora local sin zona. El offset sale de la opción `utc_offset` (`-05:00`) o se deduce de
  los trabajos que guardan las dos formas (resumen en hora local frente a estado en epoch; el más votado,
  redondeado a 15 min). Queda en el watermark: cambiar `utc_offset` después obliga a reindexar (la fuente diverge).
- **Incremental.** Watermark `mt:<ns>|a:<off>|p:<hash>|tz:<s>`: se releen solo las carpetas con algún fichero más
  nuevo que `mt` (los eventos ya guardados se descartan por id; un trabajo en curso se relee entero cada vez, son
  pocos KB) y `audit.jsonl` desde `a`, solo líneas completas. Si audit encoge o cambia su prefijo, la fuente
  diverge y se reindexa sola. Un trabajo sin cierre (sin resumen, resultado, `rc` ni `END`) solo tiene `job.start`
  hasta que cierra.
- **Manifiesto:** `delega_jobs`, `delega_jobs_scanned`, `delega_audit_lines`, `delega_audit_invalid`,
  `utc_offset_s`, `utc_offset_from` (`option`, `observed`, `watermark` o `default-utc`).

```json
{"source_options": {"delega": {"adapter": "delega", "utc_offset": "-05:00",
                                          "forbidden_terms": "~/.config/delega/forbidden.txt"}}}
```

### Sobres de delega en Nod

La fuente `nod` acepta además, solo de `source.host == "delega"`, los tipos `job.launch`, `job.notice`,
`job.alert`, `job.result`, `job.end`, `job.say`, `job.message`, `decision.request` y `decision.answer`
(`kind = nod.<tipo>`). El trabajo va en `job` (raíz del evento) o `data.job`, es la entidad y, si falta `session`,
también la sesión. Nivel: `data.level` si viene; si no, `job.alert` y `decision.request` WARN, `job.end` con
`rc ≠ 0` WARN. El título es `<tipo> <trabajo> [kind|alert|notice]` y, si delega lo pone a propósito,
`: <data.summary>` (primera línea, 160 caracteres); `summary`, `question`, `message`, `brief` y `result` no van a
`attrs`. El coste y los fallos se siguen midiendo desde las carpetas: Nod sirve para el monitor y para la latencia
de `decision.request`.

## 2. `chrono delega`

| Métrica | Cálculo | Límites |
| --- | --- | --- |
| Gasto | Σ `cost_usd` de `job.end` de los trabajos que **empiezan** en la ventana; por día (el del id del trabajo, como `delega spend`), perfil, nodo, dueño y modelo. `?` = sin dato en `meta`. | Un trabajo aún vivo no tiene `job.end`: su coste entra al cerrar (`delega spend` ya suma lo que lleve en el estado). |
| Cuadre con audit | Por cada `audit.end` con trabajo: coincide si su `cost_usd` difiere ≤ 0,005 $ del de la carpeta. Lista los que difieren, los `end` de audit sin carpeta y los trabajos cerrados después del primer evento de audit que no tienen `end` en audit. | audit solo existe desde que delega lo escribe; antes, nada que cuadrar. |
| Fallos de infraestructura | Trabajo `refused` en audit, con `rc` de arranque de `delega run` (2, 3, 4, 5, 8, 9), cerrado sin salida (`output: no`), o sin cierre y sin actividad en `stale_hours` (6) respecto al último evento de delega del índice (`never_ended`). Tasa = fallidos / (trabajos − en curso). | Un `rc` del trabajador (1) no es de infraestructura. |
| Latencia aviso → respuesta | Por trabajo: cada aviso `question` (y aparte `blocker`) → el siguiente `MESSAGE` del progreso. Mediana, p90, máximo, cuántas pasan de 30 min y las más lentas. Sin `MESSAGE` posterior: sin respuesta. | Mide cuándo el orquestador escribió al trabajador, no si respondió a esa pregunta. |
| Latencia `decision.request` | Eventos de Nod de delega: siguiente `decision.answer`/`job.say`/`job.message` con el mismo `data.id` (o, sin id, del mismo trabajo). | Vacía hasta que delega emita esos sobres. |
| Turnos del orquestador | Sesiones de Nod cuyo `cwd` acaba en la carpeta de un dueño de trabajo (`OWNER` es el nombre de la carpeta del orquestador), las que pasa `--orchestrators` (ids o trozos del `cwd`) y, con `include_command` en la fuente de Nod, las que llaman a `delega`, `delegar.sh` o `ws exec claude`. Turno vacío = `turn.end` del agente principal sin ningún `tool.call` del principal en ese turno. Se desglosa por el origen del `prompt.submit` que lo abrió; «sin el humano» = `task-notification`, `sdk`, `peer`, `scheduled-trigger`. | Un turno sin herramientas pero con una decisión escrita al humano cuenta como vacío. |
| Shas por rama | `audit.integrate`: shas distintos medidos por rama, cuántos verificados y cuántos fallaron. Un PR sale de una rama. | Vacío hasta que se use `delega integrate`. |

## 3. Rendimiento medido

Sobre un estado real de unas 300 carpetas de trabajo y 50 líneas de audit: `chrono add` produce unos 2 600 eventos en
0,6 s y un `sync` sin cambios tarda 0,02 s. Con un `events.jsonl` de Nod de unos 170 000 eventos en el mismo índice,
`chrono delega` responde en 0,12 s. El gasto que calcula cuadra al céntimo con `delega spend all`.

## 4. Límites conocidos

- `delega spend` busca el coste también en la carpeta de `CHANNEL` si es otra ruta local; la fuente lee solo la
  carpeta del trabajo. Con los datos actuales no cambia ninguna cifra (el total cuadra al céntimo).
- La hora de los trabajos legados sin resumen ni estado sale del progreso o del id, con el offset de la fuente: si
  el nodo estaba en otra zona, esa hora se desplaza.
- La identificación del orquestador por `cwd` depende de que `OWNER` sea el nombre de su carpeta (el valor por
  defecto de delega); con `DELEGA_OWNER` distinto, pasar `--orchestrators`.
- «Turno vacío» no distingue un turno que solo informa al humano de uno que no hace nada: el desglose por origen es
  lo que separa los despertares automáticos.
