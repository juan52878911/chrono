---
name: chrono-intermediario
description: "Configurar chrono como intermediario entre las herramientas y el modelo: analiza los MCP y los ficheros de datos del proyecto, decide en cuáles conviene interceptar (salidas grandes de logs, filas o registros que hay que contar) y con qué límites, y escribe .chrono/intermediario.json. Usar cuando se pida «pon chrono de intermediario», «que chrono resuma lo que devuelven los MCP», al conectar un MCP de observabilidad, base de datos o CI que devuelve muchas líneas, o cuando una herramienta corte sus resultados (p. ej. Loki a 100 líneas)."
---

# chrono como intermediario

El plugin trae dos hooks que no hacen NADA hasta que exista `.chrono/intermediario.json`:

- **PostToolUse (herramientas MCP)**: si una regla casa con la herramienta y la salida pasa sus
  límites, la SUSTITUYE por cifras exactas (niveles, servicios, contenedores, mensajes más
  repetidos por nivel) + una muestra corta, y deja todas las filas en el índice de chrono para
  seguir preguntando. Lo que no pasa los límites llega intacto.
- **PreToolUse (Read)**: leer ENTERO un fichero de datos grande (`.log`, `.jsonl`, `.csv`…) se
  cambia por su resumen exacto. Leer un trozo (`offset`/`limit`) nunca se intercepta.

Tu trabajo aquí es decidir dónde merece la pena y escribir esa configuración. **Menos es más**:
interceptar donde no hace falta añade latencia y le quita al modelo texto que sí quería leer.

## 1. Inventario

Lista las herramientas que hay en esta sesión (las tuyas, `mcp__<servidor>__<tool>`) y de dónde
vienen: `.mcp.json` del proyecto, `~/.claude.json` (`mcpServers`), plugins. Para cada servidor
apunta qué devuelve cada tool. Si no lo sabes, llámala UNA vez con una consulta pequeña.
Mira también si el proyecto tiene ficheros de datos grandes (`find . -size +200k -name '*.log'
-o -size +200k -name '*.jsonl' -o -size +200k -name '*.csv'`, sin entrar en `node_modules`/`.git`).

## 2. Decidir, herramienta por herramienta

| Devuelve | ¿Interceptar? | Regla |
|---|---|---|
| Líneas de log de Loki (`query_loki_logs` y similares) | **Sí**: el MCP corta y el modelo cuenta mal sobre la muestra | `adapter: "loki"` (vuelve a pedir la consulta COMPLETA paginando) |
| Logs de otro sistema (Elastic, Datadog, CloudWatch…) en JSON | Sí, si pueden pasar de ~30 KB | `adapter: "records"` |
| Filas de una base de datos, runs de CI, tickets, eventos de auditoría | Solo si hay que contarlos/agruparlos y pueden ser muchos | `adapter: "records"`, `min_records` ≥ 50 |
| Métricas ya agregadas (PromQL, `count_over_time`, stats) | **No**: ya son exactas | — (el adaptador loki ya las deja pasar) |
| Código, búsqueda de código, docs, páginas web, ficheros de config | **No**: el modelo necesita el texto literal | — |
| Respuestas pequeñas o de detalle (un registro, un panel, un dashboard) | **No** | — |

## 3. Límites (valores por defecto razonables)

| Clave | Por defecto | Qué hace |
|---|---|---|
| `min_lines` (loki) | 50 | Si la consulta casa con menos líneas y caben en la salida, no se toca |
| `min_bytes` (records) | 30000 | Salidas más pequeñas pasan intactas |
| `min_records` (records) | 20 | Menos registros que esto: no se agrega |
| `max_lines` (loki) | 50000 | Tope de líneas que se vuelven a pedir |
| `timeout_secs` | 45 | Si el adaptador tarda más, se queda con lo que tenga y lo dice |
| `sample_lines` | 40 (loki) / 15 (records) | Líneas literales que se devuelven además del resumen |
| `read.min_bytes` | 200000 | Ficheros más pequeños se leen normal |
| `read.extensions` | `.log .jsonl .ndjson .csv .tsv` | Solo esos tipos |

## 4. Escribir `.chrono/intermediario.json`

```json
{
  "enabled": true,
  "rules": [
    {"tool": "mcp__grafana__query_loki_logs", "adapter": "loki", "name": "loki",
     "url": "https://grafana.ejemplo/api/datasources/proxy/uid/<uid-de-loki>",
     "token_cmd": "security find-generic-password -s grafana-token -w",
     "min_lines": 50, "max_lines": 50000, "sample_lines": 40, "timeout_secs": 45},
    {"tool": "mcp__postgres__query", "adapter": "records", "name": "pg",
     "min_bytes": 30000, "min_records": 50, "dims": ["status", "service"]}
  ],
  "read": {"enabled": true, "min_bytes": 200000}
}
```

- `tool` es una expresión regular sobre el nombre completo (`mcp__grafana__query_loki_logs`).
- Credenciales: **nunca** el token literal en el fichero. Usa `token_cmd` (llavero, `pass`,
  `op read`…) o las variables `LOKI_URL`/`LOKI_TOKEN_CMD` del entorno.
- En `records`, `time_field`/`message_field` dicen qué campo es la fecha y cuál el mensaje, y
  `dims` qué campos agrupar.
- Añade `.chrono/` al `.gitignore` si no lo está (el índice y las cachés no se versionan).

## 5. Comprobar y explicar

- Prueba en seco una regla: `echo '{"cwd":"'$PWD'","tool_name":"<tool>","tool_input":{…}}' |
  python3 -I "${CLAUDE_PLUGIN_ROOT}/scripts/claude/intermediario.py" post`, y revisa que la salida
  tenga sentido.
- Explica al usuario, en una tabla corta, qué herramientas quedan interceptadas, con qué límites y
  por qué el resto no.
- Desactivar: `"enabled": false`, borrar el fichero, o `CHRONO_INTERMEDIARIO=0`.
  Diagnóstico: `CHRONO_INTERMEDIARIO_LOG=/tmp/intermediario.log`.

> El intermediario necesita `python3` en el PATH y chrono ≥ 0.2.3 (lo instala el plugin).
