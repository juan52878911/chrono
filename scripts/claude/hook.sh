#!/bin/sh
# Hooks del intermediario: `chrono hook <post|read|after-read>` con el binario
# que asegura ensure-chrono.sh (sin instalarlo desde un hook: si no hay, no hace nada).
bin=$(CHRONO_AUTO_INSTALL=0 "$(cd "$(dirname "$0")" && pwd)/ensure-chrono.sh" 2>/dev/null) || { cat >/dev/null; exit 0; }
exec "$bin" hook "$@"
