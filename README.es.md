# chrono

> 🇬🇧 Prefer English? Read the [English README](README.md).

**Convierte el historial de Git de un repo en respuestas.** chrono lee tus commits, cambios, autores y PRs una vez, los guarda en un índice local, y responde preguntas como *"¿qué es peligroso tocar?"*, *"¿qué se rompe si cambio este fichero?"* o *"¿quién sostiene esta zona del código?"* — en JSON, al instante, sin que tú (ni una IA) tengáis que leer miles de commits.

Un solo binario. Sin servidor, sin daemon. Corre en tu Mac o en cualquier Linux.

```console
$ cd mi-repo
$ chrono init            # indexa el repo (una vez)
$ chrono hotspots        # ¿qué ficheros son un campo de minas?
$ chrono coupling src/auth.go   # ¿qué cambia siempre junto a esto?
$ chrono bugs            # ¿dónde se concentran los arreglos y de qué tipo?
```

---

## Requisitos

| Herramienta | ¿Obligatoria? | Para qué |
| --- | --- | --- |
| **git** | Sí | leer el historial (ya lo tienes si tienes repos) |
| **gh** ([cli.github.com](https://cli.github.com)) | Opcional | leer PRs e issues de GitHub (`chrono prs`, tickets) |
| **Rust (cargo)** | Solo para compilar | no hace falta si usas un binario precompilado |

chrono **no** necesita base de datos, servidor ni conexión (salvo `gh` para PRs). El índice es un archivo local.

---


## Como plugin de Claude Code

chrono se instala como plugin de Claude Code: tres skills (historia de git con `explain`, datos
masivos, CI/CD) y el servidor `chrono mcp`, para que el agente lea lo que chrono expone en vez de
logs crudos y `git log`.

```text
/plugin install chrono --marketplace juan52878911/chrono
```

(En versiones antiguas de Claude Code: `/plugin marketplace add juan52878911/chrono` y después `/plugin install chrono@chrono`.)

El plugin necesita el binario `chrono` (port Rust ≥ 0.2) en el `PATH`:

```bash
cargo install --git https://github.com/juan52878911/chrono chrono-cli --locked
```

Después, dentro de un repo: `chrono init`. Las skills le dicen al agente cuándo compensa chrono y cuándo no.

## Instalación

### Opción A — Una sola línea (recomendada, macOS y Linux)

Detecta tu plataforma, descarga el binario correcto del último release, **verifica su SHA-256** e instala en un directorio de tu `PATH`:

```bash
curl -fsSL https://raw.githubusercontent.com/juan52878911/chrono/main/install.sh | sh
```

Sin clonar, sin compilador. Funciona en macOS (Apple Silicon e Intel) y Linux (x86-64 y arm64).

- Fijar una versión: `curl -fsSL https://raw.githubusercontent.com/juan52878911/chrono/main/install.sh | VERSION=v0.1.0 sh`
- Elegir el directorio de instalación: `... | PREFIX="$HOME/.local/bin" sh`
- Por defecto instala en `/usr/local/bin` si tiene permisos; si no, en `~/.local/bin` (el script te avisa si hay que añadirlo al `PATH`).

### Opción B — Binario precompilado (manual)

Descarga el `.tar.gz` de tu plataforma desde la [página de Releases](https://github.com/juan52878911/chrono/releases/latest) y colócalo en tu `PATH`:

**macOS (Apple Silicon / M1–M4):**
```bash
tar -xzf chrono-v0.1.0-darwin-arm64.tar.gz
sudo mv chrono-v0.1.0-darwin-arm64/chrono /usr/local/bin/
```
**macOS (Intel):** usa `...-darwin-amd64.tar.gz`.

**Linux x86-64 (Debian/Ubuntu, Arch, …):**
```bash
tar -xzf chrono-v0.1.0-linux-amd64.tar.gz
sudo install -m755 chrono-v0.1.0-linux-amd64/chrono /usr/local/bin/chrono
```
> El binario de Linux es **estático** (sin dependencias de sistema): el mismo archivo vale para Debian, Ubuntu, Arch, Alpine, etc.

Desde un checkout también puedes ejecutar `./install.sh` (el mismo script; usa `./dist` si existe).

### Opción C — Desde código (con Rust)
```bash
make install     # instala en ~/.cargo/bin
```

### Opción D — Nix
```bash
nix run   github:juan52878911/chrono
nix profile install github:juan52878911/chrono
```
El paquete Nix envuelve `git` y `gh` automáticamente.

---

## Uso en 30 segundos

```bash
cd tu-repo
chrono init                       # crea .chrono/ e indexa (git-ignored)
chrono hotspots                   # ficheros que más cambian y más pesan
chrono coupling ruta/al/fichero   # qué cambia junto a él
chrono owners backend/            # propiedad por autor + bus factor
chrono bugs --since 2025-01-01    # categorías de bug + zonas calientes
chrono search "manejo de webhooks"  # busca commits por significado
chrono sync                       # actualiza solo lo nuevo (rapidísimo)
```

El índice se **auto-descubre** subiendo desde el directorio actual, como git con `.git`.

---

## Comandos

| Comando | Responde |
| --- | --- |
| `init [repo]` | Prepara el índice (crea `.chrono/`) e ingiere todo. |
| `sync [repo]` | Procesa solo el delta desde la última vez. |
| `hotspots` | ¿Qué es peligroso tocar? (frecuencia × tamaño) |
| `coupling <fichero>` | ¿Qué se rompe si toco esto? (acoplamiento temporal) |
| `owners <ruta>` | Propiedad por autor y **bus factor**. |
| `bugs` | Dónde se concentran los fixes + **categorías** de bug. |
| `churn` | Líneas +/− por fichero. |
| `tickets <id>` | Commits, ficheros y PRs de un ticket. |
| `prs` | Pull requests del forge (estado, merge, si es bug por label). |
| `branches [base]` | Estado de ramas vs base: ahead/behind, mergeada, stale, autores. |
| `phases` | Fases del proyecto (etiquetas/releases). |
| `search <texto>` | Busca commits por significado (texto completo). |
| `similar <sha>` | Commits casi-duplicados (por huella SimHash). |
| `explain <fichero>[:A-B]` | ¿Por qué este código está así? Commits que escribieron esas líneas (`git blame` en la revisión indexada), certeza de arreglo (`yes`/`maybe`), tickets/PRs, autores y qué cambia con esas líneas (también funciones con `init --symbols`). Determinista. |
| `mcp` | Servidor MCP por stdio (para una IA). |

Opciones: `--db RUTA`, `--since FECHA`, `--lang en|es`.

---

## Sesiones con agentes (Nod) — port Rust

[Nod](https://github.com/juan52878911/nod) mide el **proceso** de las sesiones con agentes en Claude Code y OpenCode (tokens, costo,
permisos, esperas, commits); chrono mide el **resultado** en git. El port Rust (`rust/`) une los dos:

```sh
chrono add ~/.local/state/nod/events.jsonl --as nod   # fuente nod (incremental con chrono sync)
chrono session_patterns                               # por tarea y por sesión
chrono trace <sha>                                    # la sesión de Nod que creó un commit
```

`session_patterns` da, por tarea y por sesión: tokens y costo, tiempo del agente frente al tiempo esperando al
humano, permisos negados y por qué tool, interrupciones y bucles, commits entregados (`vcs.commit`) y cuántos tocan
hotspots, retrabajo a 30 días, cambios incompletos, si la rama o el PR se mergeó, tasa de merge y costo por commit.
También como tool MCP. Definición de cada métrica y de cuándo no se puede calcular: [`docs/NOD.md`](docs/NOD.md).

---

## Con una IA (opencode / Claude Code)

chrono habla **MCP**. Así una IA responde sobre tu historial leyendo resúmenes de ~1.000 tokens en vez de miles de commits.

**opencode** (`~/.config/opencode/opencode.json`):
```json
{ "mcp": { "chrono": { "type": "local", "command": ["chrono", "mcp"], "enabled": true } } }
```
**Claude Code** (`.mcp.json` del proyecto):
```json
{ "mcpServers": { "chrono": { "command": "chrono", "args": ["mcp"] } } }
```
El servidor se lanza bajo demanda y muere con la sesión (serverless). Auto-descubre el índice por el directorio de trabajo. Si un repo aún no tiene índice, las tools responden *"ejecuta chrono init"* en vez de fallar.

---

## Configuración (opcional)

`chrono init` crea `.chrono/config.json`, versionable y ajustable por repo: `fix_keywords`, `ticket_patterns`, `exclude_globs`, `bug_labels`, y `bug_categories` (taxonomía `categoría → palabras clave` para clasificar los fixes).

---

## Idioma

Los mensajes están en **inglés por defecto**. chrono cambia a español si tu locale es español (`LANG`/`LC_*`), o con `CHRONO_LANG=es`, o `--lang es`. La salida JSON no se traduce (es el contrato).

---

## Cómo funciona

- **Determinista:** las respuestas salen de SQL sobre un índice fijo → misma pregunta, misma respuesta, con manifiesto reproducible.
- **Acotado:** cada respuesta es un JSON pequeño con presupuesto de tokens.
- **Eficiente:** binario ~6.7 MB; índice de pocos MB; `sync` incremental en milisegundos; sin procesos en segundo plano.
- **Local y privado:** todo vive en `.chrono/`. El forge (`gh`) solo se consulta si está, y prefiere tu remoto `upstream` en los forks.

Más detalle en [`docs/`](docs/).

---

## Licencia

MIT — ver [LICENSE](LICENSE).
