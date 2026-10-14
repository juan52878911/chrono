# Nod: el proceso; chrono: el resultado

[Nod](https://github.com/juan52878911/nod) observa las sesiones con agentes de código (hoy Claude Code y OpenCode) y emite
eventos **Nod v1** (`docs/nod-v1.md` y `schema/nod-v1.schema.json` en ese repo). chrono los ingiere como una
fuente más y los cruza con el historial de git en la consulta `session_patterns`.

Implementado en el **port Rust** (`rust/`), como todo el trabajo nuevo desde la decisión 14 de
[`DECISIONS.md`](DECISIONS.md). El binario Go no lo tiene.

```sh
chrono init                                       # el repo
chrono add ~/.local/state/nod/events.jsonl --as nod
chrono session_patterns                           # por tarea y por sesión
chrono session_patterns --task ABC-123            # una tarea
chrono trace <sha>                                # la sesión de Nod que creó ese commit
chrono sync                                       # incremental: git + Nod
```

`--as nod` (el `--as` genérico de `chrono add`) hace falta solo si el fichero aún está vacío: con sobres dentro,
`detect` lo reconoce (90, por encima del 60 de `jsonl`). También vale `"adapter": "nod"` en las `source_options`
de esa ruta en `.chrono/config.json`.

## 1. La fuente `nod` (`chrono-source-nod`)

Una línea del fichero = un sobre `{"nod":1,"source":{…},"events":[…]}`. Cada evento de tipo conocido se convierte
en un `Event` de chrono:

| Nod v1 | chrono |
| --- | --- |
| (`source.instance`, `source.run`, `seq`) | `id = nod:<instance>:<run>:<seq>`. Es la identidad del contrato (§1.3): un evento entregado dos veces tiene el mismo id y el store lo ignora (`INSERT OR IGNORE`). |
| `type` | `kind = nod.<type>` (el prefijo evita chocar con el `kind` de otras fuentes) |
| `ts` (ms) | `at_epoch` tal cual (el índice ya trabaja en ms) y `at` con los ms (`…T21:32:58.028Z`) |
| `source.profile` / `source.instance` | `actor.name` / `actor.key` |
| `source.host` | `attrs.host` (`claude`, `opencode`) |
| `task` | `attrs.task`; en `vcs.commit` y `task.set`, además `link{rel:"ticket"}` (así `chrono tickets <id>` los lista) |
| `vcs.commit.data.sha` | `link{rel:"commit", target: sha}` (completo o abreviado, como lo dio git) |
| `tool.call` | touch `tool/<Tool>` (tipo `tool`) |
| `tool.call` de Write/Edit/MultiEdit/NotebookEdit (Claude Code) o `write`/`edit` (OpenCode, ruta en `filePath`) con privacidad `completo` | touch `edit:<ruta>` (tipo `edited-file`) con la ruta de la entrada |
| `tool.result.links` (lo que produjo la tool; solo con privacidad `completo`) | `link{rel:"url", target: url}` para PRs, issues y artifacts; `link{rel:"file", target: ruta}` para documentos y ficheros escritos |
| `remote_session` (`session.start`, `usage`) | `attrs.remote_session` |
| `data` | `attrs`, aplanado con `.` (`usage.input`, `repo.branch`, `context.percent`…); si una clave de `data` choca con un campo común va como `data.<clave>` (p. ej. `task.set` → `data.task`) |

- **Sin texto libre en el índice.** `text`, `answer`, `input`, `reason`, `description` y `title` no se copian; el
  `title` del evento es estructurado (`permission.decision Bash deny by user`, `vcs.commit a18ffb6 (main)`). Así
  un prompt que dice "arregla X" no cuenta como fix en `bugs`, y lo que la privacidad deja salir no se replica.
  Por lo mismo, los eventos `nod` no pasan por el clasificador de reglas.
- **Severidad derivada** (para `timeline --by level`): `ERROR` = tool falló; `WARN` = permiso negado, tool negada
  por el humano o turno abortado; `INFO` el resto.
- **Tipos desconocidos** (o sin `seq`/`ts`/`type`/`session`): se saltan. Sobres que no son JSON, de otra versión o
  sin `source` válido: también. Ambos se cuentan en el manifiesto de la fuente (`nod_skipped_events`,
  `nod_invalid_lines`).
- **Incremental.** Watermark `off:<bytes>|p:<hash del prefijo>`, como `jsonl`. Solo se consumen líneas completas:
  una línea a medio escribir (Nod está añadiendo) se deja para el siguiente `sync`. Si el fichero se trunca o se
  reescribe, la fuente diverge y se reindexa sola.
- **`trace <id>`** recorre esos links: desde un commit de git llega a su `vcs.commit` (y a la sesión y la tarea por
  `ticket`), y al revés. El sha de `vcs.commit` suele venir abreviado: `trace` resuelve los links `commit` por
  prefijo (≥ 4 hex) solo si casan con un único commit del índice. También une las sesiones que produjeron el mismo PR
  o documento (`url:`/`file:` compartido). `correlate` sigue siendo solo por tiempo.

## 2. `session_patterns`

JSON acotado con el envelope de siempre (`schema_version` del índice, hoy 5). Tiempos en ms, como el resto. `result`:

```json
{
  "summary": { "sessions": 2, "tasks": 2, "merge_rate": 0.5, "tasks_merged": 1, "tasks_merge_known": 2,
               "totals": { …las mismas métricas sobre todo… } },
  "basis":   { "events_scanned": 49, "hotspots": ["src/hot.rs"], "rework_window_days": 30,
               "horizon": "2026-03-02T00:00:00Z", "base_branch": "main" },
  "tasks_total": 2, "tasks": [ { "task": "ABC-1", "sessions": ["s-1"], "hosts": ["claude"], …métricas… } ],
  "sessions_total": 2, "sessions": [ { "session": "s-1", "remote_session": null, "tasks": ["ABC-1"], …métricas… } ],
  "notes": ["no PR data (forge): merge status comes only from local branches"]
}
```

Opciones: `--task`, `--session`, `--since` (sobre los eventos de Nod), `--hotspots N` (tamaño del top de hotspots,
25 por defecto), `--limit N` (tareas y sesiones devueltas, 25). Orden: costo, luego tokens, luego id. Las listas
internas llevan como mucho 5 elementos. También por MCP (tool `session_patterns`).

### Cómo se agrupa

- **Sesión** = `session`. Junta todos los procesos (`run`) que la reanudaron.
- **Tarea** = el `task` de cada evento (Nod lo pone en todos los eventos posteriores a fijarla). Un evento sin
  tarea va al grupo `task: null`. Solo crean grupo los eventos que aportan a alguna métrica (un `session.start` no
  lleva tarea y no abre un grupo vacío).
- **Ramas** de un grupo: `repo.branch` del `session.start` de sus sesiones y `branch` de sus `vcs.commit`.

### Métricas

| Campo | Cálculo | No se puede calcular cuando… |
| --- | --- | --- |
| `tokens` | Σ `turn.step.usage` (`input`, `output`, `cache_read`, `cache_write`; `subagents` = la parte con `agent`). `turn.end.usage` y `agent.end.usage` son agregados y no se suman (§3.1). | — (0 si no hay pasos) |
| `cost_usd` | Por (sesión, `instance`, `run`), el último `usage.cost_usd` (es acumulado del proceso, §3). Se suma como incrementos entre lecturas, que se atribuyen a la tarea de cada lectura; si una lectura baja, el proceso volvió a empezar y cuenta entera. | `null` sin ningún `usage.cost_usd` |
| `time.agent_ms` | Σ `turn.end.duration_ms` del principal (sin `agent`; los subagentes corren dentro). Si falta, `ts(turn.end) − ts(turn.start)` del mismo turno. | — |
| `time.waiting_human_ms` | Σ `permission.decision.wait_ms` con `by` = `user` o `remote`; `waiting_decisions` cuántas, `waiting_upper_bounds` cuántas con `exact: false` (cota superior). | — |
| `permissions` | `asked` = `permission.request`; `denied` = decisiones `deny` (cualquier `by`); `denied_by_user` = las de `user`/`remote`; `denied_by_tool` por tool. | — |
| `interruptions` | `turn.end` del principal con `outcome: "aborted"`. | — |
| `loops` | Rachas de ≥ 3 `tool.result` seguidos con `ok: false` de la misma tool, en la misma sesión y agente. Un `rejected: true` (lo negó el humano) no es fallo y corta la racha. Mismo criterio que el collector de Nod. | — |
| `commits.delivered` | Shas distintos de `vcs.commit`. | 0 si Nod no emitió `vcs.commit` (Nod anterior a la fase 2) |
| `commits.resolved` / `ids` / `unresolved` | Cada sha se resuelve por prefijo (≥ 4 hex) contra los commits del índice (la misma resolución que `trace`). No resuelve si no está (rama sin mergear: el índice solo conoce la historia de `HEAD`; commit reescrito por rebase o squash) o si el prefijo es ambiguo. | sin fuente git en el índice |
| `commits.touching_hotspots` / `hotspots_touched` | De los resueltos, cuántos tocan un fichero del top N de `hotspots` (la misma métrica, toda la historia, restringida a entidades `file`; `basis.hotspots` dice cuáles). | sin commits resueltos. En un repo con menos de N ficheros todo es hotspot: baja `--hotspots`. |
| `rework` | Ficheros de los commits resueltos que otro commit vuelve a cambiar en los 30 días siguientes (`(t, t+30d]`), sin contar los commits entregados por el propio grupo. `lines_delivered`/`lines_reworked` = Σ `added+deleted` de esos touches (columnas de `touches`, binarios = 0) (líneas cambiadas en esos ficheros, no un blame línea a línea). `open_windows` = commits cuya ventana aún no se cerró en `basis.horizon` (el último `sync` de git): su retrabajo puede crecer. | `null` sin commits resueltos |
| `incomplete` | Ficheros que la sesión escribió (Write/Edit/MultiEdit/NotebookEdit con `tool.result` `ok` y no negado) dentro de una raíz de repo conocida (la de la fuente git, o `repo.root`/`cwd` de la sesión) y que no aparecen en ningún commit entregado. `outside_repo` = escritos fuera de toda raíz (no cuentan). | `null` sin eventos con privacidad `completo` (sin `input` no hay rutas) |
| `merge` | `merged` si algún PR del forge que cita la tarea (título o labels, como `tickets`) está mergeado, o si alguna rama del grupo es una rama local mergeada en la base (`branches`, git en vivo). La base cuenta como mergeada solo cuando es la única rama del grupo (se trabajó directamente sobre ella): si la sesión arrancó en la base y luego trabajó en otra rama, decide esa rama. `not_merged` si hay rama local o PR y ninguno lo está. `unknown` con `reason` si no hay datos. | rama hasheada (`h-…`, privacidad `redactado`), rama que no existe en local y sin PR, o sin git |
| `produced` | URLs y rutas de `tool.result.links` del grupo (PRs, issues, artifacts, documentos), sin repetir. | `[]` sin privacidad `completo` (Nod quita `links`) o si ninguna tool produjo nada |
| `hosts` / `remote_session` | Agentes del grupo (`source.host`) y, en una sesión, la última sesión de Remote Control vista. | `remote_session: null` sin Remote Control o con privacidad `redactado` |
| `cost_per_commit` / `tokens_per_commit` | costo (o tokens) / `commits.delivered`. | `null` sin commits o sin costo |
| `summary.merge_rate` | tareas `merged` / tareas con estado conocido (sin contar `task: null`). | `null` si ninguna tarea tiene estado conocido |

`summary.totals` son las mismas métricas sobre todos los eventos; su `merge` mira todas las ramas juntas y es solo
orientativo.

## 3. Límites conocidos

- Las entidades `tool/*` y `edit:*` de Nod entran en `hotspots`/`top entity` generales del índice (como las de los
  logs): con tamaño 1 rara vez desplazan a ficheros, pero pueden aparecer. `session_patterns` usa siempre el top
  restringido a ficheros.
- Los PRs del forge no guardan la rama de origen: un PR se asocia a la tarea solo si la cita en título o labels.
- `branches` solo ve ramas locales: una rama borrada tras el merge (o solo remota) da `unknown`; el commit puede
  seguir apareciendo como resuelto (está en la historia de `HEAD`).
- Retrabajo por fichero, no por línea: cambiar otra función del mismo fichero también cuenta.
- La consulta carga en memoria los eventos de Nod de la ventana (`--since` la acota).
- OpenCode no avisa de lo que resuelven sus reglas sin preguntar: de sus sesiones no salen decisiones `by: "policy"`
  (salvo en `opencode run`). `denied` y `waiting_*` son comparables entre agentes solo en la parte del humano
  (`denied_by_user`).
- Un PR de `produced` no se cruza con los PRs del forge para el merge (eso sigue yendo por la tarea): solo se lista.

## 4. Ambigüedades del contrato Nod v1 (anotadas, sin cambiarlo)

1. **`task` en `session.start`.** Cuando la tarea sale de la rama, el `session.start` ya conoce la rama pero no
   lleva `task` (el `task.set` va justo después). chrono no abre grupos vacíos por eso, pero el contrato no dice si
   `session.start` debe llevarla.
2. **`cost_usd` y tareas.** Es acumulado por proceso, no por tarea: si la tarea cambia a mitad de sesión, el reparto
   por incrementos entre lecturas de `usage` es una aproximación (una lectura cubre el tramo desde la anterior).
3. **`vcs.commit.sha` abreviado.** Un sha de 7 caracteres puede volverse ambiguo en repos grandes; chrono lo deja
   sin resolver si casa con más de un commit.
4. **Commits reescritos.** Un `vcs.commit` de una rama que luego se aplasta (squash) o rebasea apunta a un sha que
   nunca llega a la base; el contrato no tiene un evento que lo relacione con el commit final.
5. **Ramas y tareas en `redactado`.** Van hasheadas (`h-xxxxxxxx`): sirven para agrupar, pero no para cruzar con
   las ramas de git ni con los PRs, así que el merge queda `unknown`.
6. **Qué es un permiso "negado".** El contrato no lo define; el collector de Nod solo cuenta los `deny` de
   `user`/`remote`, pero un `deny` de `policy` (reglas, modo, clasificador) también frena al agente. chrono da los
   dos números (`denied` y `denied_by_user`).
7. **Orden con `ts` iguales.** Varios eventos comparten `ts` (p. ej. `turn.end` y `usage`); chrono ordena por
   (`ts`, `instance`, `run`, `seq`), que dentro de un proceso es el orden de emisión, pero entre procesos el
   contrato no fija un orden total.

## Comandos en el título (`include_command`, opcional)

Por defecto el título de un evento no lleva texto libre: un `tool.call` sale como `tool.call Bash`. Con
`"include_command": "true"` en las `source_options` del fichero de Nod (el valor va como texto), y solo para
sesiones grabadas con privacidad `completo`, el título lleva además la primera línea del comando, recortada a
160 caracteres: `tool.call Bash: bash .claude/hooks/build/slice-ops.sh gate wiring_verified pass`. Sirve para
que `correlate` y `events --grep` distingan qué hizo la sesión alrededor de un evento de otra fuente (un
rechazo, un fallo de test). Es determinista: el mismo fichero da los mismos títulos.

```json
{"source_options": {"events.jsonl": {"adapter": "nod", "include_command": "true"}}}
```

