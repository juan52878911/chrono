---
name: chrono-ci
description: "Observabilidad del PIPELINE de CI/CD con chrono sobre GitHub Actions (u otro sistema vía comandos): por qué falla un workflow o un despliegue, qué workflow falla más, qué jobs/pasos se vuelven lentos, qué commit rompió un despliegue y cuál lo arregló. Usar ante un CI/despliegue en rojo o al revisar la salud del pipeline, en vez de leer logs de Actions enteros. NO es para los logs de los servicios en ejecución (Loki/Grafana, errores, excepciones o reinicios de una app o base de datos): eso es chrono-datos."
---

> **Un fallo suelto vs. patrones.** Para «¿por qué falló ESTE run?» lo más directo y barato es
> `gh run view <id> --log-failed | tail -40` (medido: 3 llamadas, ~3,7k tokens). chrono gana cuando
> la pregunta cruza runs: qué workflow o paso falla más, desde cuándo, qué commit lo rompió y cuál lo
> arregló, qué precede a los despliegues fallidos (`precede`), o «¿falló igual antes?» (`events
> source=cmd:gh-fallos grep=…`). Si el índice tiene `cmd:gh-fallos`, un `events source=cmd:gh-fallos
> grep=<run_id>` da el paso y las líneas del error sin abrir el log entero.

# CI/CD con chrono

> **Requisito**: el binario `chrono` (port Rust ≥ 0.2). Si `chrono version` falla o no
> reconoce el comando que necesitas, díselo al usuario con
> `cargo install --git https://github.com/juan52878911/chrono chrono-cli --locked`
> (necesita Rust). No lo instales sin su permiso.

Un log de fallo de Actions son ~60 k tokens; los de 25 fallos, ~1,3 M. Con chrono, las causas
agrupadas de los 25 caben en ~500 (medido). No hagas `tail` del log: suele mostrar solo
`exit code 1` y esconde la causa.

## Preparar (una vez por repo)

El preset viene con chrono: `examples/github-actions/` (`config.json` + 2 scripts) en el repo
https://github.com/juan52878911/chrono (o en la carpeta del plugin, si lo instalaste como plugin).

1. `mkdir -p .chrono/scripts && cp <chrono>/examples/github-actions/*.sh .chrono/scripts/`
2. Fusiona su clave `commands` en `.chrono/config.json` cambiando `OWNER/REPO` y los workflows
   de despliegue. Es configuración del usuario: enséñale el cambio antes de escribirlo.
3. `chrono init` (si no hay índice) y `chrono add cmd:gh-runs`, `cmd:gh-deploys`,
   `cmd:gh-steps`, `cmd:gh-fallos`.

Requiere `gh` autenticado y `jq`. Otros sistemas (GitLab, Jenkins, Argo, kubectl): el mismo
mecanismo con su CLI —cualquier comando que imprima JSON es una fuente.

## Antes de cada pregunta

`chrono sync` (vuelve a ejecutar los comandos y procesa solo lo nuevo).

## Pregunta → consulta (siempre `--compact`)

| Pregunta | Consulta |
|---|---|
| ¿Cómo está el pipeline? | `chrono top level --source cmd:gh-runs` |
| ¿Qué workflow falla más? | `chrono top entity --source cmd:gh-runs --level FAILURE` |
| ¿Por qué falla? (todas las ejecuciones) | `chrono patterns --source cmd:gh-fallos --level ERROR` |
| ¿Desde cuándo sale este error? | `chrono timeline --bucket 1d --grep "<texto>" --source cmd:gh-fallos` |
| ¿Qué jobs son lentos? | `chrono stats duration_s --by entity --kind ci.step` |
| Despliegues fallidos | `chrono events --source cmd:gh-deploys --level FAILURE --limit 10` |
| ¿Qué commit lo rompió? ¿y sus ejecuciones de CI? | `chrono trace <id del despliegue>` |
| ¿Hubo un arreglo justo después? | `chrono correlate <id> --kind commit --delta 2h` |
| Ese commit, ¿toca algo peligroso? | `chrono hotspots` / `chrono coupling <fichero>` (skill chrono-historia) |

## Cuándo NO

- Un fallo concreto que acabas de ver y del que ya tienes el log abierto: `gh run view <id>
  --log-failed | grep -E "FAILED|Error"` puede bastar.
- Contar fallos por workflow una vez: un `gh run list --json … --jq 'group_by(…)'` ocupa lo
  mismo (empate medido). chrono gana cuando hay que cruzarlo con commits o con otras fuentes.
