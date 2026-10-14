#!/bin/sh
# Hook SessionStart del plugin: deja chrono listo sin que nadie lo pida.
#  1. Asegura el binario (lo instala la primera vez).
#  2. Si la sesión está en un repo git: crea el índice (`chrono init`) si no
#     existe, o lo pone al día (`chrono sync`) si existe. En SEGUNDO PLANO y con
#     un cerrojo: la sesión no espera y dos sesiones no indexan a la vez.
# No escribe nada en stdout salvo la primera instalación (una línea), para no
# gastar contexto en cada sesión. Desactivar: CHRONO_AUTO=0.
[ "${CHRONO_AUTO:-1}" = "0" ] && exit 0
dir=$(cd "$(dirname "$0")" && pwd)
antes=$(command -v chrono 2>/dev/null || true)
bin=$("$dir/ensure-chrono.sh" 2>/dev/null) || exit 0
[ -z "$antes" ] && [ "$bin" = "$HOME/.local/bin/chrono" ] && [ ! -e "$HOME/.local/state/chrono/avisado" ] && {
  mkdir -p "$HOME/.local/state/chrono" && : > "$HOME/.local/state/chrono/avisado"
  echo "chrono se instaló en $bin (plugin chrono). Si \`chrono\` no está en el PATH del shell, usa esa ruta."
}

raiz=$(git -C "${CLAUDE_PROJECT_DIR:-$PWD}" rev-parse --show-toplevel 2>/dev/null) || exit 0
mkdir -p "$raiz/.chrono" 2>/dev/null || exit 0
lock="$raiz/.chrono/.auto.lock"
mkdir "$lock" 2>/dev/null || exit 0      # ya hay uno indexando
(
  trap 'rmdir "$lock" 2>/dev/null' EXIT
  cd "$raiz" || exit 0
  if [ -f .chrono/index.db ]; then
    "$bin" sync >/dev/null 2>&1
  else
    "$bin" init >/dev/null 2>&1
  fi
) </dev/null >/dev/null 2>&1 &
exit 0
