#!/bin/sh
# Imprime la ruta de un binario `chrono` reciente (≥ $CHRONO_MIN) y, si no hay
# ninguno, lo instala desde la release de GitHub (install.sh verifica SHA256)
# en ~/.local/bin, sin sudo. Todo lo que no es la ruta va a stderr: lo usa el
# servidor MCP, cuyo stdout es el protocolo.
#
# Desactivar la instalación automática: CHRONO_AUTO_INSTALL=0.
set -eu

# Versión mínima que entiende lo que piden las skills y el MCP (filtros
# level/entity/until en top/patterns/events, tool `sources`). Un binario más
# viejo se ignora y se instala la release encima de ~/.local/bin.
CHRONO_MIN="0.2.5"

es_rust() { # `chrono 0.2.2` (o `0.2.2-dev`) ≥ CHRONO_MIN; el Go v0.1 no cumple
  v=$("$1" --version 2>/dev/null | awk '{print $2}' | sed 's/-.*//')
  [ -n "$v" ] || return 1
  printf '%s\n%s\n' "$CHRONO_MIN" "$v" | awk -F. '
    NR==1 { a=$1*1000000+$2*1000+$3 }
    NR==2 { b=$1*1000000+$2*1000+$3; exit !(b>=a) }'
}

for c in "${CHRONO_BIN:-}" "$(command -v chrono 2>/dev/null || true)" \
         "$HOME/.local/bin/chrono" "$HOME/.cargo/bin/chrono" /usr/local/bin/chrono /opt/homebrew/bin/chrono; do
  if [ -n "$c" ] && [ -x "$c" ] && es_rust "$c"; then
    echo "$c"
    exit 0
  fi
done

if [ "${CHRONO_AUTO_INSTALL:-1}" = "0" ]; then
  echo "chrono: no encontrado y CHRONO_AUTO_INSTALL=0" >&2
  exit 1
fi

# Un solo instalador a la vez (varias sesiones pueden arrancar juntas).
lock="${TMPDIR:-/tmp}/chrono-install.lock"
i=0
until mkdir "$lock" 2>/dev/null; do
  i=$((i + 1)); [ "$i" -gt 120 ] && break; sleep 1
done
trap 'rmdir "$lock" 2>/dev/null || true' EXIT

if [ -x "$HOME/.local/bin/chrono" ] && es_rust "$HOME/.local/bin/chrono"; then
  echo "$HOME/.local/bin/chrono"; exit 0
fi
echo "chrono: instalando la última release en ~/.local/bin…" >&2
curl -fsSL https://raw.githubusercontent.com/juan52878911/chrono/main/install.sh \
  | PREFIX="$HOME/.local/bin" sh >&2
if [ -x "$HOME/.local/bin/chrono" ] && es_rust "$HOME/.local/bin/chrono"; then
  echo "$HOME/.local/bin/chrono"
  exit 0
fi
echo "chrono: la instalación no dejó un binario válido; instálalo a mano: cargo install --git https://github.com/juan52878911/chrono chrono-cli --locked" >&2
exit 1
