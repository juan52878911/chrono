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
| **Go 1.23+** | Solo para compilar | no hace falta si usas un binario precompilado |

chrono **no** necesita base de datos, servidor ni conexión (salvo `gh` para PRs). El índice es un archivo local.

---

## Instalación

### Opción A — Binario precompilado (lo más rápido)

Descarga el `.tar.gz` de tu plataforma (carpeta `dist/` o la sección *Releases*) y colócalo en tu `PATH`:

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

O deja que el script elija por ti (instala en `~/.local/bin`):
```bash
./install.sh
```

### Opción B — Desde código (con Go)
```bash
make install     # instala en ~/go/bin
```

### Opción C — Nix
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
| `phases` | Fases del proyecto (etiquetas/releases). |
| `search <texto>` | Busca commits por significado (texto completo). |
| `similar <sha>` | Commits casi-duplicados (por huella SimHash). |
| `mcp` | Servidor MCP por stdio (para una IA). |

Opciones: `--db RUTA`, `--since FECHA`, `--lang en|es`.

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
