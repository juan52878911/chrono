# Preset: GitHub Actions como fuentes de chrono

Cuatro **fuentes de comando** (`commands` en `.chrono/config.json`) que convierten GitHub Actions en
eventos del índice, unidos a sus commits por el sha (`links: commit=…`):

| Fuente | Qué trae | `kind` |
|---|---|---|
| `gh-runs` | las últimas 500 ejecuciones (todas los workflows) | `ci.run` |
| `gh-deploys` | las ejecuciones de los workflows de despliegue | `ci.deploy` |
| `gh-steps` | jobs y pasos de las últimas 60, con `duration_s` | `ci.step` |
| `gh-fallos` | las líneas con causa de los logs de los últimos 25 fallos | `ci.log` |

Instalar en un repo:

```bash
mkdir -p .chrono/scripts && cp examples/github-actions/*.sh .chrono/scripts/
# Fusionar la clave "commands" de config.json en .chrono/config.json, cambiando
# OWNER/REPO y los nombres de los workflows de despliegue.
chrono init
chrono add cmd:gh-runs && chrono add cmd:gh-deploys && chrono add cmd:gh-steps && chrono add cmd:gh-fallos
chrono sync        # vuelve a ejecutar los comandos y procesa solo lo nuevo
```

Preguntas:

```bash
chrono top level --source cmd:gh-runs                      # salud del CI/CD
chrono top entity --source cmd:gh-runs --level FAILURE     # qué workflow falla más
chrono patterns --source cmd:gh-fallos --level ERROR       # causas de los fallos, agrupadas
chrono timeline --bucket 7d --grep "No module named" --source cmd:gh-fallos   # desde cuándo
chrono stats duration_s --by entity --kind ci.step         # jobs lentos (p50/p95/max)
chrono events --source cmd:gh-deploys --level FAILURE      # despliegues fallidos
chrono trace <id>                                          # → su commit y sus ejecuciones de CI
chrono correlate <id> --kind commit --delta 2h             # commits de alrededor (¿el arreglo?)
```

Requisitos: `gh` autenticado y `jq`. Coste: `gh-steps` hace una llamada a la API por ejecución y
`gh-fallos` descarga los logs de los fallos; baja `N` en el comando si haces `sync` a menudo.
Nada de esto es específico de chrono: cualquier comando que imprima JSON (kubectl, docker,
`curl` a una API, un script propio) se añade igual.
