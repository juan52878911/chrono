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
| **Voy a leer/tocar estas líneas: ¿por qué están así?** | `chrono explain <fichero>:DESDE-HASTA --compact` |
| ¿Qué se rompe si toco X? | `chrono coupling <fichero> --compact` |
| ¿Quién conoce esto? ¿bus factor? | `chrono owners <ruta> --compact` |
| ¿Dónde se concentran los arreglos y de qué tipo? | `chrono bugs --compact` |
| ¿Qué entró con el ticket/PR? | `chrono tickets <ID> --compact` |
| ¿Cómo están las ramas frente a la base? | `chrono branches <base> --compact` |
| ¿Hubo algo parecido antes? | `chrono search "<texto>" --compact` y `chrono similar <sha> --compact` |
| Por funciones, no por fichero | añade `--by symbol` (requiere `chrono init --symbols`) |

Ventana temporal: `--since AAAA-MM-DD`. Más filas: `--limit N` (la salida dice `truncated` si cortó).

## `explain`: antes de tocar un trozo

`chrono explain src/x.py:120-180 --compact` da, en unos cientos de tokens (frente a ~10 k de
`git log -L`):

- **por qué está así**: los commits que escribieron esas líneas, con fecha, autor, cuántas
  líneas puso cada uno y su título;
- **si eran arreglos**: `fix=yes` (prefijo `fix:` o revert), `maybe` (solo una palabra clave
  en el mensaje: no lo afirmes como arreglo), `no`;
- **tickets/PRs** de esos commits, si el índice los tiene;
- **qué cambia con esas líneas** (`changes_with`): entidades que cambiaron en ≥2 de esos
  commits; con `chrono init --symbols`, también las funciones concretas.

Usa `changes_with` como **lista de «revisa también»**, no como certeza: medido sobre sesiones
reales, ~4 de cada 10 de esos avisos acabaron siendo ficheros que hubo que tocar (frente a
~2 de cada 10 de «lo de la misma carpeta»). Menciónalos al usuario antes de dar el cambio
por terminado.

La salida es determinista (mismo índice ⇒ mismos bytes) y es la fuente: cita sus ids y
fechas tal cual; no completes con suposiciones lo que no aparece. Si el índice está atrasado
respecto al código que lees (`revision` distinta de HEAD), haz `chrono sync` primero.

## Cuándo NO

- Un commit concreto que ya conoces: `git show <sha>` directamente.
- «¿Quién toca esta ruta?» en un repo pequeño: `git log --format=%an -- ruta | sort | uniq -c`
  ocupa lo mismo (empate medido).
- Leer el CÓDIGO: chrono no lo sustituye.

Cita lo que digas con los ids/shas que devuelve chrono; no inventes cifras que no estén en su salida.
