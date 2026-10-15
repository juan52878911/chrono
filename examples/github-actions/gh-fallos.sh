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
# Patrones de fallo, también en español y con los símbolos que usan los smoke tests (✗, ❌).
patron='(FAILED|ERROR|Error|error:|Traceback|Exception|AssertionError|assert |exit code|failed|FATAL|panicked|✗|❌|FALL[OÓ]|[Ff]all[oó]|[Ff]allid|en rojo)'
gh run list --repo "$repo" --limit "$n" --status failure --json databaseId,workflowName,headSha |
jq -c '.[]' |
while read -r run; do
  id=$(jq -r .databaseId <<<"$run")
  # Las líneas que casan con el patrón y, además, las 6 últimas de cada paso fallido
  # (ahí suele estar el motivo aunque no use ninguna palabra del patrón).
  gh run view "$id" --repo "$repo" --log-failed 2>/dev/null |
  sed -E 's/\x1b\[[0-9;]*m//g' |
  awk -F'\t' -v pat="$patron" '
    { k = $1 FS $2; n[k]++; buf[k, n[k]] = $0; if ($0 ~ pat) keep[k, n[k]] = 1; order[k] = order[k] ? order[k] : ++nk; name[nk] = k }
    END { for (i = 1; i <= nk; i++) { k = name[i]; for (j = 1; j <= n[k]; j++) if (keep[k, j] || j > n[k] - 6) print buf[k, j] } }' |
  grep -vE 'DeprecationWarning|PydanticDeprecated|deprecated' |
  jq -R -c --argjson run "$run" '
    split("\t") as $p | select(($p | length) >= 3) |
    ($p[2] | capture("^(?<ts>\\S+Z) ?(?<msg>.*)$")) as $m |
    { ts: $m.ts, msg: $m.msg, job: $p[0], step: $p[1],
      level: (if ($m.msg | test("FAILED|Error|ERROR|Traceback|Exception|exit code|FATAL|panicked")) then "error" else "warning" end),
      run_id: $run.databaseId, workflow: $run.workflowName, head_sha: $run.headSha }'
done
