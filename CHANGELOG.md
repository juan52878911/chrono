# Cambios

## 0.7.0

Cierra los pendientes de 0.4.0–0.6.0. El índice no cambia de esquema (v7): no hace falta reindexar.

**Nuevo**
- `chrono reindex`: rehace el índice con las mismas fuentes (git, logs, globs, comandos). Es lo que propone el
  error de un índice de otro esquema. Si se corta, se vuelve a lanzar y reanuda desde la copia.
- `events --order desc` (MCP `order=desc`): lo más reciente primero.
- `top <dim>` sin `attr:` para una dim declarada (`top cwd`); una dim desconocida lista las que hay.
- `gaps --any-order` y `--until`; `grep=` con espacios en `--from`/`--to`.
- `-h`/`--help` en cualquier subcomando, sugerencia ante un comando mal escrito y aviso de flags desconocidas o
  que no aplican.
- `hints` en el resultado (y `⚠` en la cabecera MCP) cuando un vacío tiene explicación: attr o nivel que no
  existe, `stats` sin valores numéricos, `hotspots` sin git, eventos sin tiempo…

**Cambia**
- `gaps` solo da por cubierta una unidad con un `to` en o después de su primer `from` (antes, en cualquier
  orden; `--any-order` lo recupera). Cuenta aparte `from_without_key` y `units_without_time`.
- `precede`: el lift se calcula sobre las ventanas con actividad de la fuente (`active_episodes`), no sobre todo
  el periodo; en fuentes a ráfagas, como las sesiones de Claude, baja mucho (4,74 → 0,96 en un caso medido). Un
  ancla con varios valores de `--join` cuenta en cada uno; `anchors_without_key`, `anchors_without_time` y
  `precursors_total` dicen lo que queda fuera.
- Cambiar las opciones de una fuente (`entity_via`, `explode`, `attrs`, `links`…) la reindexa en el siguiente
  `sync`, como las dims. Si parte de la fuente ya no se puede releer, no se reindexa: avisa y `sources` cuenta los
  eventos que conservan las de antes (`stale_dims_events`).
- `--join`, `--via` y `--by` con una clave que no existe dan error en vez de vacío.

**Arreglado**
- Un fichero de una glob reescrito con el mismo tamaño y fecha, o editado en medio, se relee.
- `sync` y `tail` siguen con las demás fuentes si una falla; `sources` enseña el `sync_error`.
- `init` ya no borra las fuentes añadidas con `add` ni un índice que no puede abrir (uno dañado queda como
  `.corrupt.bak`); funciona en un repo sin commits y explica una rama huérfana.
- `tail` deja el índice como un `sync` (plantillas, rollups, búsqueda) y no pierde eventos del mismo instante.
- Dos procesos que abren o migran el mismo índice a la vez ya no fallan; una consulta sobre una ruta sin índice no
  crea un `.db` vacío; el cerrojo del hook de Loki no se queda bloqueado si el proceso muere.
- `search --limit`, `informe --level`, el `total` de `stats` por MCP, attrs con punto (`message.model`) y
  `patterns` con solo fuente o texto (ahora exacto). `--compact` usa el alias de la fuente.
