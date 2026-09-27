# Contratos: cómo consumen chrono otras herramientas

> 🇬🇧 [English version](CONTRACTS.md). Si difieren, manda la inglesa.

chrono es un producto público propio (MIT, un binario). Otras herramientas lo
adoptan **por contrato, no por código**: ejecutan el binario y se apoyan en las
cuatro cosas de abajo. Lo que no esté aquí es detalle de implementación.

Los consumidores de hoy son **UltraMemory** (una pila de memoria para agentes,
que sirve chrono como una capa detrás del gateway MCP de kindling) y, a través
de ese gateway, el **plugin de kindling para Claude Code**. El contrato del
clasificador va en el otro sentido: chrono produce datos para **Chispa**, el
clasificador lineal de kindling.

Las dos implementaciones lo cumplen: el binario Go (el que distribuyen
`install.sh` y las releases, desde v0.2.0) y el port Rust de `rust/`.

## 1. Etiqueta de repo: `<nombre>@<rama>`

Toda respuesta sobre un repositorio lleva su etiqueta: `kindling@main`,
`kindling@claude/fix-x`.

- `nombre` es el directorio del repo **principal**: un worktree enlazado de
  `kindling` se etiqueta `kindling@<rama>`, no con el nombre de su carpeta.
- `rama` es la rama del árbol, leída de `.git/HEAD` (sin lanzar `git`). HEAD
  separado es `HEAD`. Un directorio que no es repo git lleva solo su nombre.
- La rama conserva su `/`. Cuando una etiqueta tiene que ser nombre de
  directorio, el **slug** cambia cada tramo fuera de `[A-Za-z0-9._-]` por `-` y
  recorta `-`/`.` (la regla de `ramas.py` de UltraMemory): `claude/fix-x` →
  `claude-fix-x`.

Es la misma convención `repo@rama` que UltraMemory usa con codegraph y
graphify, para que un agente cruce respuestas entre capas.

## 2. Clave de entidad: `ruta` o `ruta#símbolo`

Un fichero se nombra por su **ruta relativa a la raíz del repo**, con `/`:
`internal/mcp/mcp.go`. Un símbolo es `ruta#símbolo`; chrono solo indexa
símbolos con `init --symbols` (port Rust).

Donde se espera un fichero o una ruta (`file` de `coupling`, `path` de
`owners`, en la CLI y en MCP) la entrada se normaliza igual:

| Entrada | Queda |
| --- | --- |
| `src/a.go#Parse` | `src/a.go` — el `#símbolo` se ignora en respuestas a nivel de fichero |
| `./src/a.go` | `src/a.go` |
| `/abs/repo/src/a.go#x` | `src/a.go` — una ruta absoluta dentro del repo se hace relativa |
| `/otro/sitio.go` | igual (está fuera del repo) |

Así un agente puede pasar un id `ruta#símbolo` de codegraph tal cual. Única
excepción: en la CLI Rust, `--by symbol` respeta `fichero#func`, porque ahí el
símbolo *es* la entidad. Un fichero cuyo nombre lleve `#` no se puede consultar
con esta normalización.

## 3. MCP: el argumento opcional `repo`

Un solo proceso `chrono mcp` sirve muchos repos. Cada tool acepta un argumento
opcional `repo` (como `projectPath` en codegraph):

| `repo` | Se resuelve a |
| --- | --- |
| *(ausente)* | el índice que se encuentra subiendo desde el cwd del servidor, como siempre |
| `kindling` | `<raíz>/kindling` |
| `kindling@feature-x` | primero el directorio (o symlink) con el nombre de la etiqueta, `<raíz>/kindling@feature-x` (una `/` en la rama pasa a `-`; también se prueba el slug completo), apunte a lo que apunte; si no, `<raíz>/kindling` si está en esa rama, luego su worktree git de esa rama, luego `<raíz>/../ramas/kindling/<slug>` (la disposición de UltraMemory) |
| `/ruta/abs/al/arbol` | ese árbol |

- **Raíz**: `chrono mcp -repos DIR` (o `--repos DIR`) o la variable
  `CHRONO_REPOS`; gana la bandera. Sin raíz, `repo` acepta rutas absolutas y el
  nombre o la etiqueta del repo por defecto.
