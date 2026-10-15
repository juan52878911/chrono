#!/bin/sh
# Hook del plugin (UserPromptSubmit / PostToolUse): si el índice de chrono del
# proyecto tiene logs (fuentes que no son git), se lo dice al modelo, con su
# ventana y sus niveles, para que cuente y rankee con chrono en vez de con las
# líneas truncadas de Loki/Grafana. Medido con MiniMax: sin este aviso elige
# chrono 1 de cada 6 veces aunque tenga la skill y el MCP.
#  - UserPromptSubmit: solo si la pregunta va de logs/errores (palabras clave).
#  - PostToolUse (tools de observabilidad): una vez por sesión.
# No dice nada si no hay índice o no tiene logs. Desactivar: CHRONO_HINT=0.
[ "${CHRONO_HINT:-1}" = "0" ] && exit 0
evento=$1
entrada=$(cat)

campo() { printf '%s' "$entrada" | sed -n "s/.*\"$1\" *: *\"\([^\"]*\)\".*/\1/p" | head -n1; }

if [ "$evento" = "UserPromptSubmit" ]; then
  # Solo el texto de la pregunta (la ruta del proyecto puede contener «grafana» o «logs»).
  campo prompt | grep -qiE 'log|error|fatal|warn|critic|excep|traza|trace|reinici|restart|ca[ií]d|crash|incident|loki|grafana|latenc|timeout|(^|[^0-9])5[0-9][0-9]([^0-9]|$)' || exit 0
fi
sesion=$(campo session_id)
if [ "$evento" = "PostToolUse" ]; then
  marca="${TMPDIR:-/tmp}/chrono-hint-${sesion:-x}"
  [ -e "$marca" ] && exit 0
fi

# Índice: del directorio de la sesión hacia arriba.
d=$(campo cwd); [ -n "$d" ] || d=${CLAUDE_PROJECT_DIR:-$PWD}
db=""
while [ -n "$d" ]; do
  [ -f "$d/.chrono/index.db" ] && { db="$d/.chrono/index.db"; break; }
  [ "$d" = "/" ] && break
  d=$(dirname "$d")
done
[ -n "$db" ] || exit 0
# Con intermediario configurado, él ya sustituye la salida de esas tools.
[ "$evento" = "PostToolUse" ] && [ -f "$(dirname "$db")/intermediario.json" ] && exit 0
bin=$(CHRONO_AUTO_INSTALL=0 "$(cd "$(dirname "$0")" && pwd)/ensure-chrono.sh" 2>/dev/null) || exit 0

# Fuentes con niveles que no son git: logs, CI, comandos.
logs=$("$bin" sources --compact --db "$db" 2>/dev/null | awk -F'\t' 'NR>2 && $2!="git" && $1!="" && $6!="" {
  printf "%s%s (%s eventos, %s → %s; %s)", (n++ ? "; " : ""), $1, $3, $4, $5, $6 }')
[ -n "$logs" ] || exit 0

aviso="chrono tiene indexados logs para esta pregunta: $logs. Para contar, rankear, ver patrones, cuándo pasó algo o qué lo precede, usa las tools MCP de chrono (sources, top, patterns, precede, timeline, events; si están diferidas, cárgalas con ToolSearch \"chrono\") con filtros level=FATAL,ERROR, entity=<servicio>, since/until: dan cifras exactas sobre todo lo indexado. Las tools de Loki/Grafana cortan en 100 líneas por consulta: úsalas solo para el detalle que chrono no tenga. Skill: chrono:chrono-datos."

if [ "$evento" = "PostToolUse" ]; then
  : > "$marca"
  esc=$(printf '%s' "$aviso" | sed 's/\\/\\\\/g; s/"/\\"/g')
  printf '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"%s"}}\n' "$esc"
else
  printf '%s\n' "$aviso"
fi
exit 0
