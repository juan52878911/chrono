---
name: chrono-datos
description: "Logs de servicios y aplicaciones en ejecución (Loki/Grafana u otro MCP de observabilidad, ficheros, comandos, APIs) con cifras EXACTAS usando chrono en vez de leerlos: cuántos errores hay y en qué servicios, cuáles se repiten más, desde cuándo, qué pasó antes de un reinicio o una caída, la causa de una excepción. También ante cualquier fichero o salida de más de ~50 KB o miles de líneas parecidas (JSONL, CSV, syslog, volcados). No hace falta que se nombre chrono. (Para el pipeline de CI/CD de GitHub Actions: chrono-ci.)"
---

# Logs y datos masivos con chrono

chrono indexa una vez y responde con agregados exactos sobre TODO lo indexado: 24 h de logs de
un entorno (~1 M tokens) se resumen en ~1 100 (medido). Las herramientas que devuelven líneas
crudas mienten por omisión: el MCP de Grafana corta en 100 líneas por consulta, y un modelo que
cuenta sobre esa muestra da cifras falsas con aplomo (medido: 6 de 12 totales mal sin chrono,
12 de 12 bien con él).

## 0. ¿Lo tiene ya chrono?

Antes de contar con otra herramienta, mira qué hay indexado:

- MCP de chrono: tool `sources` → fuentes, nº de eventos, ventana (`first`/`last`), niveles y
  entidades principales. Una fuente con alias `cmd:loki` (o similar) son los logs de Loki ya volcados.
- Terminal: `chrono top kind --compact` y `chrono top entity --compact`.

Si la ventana que te piden está dentro, las cifras salen de chrono (sección 2). Si no está,
cárgalo (sección 1) o, si no puedes, dilo y usa la otra herramienta solo para leer detalle.

## 1. Meterlo en el índice

- **Loki/Grafana**: el preset `examples/grafana-loki` del repo de chrono es una fuente de comando
  (`cmd:loki`) que pagina la API sin tope; `chrono add cmd:loki` la carga y `chrono sync` trae solo lo
  nuevo (el plugin de Claude Code ejecuta `sync` al empezar la sesión).
- **Fichero**: `chrono add <fichero>`. Detecta JSONL/NDJSON, CSV, syslog/nginx y texto
  (`--as jsonl|csv|textlog|template` para forzarlo).
- **Muchos ficheros**: `chrono add '<dir>/**/*.jsonl' --as jsonl --name <alias>` (entre comillas) es UNA
  fuente; `sync` solo relee los ficheros nuevos o que crecieron, y `--source <alias>` filtra.
  Sesiones de Claude Code: `chrono add '~/.claude/projects/**/*.jsonl' --as jsonl --name claude
  --preset claude-sessions` (una tool = un evento; entidad = la tool, ERROR si falló; sin el input
  ni el contenido, salvo la primera línea de los errores). Qué precede a un fallo:
  `precede` con `level=ERROR entity=Bash before_levels=*` (`join=sessionId`: solo lo de la misma sesión).
- **Relacionar fuentes por clave**: `relations` en `.chrono/config.json` (versión 2) declara de dónde sale
  una clave en cada fuente (campo, `link:<rel>` o patrón sobre la ruta/el título, p. ej.
  `{"relations": {"prueba": [{"source": "pruebas", "field": "file", "pattern": "prueba-(\\d+)"}, …]}}`;
  `chrono config check` la valida). Una dim (`sessionId`) o un link (`ticket`, `tool_use`) ya son claves.
- **Inventario de una carpeta** (PDF, imágenes, audio…): `chrono add <carpeta> --as dir`: un evento
  por fichero con tipo, formato, tamaño y fechas; la carpeta es la entidad
  (`top attr:type`, `stats size --by attr:type`). No lee el contenido.
- **Salida de un comando**: declárala en `.chrono/config.json` → `commands` y `chrono add cmd:<nombre>`,
  o guárdala en un fichero y añádelo.
- Fuera de un repo, o para no mezclar: `--db /tmp/analisis/index.db` en cada comando.
- Si los campos no se detectan solos, en `options` del comando (o `source_options`):
  `time_field`, `message_field`, `level_field`, `entity_field` (el servicio), `kind`, `id_field`,
  `links` (`commit=sha`), `explode`, `level_map` (`true=ERROR`), `where`, `attrs`, `dims` (attrs por los
  que se agrupa o filtra a menudo, p. ej. `host,container`: indexadas). `chrono config check`
  valida `.chrono/config.json` y dice qué clave falla y por qué.

## 2. Preguntar

Los mismos filtros en `top`, `patterns`, `timeline`, `events` y `stats`, en el MCP y en el CLI:
`since`/`until` (ISO; `until` exclusivo), `level` (lista con comas, sin distinguir mayúsculas),
`entity` (servicio o fichero; lista con comas), `source` (`cmd:loki`, `jsonl`, `git`…), `kind`, `grep`.
En el CLI son `--since`, `--until`, `--level`… y siempre `--compact`.

