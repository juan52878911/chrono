---
name: chrono-historia
description: "Preguntas sobre la HISTORIA de un repo git con chrono en vez de leer git log/show crudo: qué es peligroso tocar, qué cambia junto con un fichero, quién conoce una zona (bus factor), dónde se concentran los bugs y de qué tipo, estado de ramas, qué entró con un ticket. Usar antes de un cambio arriesgado, una revisión o un refactor, aunque no se nombre chrono."
---

# Historia de un repo con chrono

`git log --numstat` de un repo mediano son ~100 k tokens; `chrono hotspots` responde lo mismo en
~400 (medido: `bench/agentes/RESULTADOS.md` del repo de chrono). No leas la historia cruda para
estas preguntas.

## Antes

1. ¿Hay índice? `chrono hotspots --compact --limit 1`. Si dice que no hay, `chrono init` en la
   raíz del repo (segundos; crea `.chrono/`, ignorado por git salvo `config.json`).
2. Si el repo avanzó: `chrono sync`.

Usa **siempre** `--compact` (tabla de texto, sin metadatos, pensada para agentes).

## Pregunta → consulta

| Pregunta | Consulta |
|---|---|
| ¿Qué es peligroso tocar? | `chrono hotspots --compact` |
| ¿Qué se rompe si toco X? | `chrono coupling <fichero> --compact` |
| ¿Quién conoce esto? ¿bus factor? | `chrono owners <ruta> --compact` |
| ¿Dónde se concentran los arreglos y de qué tipo? | `chrono bugs --compact` |
| ¿Qué entró con el ticket/PR? | `chrono tickets <ID> --compact` |
| ¿Cómo están las ramas frente a la base? | `chrono branches <base> --compact` |
| ¿Hubo algo parecido antes? | `chrono search "<texto>" --compact` y `chrono similar <sha> --compact` |
| Por funciones, no por fichero | añade `--by symbol` (requiere `chrono init --symbols`) |

Ventana temporal: `--since AAAA-MM-DD`. Más filas: `--limit N` (la salida dice `truncated` si cortó).

## Cuándo NO

- Un commit concreto que ya conoces: `git show <sha>` directamente.
- «¿Quién toca esta ruta?» en un repo pequeño: `git log --format=%an -- ruta | sort | uniq -c`
  ocupa lo mismo (empate medido).
- Leer el CÓDIGO: chrono no lo sustituye.

Cita lo que digas con los ids/shas que devuelve chrono; no inventes cifras que no estén en su salida.
