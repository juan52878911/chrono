#!/usr/bin/env bash
# Jobs y pasos de las últimas N ejecuciones de GitHub Actions, como NDJSON: una
# línea por job, con sus `steps` (cada uno con `duration_s`) y los datos de la
# ejecución (workflow, sha, rama). chrono los expande con `explode: steps`.
#
#   gh-steps.sh OWNER/REPO [N=60]
#
# Una llamada a la API por ejecución: N acota el coste. Las ya vistas no se
# repiten en chrono (el caché va por id), pero el script sí las vuelve a pedir;
# baja N para un sync frecuente.
set -euo pipefail
repo="${1:?uso: gh-steps.sh OWNER/REPO [N]}"
n="${2:-60}"
gh run list --repo "$repo" --limit "$n" --status completed \
  --json databaseId,workflowName,headSha,headBranch,event,conclusion,displayTitle |
jq -c '.[]' |
while read -r run; do
  id=$(jq -r .databaseId <<<"$run")
  gh api "repos/$repo/actions/runs/$id/jobs?per_page=100" --jq '.jobs[]' 2>/dev/null |
  jq -c --argjson run "$run" '
    def secs(a; b): if (a and b) then ((b | fromdateiso8601) - (a | fromdateiso8601)) else null end;
    {
      job_id: .id, job: .name, started_at, completed_at, conclusion,
      duration_s: secs(.started_at; .completed_at),
      run_id: $run.databaseId, workflow: $run.workflowName, head_sha: $run.headSha,
      branch: $run.headBranch, event: $run.event, run_title: $run.displayTitle,
      steps: [ .steps[]? | {
        step: .name, number, conclusion, started_at, completed_at,
        duration_s: secs(.started_at; .completed_at)
      } | .step_id = "\($run.databaseId)-\(.number)" ]
    }'
done
