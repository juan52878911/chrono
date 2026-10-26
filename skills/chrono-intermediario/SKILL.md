---
name: chrono-intermediario
description: "Configurar chrono como intermediario entre las herramientas y el modelo: analiza los MCP y los ficheros de datos del proyecto, decide en cuáles conviene interceptar (salidas grandes de logs, filas o registros que hay que contar) y con qué límites, y escribe la clave "intermediario" de .chrono/config.json. Usar cuando se pida «pon chrono de intermediario», «que chrono resuma lo que devuelven los MCP», al conectar un MCP de observabilidad, base de datos o CI que devuelve muchas líneas, o cuando una herramienta corte sus resultados (p. ej. Loki a 100 líneas)."
---

# chrono como intermediario

El plugin trae dos hooks que no hacen NADA hasta que exista la clave `intermediario` en
`.chrono/config.json` (o el fichero antiguo `.chrono/intermediario.json`, que se sigue leyendo si
falta la clave):

- **PostToolUse (herramientas MCP)**: si una regla casa con la herramienta y la salida pasa sus
  límites, la SUSTITUYE por cifras exactas (niveles, servicios, contenedores, mensajes más
  repetidos por nivel) + una muestra corta, y deja todas las filas en el índice de chrono para
  seguir preguntando. Lo que no pasa los límites llega intacto.
- **Read**: leer ENTERO un fichero de datos muy grande (≥ `read.deny_bytes`, 5 MB) se cambia por su
  resumen exacto; uno mediano (entre `read.min_bytes` y `deny_bytes`) se lee normal, se indexa y se le
  dice al modelo que las cifras salen de chrono. Leer un trozo (`offset`/`limit`) nunca se intercepta.
- Con la regla `loki`, cada consulta indexa además EN SEGUNDO PLANO la ventana completa de su selector
  (`auto_index`, por defecto sí): lo siguiente que se pregunte ya está en chrono. La salida solo se
  sustituye si venía truncada.

- **Stop (verificador de cifras, APAGADO por defecto)**: con `"verify": true` en esa config o
  `CHRONO_VERIFY=1`, si la sesión usó chrono y la respuesta final da conteos que no
  salen de ninguna salida de chrono (ni de un agregado pequeño de otra consulta), pide UNA vez que se
  comprueben o se marquen como estimación. Ignora fechas, horas, versiones, IPs, duraciones, puertos y
  códigos HTTP; acepta sumas de dos cifras que aparecen en la misma línea. Está apagado porque, medido con
  Opus, al bloquear el modelo contesta al aviso y ese mensaje queda como respuesta final, y los falsos
  positivos (códigos HTTP, sumas de varias cifras) lo disparan: 3 de 6 sesiones se estropearon.

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
| `read.min_bytes` | 200000 | Ficheros más pequeños se leen sin más |
| `read.deny_bytes` | 5000000 | Desde aquí, leer entero se sustituye por el resumen |
| `auto_index` (loki) | true | Indexar en segundo plano la ventana del selector |
| `index_max_lines` (loki) | 200000 | Tope del indexado de fondo |
| `read.extensions` | `.log .jsonl .ndjson .csv .tsv` | Solo esos tipos |

## 4. Escribir `intermediario` en `.chrono/config.json`

Lo que sigue va como valor de `"intermediario"` (junto a `"version": 1` y el resto de la config).
Si ya hay un `.chrono/intermediario.json`, muévelo ahí y bórralo. Después, `chrono config check`.

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

- `tool` es el nombre completo (`mcp__grafana__query_loki_logs`), con comodines `*`/`?` y alternativas con `|`.
- Credenciales: **nunca** el token literal en el fichero. Usa `token_cmd` (llavero, `pass`,
  `op read`…) o las variables `LOKI_URL`/`LOKI_TOKEN_CMD` del entorno.
- En `records`, `time_field`/`message_field` dicen qué campo es la fecha y cuál el mensaje, y
  `dims` qué campos agrupar.
- Añade `.chrono/` al `.gitignore` si no lo está (el índice y las cachés no se versionan).

## 5. Comprobar y explicar

- Prueba en seco una regla: `echo '{"cwd":"'$PWD'","tool_name":"<tool>","tool_input":{…}}' |
  chrono hook post`, y revisa que la salida
  tenga sentido.
- Explica al usuario, en una tabla corta, qué herramientas quedan interceptadas, con qué límites y
  por qué el resto no.
- Desactivar: `"enabled": false`, quitar la clave `intermediario`, o `CHRONO_INTERMEDIARIO=0`.
  Diagnóstico: `CHRONO_INTERMEDIARIO_LOG=/tmp/intermediario.log`.

> El intermediario es parte del binario (`chrono hook`, ≥ 0.2.3, lo instala el plugin) y usa `curl` para Loki.
