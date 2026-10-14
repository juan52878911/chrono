#!/bin/sh
# Servidor MCP del plugin: asegura el binario y arranca `chrono mcp` (que
# autodescubre .chrono/index.db desde el directorio del proyecto).
set -eu
bin=$("$(dirname "$0")/ensure-chrono.sh") || exit 1
exec "$bin" mcp
