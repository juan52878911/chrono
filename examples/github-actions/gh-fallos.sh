#!/usr/bin/env bash
# Las líneas con CAUSA de los logs de las últimas N ejecuciones FALLIDAS, como
# NDJSON (una por línea de log): job, paso, hora, texto, ejecución y sha.
#
#   gh-fallos.sh OWNER/REPO [N=25]
#
# No vuelca el log entero (un fallo del CI de Aura son ~1.600 líneas y ~60 k
# tokens): se queda con lo que suele explicar el fallo —tests FAILED, errores,
# trazas, aserciones, «exit code»— y quita los códigos de color. Lo demás está
# en `gh run view <id> --log-failed` si hace falta.
set -euo pipefail
repo="${1:?uso: gh-fallos.sh OWNER/REPO [N]}"
n="${2:-25}"
patron='(FAILED|ERROR|Error|error:|Traceback|Exception|AssertionError|assert |exit code|failed|FATAL|panicked)'
gh run list --repo "$repo" --limit "$n" --status failure --json databaseId,workflowName,headSha |
jq -c '.[]' |
while read -r run; do
  id=$(jq -r .databaseId <<<"$run")
  gh run view "$id" --repo "$repo" --log-failed 2>/dev/null |
  sed -E 's/\x1b\[[0-9;]*m//g' |
  grep -E "$patron" |
  grep -vE 'DeprecationWarning|PydanticDeprecated|deprecated' |
  jq -R -c --argjson run "$run" '
    split("\t") as $p | select(($p | length) >= 3) |
    ($p[2] | capture("^(?<ts>\\S+Z) ?(?<msg>.*)$")) as $m |
    { ts: $m.ts, msg: $m.msg, job: $p[0], step: $p[1],
      level: (if ($m.msg | test("FAILED|Error|ERROR|Traceback|Exception|exit code|FATAL|panicked")) then "error" else "warning" end),
      run_id: $run.databaseId, workflow: $run.workflowName, head_sha: $run.headSha }'
done
