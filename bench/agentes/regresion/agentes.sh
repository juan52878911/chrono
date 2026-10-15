#!/bin/sh
# Bench de agentes sobre el corpus: una sesión de Claude Code por llamada.
#   agentes.sh <brazo> <n> [salida]
#   brazo: sin-chrono | skill (datos ya indexados + plugin) | intermediario (plugin + intercepción de Read)
#   MOTOR=minimax (MiniMax por su API compatible; clave en MINIMAX_API_KEY o MINIMAX_API_KEY_CMD) | claude (cuenta de la máquina)
#   MODELO=MiniMax-M3 | claude-sonnet-5-5 | …     CHRONO_BIN=<binario a probar> (por defecto, el del PATH)
# Cada sesión corre en su propio directorio (no comparten índice) y deja <salida>/<brazo>-<n>.jsonl.
set -eu
here=$(cd "$(dirname "$0")" && pwd); repo=$(cd "$here/../../.." && pwd)
brazo=$1; n=$2; out=${3:-$here/resultados/$(date +%Y%m%d)}
motor=${MOTOR:-minimax}; bin=${CHRONO_BIN:-$(command -v chrono)}
mkdir -p "$out"; dir="$out/run-$brazo-$n"; rm -rf "$dir"; mkdir -p "$dir/bin"
gzip -dc "$here/corpus/logs-sandbox.jsonl.gz" > "$dir/logs-sandbox.jsonl"
ln -sf "$bin" "$dir/bin/chrono"
case $brazo in
  sin-chrono) echo '{"mcpServers":{}}' > "$dir/mcp.json"; plug="" ;;
  skill|intermediario)
    printf '{"mcpServers":{"chrono":{"command":"%s/scripts/claude/mcp.sh"}}}\n' "$repo" > "$dir/mcp.json"; plug="--plugin-dir $repo"
    mkdir -p "$dir/.chrono"
    if [ "$brazo" = skill ]; then (cd "$dir" && "$bin" add logs-sandbox.jsonl --db .chrono/index.db >/dev/null 2>&1); fi
    if [ "$brazo" = intermediario ]; then echo '{"enabled": true, "rules": [], "read": {"enabled": true, "min_bytes": 200000, "deny_bytes": 1000000}}' > "$dir/.chrono/intermediario.json"; fi ;;
  *) echo "brazo desconocido: $brazo" >&2; exit 2 ;;
esac
export CHRONO_BIN="$bin" CHRONO_AUTO_INSTALL=0 PATH="$dir/bin:$PATH" ENABLE_TOOL_SEARCH=true DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
if [ "$motor" = minimax ]; then
  export ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-https://api.minimax.io/anthropic}"
  export ANTHROPIC_AUTH_TOKEN="${MINIMAX_API_KEY:-$(sh -c "${MINIMAX_API_KEY_CMD:?falta MINIMAX_API_KEY o MINIMAX_API_KEY_CMD}")}"
  modelo=${MODELO:-MiniMax-M3}; export ANTHROPIC_MODEL="$modelo" ANTHROPIC_SMALL_FAST_MODEL="$modelo"
  export CLAUDE_CONFIG_DIR="$dir/cc"; mkdir -p "$CLAUDE_CONFIG_DIR"; unset ANTHROPIC_API_KEY
  sources=""
else
  modelo=${MODELO:-claude-sonnet-5-5}; sources="--setting-sources project"
fi
cd "$dir"
# shellcheck disable=SC2086
claude -p "$(cat "$here/pregunta.txt")" --model "$modelo" $sources $plug --strict-mcp-config --mcp-config "$dir/mcp.json" \
  --allowedTools Read Grep Glob mcp__chrono Skill "Bash(jq:*)" "Bash(grep:*)" "Bash(wc:*)" "Bash(head:*)" "Bash(tail:*)" \
     "Bash(sort:*)" "Bash(uniq:*)" "Bash(cut:*)" "Bash(awk:*)" "Bash(chrono:*)" "Bash(ls:*)" \
  --disallowedTools Write Edit WebFetch WebSearch Task NotebookEdit Agent \
  --permission-mode dontAsk --output-format stream-json --verbose --max-turns "${MAX_TURNS:-40}" \
  </dev/null > "$out/$brazo-$n.jsonl" 2> "$out/$brazo-$n.err" || true