- **Nombres**: sin `/` ni `\`, sin empezar por `.`, nunca `..`: un nombre no
  sale de la raíz. Las rutas absolutas valen donde el proceso pueda leer; pon el
  servidor detrás de algo que autentique (el gateway de kindling lo hace).
- **Se siguen los symlinks.** UltraMemory ejecuta
  `chrono mcp -repos /data/chrono/arboles`, una carpeta de symlinks
  `kindling`, `kindling@<slug>`… a árboles con su propio índice.
- **Dónde vive el índice**: siempre en `<árbol>/.chrono/index.db`, creado con
  `chrono init <árbol>` como hasta ahora; cada árbol lleva el suyo. (`.chrono/`
  escribe su propio `.gitignore`; en un espejo en el que no se commitea, añade
  también `.chrono/` a `.git/info/exclude`.)
- **Los índices Go y Rust no son intercambiables** (esquema v1 frente a v2+):
  indexa y sirve una raíz con el mismo binario.
- Los índices se abren al primer uso y quedan abiertos mientras vive el proceso.

**Toda respuesta lleva la etiqueta**: el JSON de cada resultado tiene
`"repo": "<nombre>@<rama>"` en el primer nivel (en el repo por defecto, si el
índice conoce su `repo_path`). En el binario Go eso implica que los resultados
que eran listas van ahora dentro de un objeto con las mismas claves que la CLI
(`{"hotspots": [...]}`, `{"churn": [...]}`, `{"phases": [...]}`,
`{"pull_requests": [...]}`, `{"query", "results"}`, `{"of", "similar"}`).

**Los errores son resultados con `isError: true`** y una frase accionable
(p. ej. `no chrono index for gamma@main (...). Run: chrono init /data/repos/gamma`).
Sin `repo` y sin índice en el cwd se mantiene lo de antes: un texto pidiendo
`chrono init`.

**La tool `repos`** lista las entradas de la raíz que son árboles git o tienen
índice (sigue symlinks, salta los rotos), por nombre, con `repo` (la etiqueta),
`name` (lo que se pasa como `repo`), `path` e `indexed`.

## 4. Datos para Chispa: `chrono export-dataset`

chrono no entrena ni embebe un clasificador propio. El nivel aprendido es
**Chispa** (kindling: `kling ai chispa train|eval|predict|deploy`); chrono
exporta los commits etiquetados en el JSONL que lee `kling ai chispa train`:

```sh
chrono export-dataset --out commits.jsonl        # o stdout; --since FECHA, --body N (300)
kling ai chispa train -data commits.jsonl -o commits.chispa
```

Una línea por commit:
`{"text", "label", "fields": {"files","churn","ext","dir"}, "id", "repo", "time", "label_source"}`.

- `text`: el asunto **sin el prefijo conventional commit** (el prefijo es la
  etiqueta) y, en otra línea, hasta `--body` bytes del cuerpo sin trailers ni
  líneas con prefijo; corte en frontera UTF-8.
- `label`: la señal determinista de chrono, gana la primera: label de bug del
  forge (`label_source: "forge"`), revert, prefijo conventional, `fix_keywords`
  (`"rules"`). Los commits sin señal no se exportan.
- `fields`: los mismos que `tools/chispa-commits` de kindling.
- `id`, `repo`, `time`, `label_source`: claves que Chispa ignora; sirven para
  repartir y auditar. Orden del más antiguo al más reciente; textos repetidos
  se quedan en su primera aparición.

Verificado: sobre OpenWA (1.738 commits) los dos binarios exportan las mismas
1.726 filas y `kling ai chispa train` las lee tal cual.

**Siguiente paso, sin implementar:** con `CHRONO_CHISPA_MODEL=<m.chispa>`,
`init`/`sync` mandarían los commits sin señal dura a
`kling ai chispa predict -model M` por stdin y guardarían la etiqueta solo si
`decision` es `confident`, con `source: "chispa"`. Las reglas siguen por
defecto y de respaldo.

## Cómo lo usan UltraMemory y el plugin de kindling

1. UltraMemory mantiene sus árboles (espejos en `/data/repos`, worktrees de
   rama en `/data/ramas/<repo>/<slug>`) y corre `chrono init` / `chrono sync`
   en cada uno.
2. Apunta un servidor a una raíz de symlinks
   (`chrono mcp -repos /data/chrono/arboles`), envuelto en `mcp-proxy` y dado
   de alta en el gateway de kindling con `kling mcp link`.
3. Los agentes llegan por el gateway. Con el plugin de kindling para Claude
   Code, Claude usa `find_tools` / `describe_tool` / `call_tool` del gateway,
   p. ej. `coupling` con `{"repo": "kindling@main", "file": "pkg/aigw/gateway.go#Serve"}`,
   y cruza la respuesta con codegraph y graphify por la etiqueta y la clave.
4. `repos` dice qué etiquetas existen y cuáles aún piden `chrono init`.
5. Para clasificar, `chrono export-dataset` alimenta `kling ai chispa train`; la
   evaluación que decide si un modelo merece usarse es `kling ai chispa eval`.
