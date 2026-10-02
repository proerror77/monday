#!/usr/bin/env bash
# Security consumes the Monorepo batch's signed result. It never dispatches a
# second private compiler or relabels that producer as this workflow's run.
set -euo pipefail
source_sha=${1:?actual checkout SHA required}
destination=${2:-${RUNNER_TEMP:?}/ack-rust-batch}
[[ ${GITHUB_REPOSITORY:-} == proerror77/monday && $source_sha =~ ^[0-9a-f]{40}$ ]] || exit 2
case ${GITHUB_EVENT_NAME:-} in push|pull_request) ;; *) exit 2 ;; esac
mkdir -p "$destination"
head=$(jq -er '.pull_request.head.sha // .after' "$GITHUB_EVENT_PATH")
base=$(jq -er '.pull_request.base.sha // .before' "$GITHUB_EVENT_PATH")
timeout=${ACK_RECEIPT_TIMEOUT_SECONDS:-18000}
[[ $timeout =~ ^[0-9]+$ ]] && ((timeout >= 60 && timeout <= 21600)) || exit 2
deadline=$(( $(date +%s) + timeout ))
producer=
while (( $(date +%s) < deadline )); do
  gh api --method GET repos/proerror77/monday/actions/workflows/ci.yml/runs \
    -f head_sha="$head" -f event="$GITHUB_EVENT_NAME" -f per_page=100 >"$destination/runs.json"
  producer=$(jq -r --arg head "$head" --arg event "$GITHUB_EVENT_NAME" '
    [.workflow_runs[] | select(.head_sha==$head and .event==$event and
      .head_repository.full_name=="proerror77/monday" and .path==".github/workflows/ci.yml")]
    | sort_by(.id) | last | .id // empty' "$destination/runs.json")
  if [[ -n $producer ]]; then break; fi
  sleep 15
done
[[ $producer =~ ^[1-9][0-9]*$ ]] || { echo 'No matching Monorepo Rust producer before deadline' >&2; exit 1; }
gh api "repos/proerror77/monday/actions/runs/$producer" >"$destination/producer.json"
attempt=$(jq -er '.run_attempt | select(type=="number" and .>0 and floor==.)' "$destination/producer.json")
jq -e '.status!="completed" or .conclusion=="success"' "$destination/producer.json" >/dev/null || {
  echo 'Matching Monorepo run already failed or was cancelled' >&2; exit 1;
}
remaining=$((deadline - $(date +%s)))
((remaining >= 60)) || exit 1
GITHUB_RUN_ID=$producer GITHUB_RUN_ATTEMPT=$attempt GITHUB_JOB=rust \
  ACK_RECEIPT_TIMEOUT_SECONDS=$remaining \
  bash .github/scripts/wait-ack-research-receipt.sh ci-rust "$source_sha" "$destination"
# Independently re-read the producing attempt/job after the signed batch is
# consumed. The receipt's job ID must still belong to this exact source/run.
gh api "repos/proerror77/monday/actions/runs/$producer" >"$destination/current-run.json"
gh api "repos/proerror77/monday/actions/runs/$producer/attempts/$attempt/jobs?per_page=100" >"$destination/jobs.json"
jq -e --arg head "$head" --arg event "$GITHUB_EVENT_NAME" --argjson attempt "$attempt" '
  .head_sha==$head and .event==$event and .run_attempt==$attempt and
  .head_repository.full_name=="proerror77/monday" and .path==".github/workflows/ci.yml" and
  (.status!="completed" or .conclusion=="success")' "$destination/current-run.json" >/dev/null
jq -e --slurpfile receipt "$destination/receipt.json" --arg head "$head" --arg base "$base" --argjson run "$producer" '
  $receipt[0] as $r | $r.head_sha==$head and $r.base_sha==$base and
  ([.jobs[]|select(.id==$r.public_job_id and .run_id==$run and .head_sha==$head and .name=="Rust Workspace" and
    ((.status=="in_progress" and .conclusion==null) or (.status=="completed" and .conclusion=="success")))] | length==1)
  ' "$destination/jobs.json" >/dev/null
if [[ $GITHUB_EVENT_NAME == pull_request ]]; then
  number=$(jq -er .pull_request.number "$GITHUB_EVENT_PATH")
  gh api "repos/proerror77/monday/pulls/$number" >"$destination/current-source.json"
  jq -e --arg head "$head" --arg base "$base" '.state=="open" and .head.sha==$head and .base.sha==$base and
    .head.repo.full_name=="proerror77/monday"' "$destination/current-source.json" >/dev/null
fi
printf 'Strict Clippy verified from Monorepo batch %s attempt %s at %s\n' "$producer" "$attempt" "$source_sha"
