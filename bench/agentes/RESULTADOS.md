# chrono frente a no usarlo — preguntas reales de programación

Medido el 2026-10-14 sobre **AuraCRM** (repo de 1 037 commits en `dev`, 500 ejecuciones de
GitHub Actions, 309 despliegues, 60 ejecuciones con jobs/pasos, 25 fallos con su log, y 24 h
de logs del entorno sandbox). Script: `bench/agentes/simular.py` (reproducible).

**Qué se compara**: los tokens que un agente tiene que LEER para responder bien.
- *Sin chrono*: la salida cruda de los comandos necesarios (`git log --numstat`, `gh run view --log-failed` × N…).
- *Con chrono*: la salida `--compact` de la consulta equivalente.
- *Alternativa hábil*: un pipeline de shell bien escrito (awk, `uniq -c`, `--jq`) cuando lo hay.

Tokens ≈ bytes / 4.

| Caso | Pregunta | Sin chrono | Con chrono | Reducción | Latencia | Alternativa hábil |
|---|---|---:|---:|---:|---:|---|
| ci-causas | ¿Por qué fallaron las últimas 25 ejecuciones? | 1 311 126 | 476 | ×2 750 | 80 ms | — |
| ci-lentos | ¿Qué jobs/pasos tardan más (p50/p95)? | 238 714¹ | 93 | ×2 567 | 8 ms | — |
| logs-24h | Logs de 24 h: ¿qué está fallando? | 1 069 005 | 1 144 | ×934 | 43 ms | — |
| hotspots | ¿Qué ficheros son más peligrosos de tocar? | 103 853 | 364 | ×285 | 9 ms | 257 (awk) |
| bugs | ¿Dónde se concentran los arreglos y de qué tipo? | 126 247 | 495 | ×255 | 13 ms | — |
| acoplamiento | ¿Qué cambia junto con `api_gateway/src/main.py`? | 13 998 | 336 | ×42 | 6 ms | — |
| ci-salud | ¿Cuánto falla el CI/CD y qué workflow más? | 47 097 | 57 | ×826 | 61 ms | **71 (`--jq`)** |
| dueños | ¿Quién conoce `messaging_service`? | 1 009 | 41 | ×25 | 14 ms | **24 (`uniq -c`)** |
| despliegue | Despliegue fallido: ¿qué commit lo rompió y cuál lo arregló? | 999 | 720 | ×1,4 | 15 ms | — |

¹ Cota inferior: es la API de jobs ya reducida a NDJSON; la respuesta cruda pesa más.

## Lectura honesta

**Donde chrono brilla (×250 – ×2 750)**: preguntas cuya respuesta exige mirar MUCHO dato
repetitivo — logs de CI de muchas ejecuciones, logs de un entorno, la historia entera de git
para hotspots/bugs, duraciones de cientos de pasos. Sin índice, el agente o lo lee todo (y no
le cabe) o hace `tail`/`grep` a ciegas y se pierde la causa. Con chrono recibe la respuesta
agrupada y citable en < 1 500 tokens y en < 100 ms.

**Empates**: cuando la pregunta es una agregación simple sobre UNA fuente (`¿quién toca esto?`,
`¿cuántos fallos por workflow?`), un `uniq -c` o un `--jq` bien escrito ocupa lo mismo. chrono
no ahorra tokens ahí; aporta lo que viene después: cruzar esa respuesta con otra fuente.

**Donde no ahorra tokens pero sí trabajo**: el caso *despliegue*. Las salidas crudas ya son
pequeñas, pero el agente necesita saber qué pedir (`gh run view` → sha → `git show` → `git log`
alrededor de la hora). `chrono trace` + `correlate --kind commit` lo da en una consulta: el
despliegue, su commit, sus ejecuciones de CI y el commit que lo arregló 3 minutos después.

**Lo que no se mide aquí**: leer código. `cat`/`sed`/`Read` de ficheros fuente son el mayor
consumo real de un agente (medido en 1 057 sesiones: ~15 M tokens de 46 M), y chrono no lo
sustituye — el modelo necesita el código literal. Para eso sirve un índice de código, no chrono.
