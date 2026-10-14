---
name: chrono-datos
description: "Analizar datos GRANDES que no caben o no conviene meter en el contexto —logs (texto o JSON), JSONL/NDJSON, CSV, volcados de una API, salidas de comandos de miles de líneas, logs exportados de Grafana/Loki— con chrono en vez de leerlos, hacer grep a ciegas o delegarlos a un subagente. Usar en cuanto un fichero o una salida pase de unos 50 KB o tenga miles de líneas parecidas."
---

# Datos masivos con chrono

chrono indexa una vez y responde con agregados citables: 24 h de logs de un entorno (~1 M tokens)
se resumen en ~1 100 (medido). Regla: **mide antes de leer**.

## 1. ¿Merece la pena?

`wc -c <fichero>` (o el tamaño de la salida). Menos de ~20 KB: léelo y ya. Más de ~50 KB, o miles
de líneas con la misma forma: chrono.

## 2. Meterlo en un índice

- Fuera de un repo, o para no mezclar: `chrono init <repo> --db /tmp/analisis/index.db` y después
  `--db` en cada comando.
- Fichero: `chrono add <fichero> --db …`. Detecta JSONL/NDJSON, CSV, syslog/nginx y texto.
  Fuerza el formato con `--as jsonl|csv|textlog|template`.
- Salida de un comando: guárdala en un fichero (`… > /tmp/x.jsonl`) y añádelo, o declárala como
  fuente de comando en `.chrono/config.json` (`commands`) para poder repetirla con `chrono sync`.
- Si los campos no se detectan solos, en `source_options` (o en `options` del comando):
  `time_field`, `message_field`, `level_field`, `entity_field`, `kind`, `id_field`,
  `links` (`commit=sha`), `explode` (un array → un evento por elemento).

## 3. Preguntar (siempre `--compact`)

| Quiero saber | Consulta |
|---|---|
| Reparto por nivel / tipo / origen | `chrono top level` · `top kind` · `top entity` · `top attr:<campo>` |
| Qué está pasando (agrupado) | `chrono patterns --level ERROR` (o `--grep texto`) |
| Desde cuándo / con qué ritmo | `chrono timeline --bucket 1h --grep "<texto>"` (`--by level`) |
| Cuánto (latencias, tamaños, duraciones) | `chrono stats <atributo> --by entity` → min/p50/p95/max |
| Los eventos concretos | `chrono events --level ERROR --limit 20` |
| Qué pasó alrededor de un evento en OTRAS fuentes | `chrono correlate <id> --delta 30m --kind commit` |

Acota con `--source <tipo|id|cmd:nombre>`, `--level`, `--grep`, `--since`.

## Cuándo NO

- Datos pequeños: leer es más barato que indexar.
- Necesitas el texto literal completo de algo concreto: localízalo con chrono (`events`, `show`) y
  lee solo eso.

Nunca pegues al usuario miles de líneas: resume con lo que da chrono y cita ids de ejemplo.