| Quiero saber | MCP | CLI |
|---|---|---|
| **Todo el panorama de una vez (empieza aquí)** | `informe` (since/until/entity/source) | `chrono informe` |
| Por qué un fichero de código está así | `informe` tipo=codigo target=fichero[:A-B] | `chrono informe codigo fichero[:A-B]` |
| Cuántas líneas por nivel | `top` dim=level | `chrono top level` |
| En qué servicios caen los errores | `top` dim=entity, level=FATAL (una llamada por nivel para el reparto) | `chrono top entity --level FATAL` |
| Los mensajes más repetidos | `patterns` level=FATAL,ERROR,CRITICAL limit=3 | `chrono patterns --level FATAL,ERROR,CRITICAL --limit 3` |
| Qué escupe un servicio | `patterns` entity=product_service | `chrono patterns --entity product_service` |
| Desde cuándo / picos / incidentes | `timeline` grep="<mensaje>" bucket_secs=600 | `chrono timeline --grep "<mensaje>" --bucket 10m` |
| Cuántas veces pasó X, cuándo y qué lo precede | `precede` grep="<mensaje de X>" attr=container:<nombre> | `chrono precede --grep "<mensaje>" --attr container:<nombre>` |
| Qué pasó justo antes de X | `events` entity=postgres until=<ts> limit=30 | `chrono events --entity postgres --until <ts>` |
| Una traza o excepción completa | `events` entity=<svc> since=<ts> limit=80 | `chrono events --entity <svc> --since <ts> --limit 80` |
| Latencias, tamaños, duraciones | `stats` attr=duration_s by=entity | `chrono stats duration_s --by entity` |
| Qué hubo en OTRAS fuentes alrededor | `correlate` id=<evento> delta_secs=1800 | `chrono correlate <id> --delta 30m --kind commit` |
| Qué comparte una clave con un evento (misma sesión, sha, prueba) | `correlate` id=<evento> via=<clave> | `chrono correlate <id> --via <clave>` |
| Los eventos de una clave | `events` join=<clave>=<valor> | `chrono events --join <clave>=<valor>` |
| X sin Y (pruebas sin resultado, sesiones sin commit; solo cubre un Y posterior al primer X, any_order=true para cualquiera) | `gaps` from="<fuente> kind=…" to=<fuente> via=<clave> by=<unidad> | `chrono gaps --from '<fuente> grep=texto con espacios' --to <fuente> --via <clave> --by <unidad> [--until <ts>] [--any-order]` |

Patrones de trabajo:
- **Cifras**: siempre de `top`/`patterns`/`timeline`, nunca contando líneas de `events` o de Loki.
- **Causa de una excepción**: `patterns` o `events` con level=ERROR para ubicarla, y después
  `events` del mismo servicio desde ese instante para leer la traza hasta la última línea
  (`AttributeError: …`) y la petición que falló (`DELETE /api/… 500`).
- **Líneas sin nivel**: si `sources` muestra `unlabeled_inferred` (líneas sin nivel cuyo texto dice
  `ERROR:`/`FATAL:`), un filtro `level` NO las ve.
  - Las CIFRAS por nivel (cuántos FATAL, top de errores) se dan SIEMPRE sin infer_level: son las de la
    fuente y las que coinciden con Grafana/Loki. Si quieres, añade aparte «+N líneas sin nivel cuyo
    texto dice FATAL», pero nunca las sumes al total.
  - infer_level=true sirve para ENCONTRAR (¿hubo un error antes de X?, `events`, `precede` ya lo usa):
    nunca niegues un error solo porque `level` no lo encuentra.
- **Reinicios/caídas**: primero `precede` con el mensaje de apagado y el contenedor; si un episodio trae
  dos `origins` (p. ej. un preflight y el principal), sepáralos con `attr=container:<nombre>`. Por defecto
  `precede` da los precursores comunes y la lista de episodios (depth=summary); el detalle de lo que
  precedió a cada episodio, con depth=full y, si son muchos, limit o since/until. A mano:
  `timeline` con grep del mensaje de apagado exacto (en Postgres,
  `received fast shutdown request`; de arranque, `database system is ready to accept connections`),
  entity del servicio y bucket_secs=60: cada bucket es un reinicio con su hora. Para lo que lo
  precede, `timeline` con level=ERROR,FATAL en la misma ventana, o `events` con until=<hora>.
  Ojo: un sidecar o contenedor de preflight del mismo servicio emite los mismos mensajes; si los
  eventos traen `container`, sepáralos con `top dim=attr:container grep=<mensaje>`.
- Cita los números tal cual vienen; si chrono no tiene un dato, dilo en vez de estimarlo.

## 3. Pasada por el código (en una auditoría o «busca errores»)

Los logs solo enseñan lo que ya falló. Antes de entregar, dedica una pasada corta al código
(medido: sin ella se escapó un fallback que mezclaría tenants y que una sesión sin chrono sí vio):

- **Confirma cada causa en el código** de la rama que se despliega (`origin/dev` o la que diga
  `sources`), no en un worktree atrasado: fichero:línea de lo que produce el mensaje.
- **Sigue cada WARNING repetido hasta su origen** y mira qué hace el código cuando falla: valores
  por defecto silenciosos (URL sin contraseña, «primer tenant», almacenamiento local), `except` que
  solo registran, cabeceras opcionales que deciden el tenant o los permisos. Eso es un riesgo aunque
  hoy no rompa nada.
- **Busca el mismo patrón en los demás servicios** (`grep -n` del nombre de la variable o del
  fallback): si a uno le falta `REDIS_URL`, revisa todos.
- **Configuración que no se ve en logs**: reglas de alerta vacías, servicios sin healthcheck,
  ventana de retención de logs más corta que la pedida.

Delega la pasada en un subagente si son muchos ficheros, y sepáralo en la respuesta: «fallos que
ya ocurren» (con cifras de chrono) y «riesgos en el código» (con fichero:línea).

## Cuándo NO

- Datos pequeños (< ~20 KB): leer es más barato que indexar.
- Necesitas el texto literal completo de algo concreto: localízalo con chrono y lee solo eso.

> **Requisito**: el binario `chrono` ≥ 0.2.2. El plugin de Claude Code lo instala solo; fuera
> del plugin: `cargo install --git https://github.com/juan52878911/chrono chrono-cli --locked`.
