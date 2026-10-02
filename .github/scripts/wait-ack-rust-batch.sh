#!/usr/bin/env bash
# Security consumes the Monorepo batch's signed result. It never dispatches a
# second private compiler or relabels that producer as this workflow's run.
set -euo pipefail
source_sha=${1:?actual checkout SHA required}
destination=${2:-${RUNNER_TEMP:?}/ack-rust-batch}
[[ ${GITHUB_REPOSITORY:-} == proerror77/monday && $source_sha =~ ^[0-9a-f]{40}$ ]] || exit 2
case ${GITHUB_EVENT_NAME:-} in push|pull_request) ;; *) exit 2 ;; esac
number=0
if [[ $GITHUB_EVENT_NAME == pull_request ]]; then
  [[ $(jq -er '.pull_request.head.repo.full_name' "$GITHUB_EVENT_PATH") == "$GITHUB_REPOSITORY" ]] || {
    echo 'Fork research jobs require independent source admission; no hosted compiler fallback.' >&2
    exit 1
  }
  number=$(jq -er '.pull_request.number | select(type=="number" and .>0 and floor==.)' "$GITHUB_EVENT_PATH")
fi
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
  producer=$(jq -r --arg head "$head" --arg base "$base" --arg event "$GITHUB_EVENT_NAME" --argjson number "$number" '
    [.workflow_runs[] | select(.head_sha==$head and .event==$event and
      .head_repository.full_name=="proerror77/monday" and .path==".github/workflows/ci.yml" and
      ($event!="pull_request" or any(.pull_requests[]; .number==$number and .head.sha==$head and .base.sha==$base)))]
    | sort_by(.id) | last | .id // empty' "$destination/runs.json")
  if [[ $producer =~ ^[1-9][0-9]*$ ]]; then
    gh api "repos/proerror77/monday/actions/runs/$producer" >"$destination/producer.json"
    attempt=$(jq -er '.run_attempt | select(type=="number" and .>0 and floor==.)' "$destination/producer.json")
    # Keep discovering until this exact checkout/attempt has a result. A
    # reopened PR can briefly expose only the previous base's workflow run.
    receipt_base="https://raw.githubusercontent.com/proerror77/monday/codex/ack-ci-receipts/$producer/$attempt/rust/$source_sha"
    if curl --connect-timeout 10 --max-time 20 --max-filesize 262144 -fsS \
      "$receipt_base/receipt.json?time=$(date +%s)" -o "$destination/candidate.json" 2>/dev/null; then break; fi
    jq -e '.status!="completed" or .conclusion=="success"' "$destination/producer.json" >/dev/null || {
      echo 'Matching Monorepo run already failed or was cancelled' >&2; exit 1;
    }
  fi
  producer=
  sleep 15
done
[[ $producer =~ ^[1-9][0-9]*$ ]] || { echo 'No matching Monorepo Rust producer before deadline' >&2; exit 1; }
remaining=$((deadline - $(date +%s)))
((remaining >= 60)) || exit 1
GITHUB_RUN_ID=$producer GITHUB_RUN_ATTEMPT=$attempt GITHUB_JOB=rust \
  ACK_RECEIPT_TIMEOUT_SECONDS=$remaining \
  bash .github/scripts/wait-ack-research-receipt.sh ci-rust "$source_sha" "$destination"
# Independently re-read the producing attempt/job after the signed batch is
# consumed. The receipt's job ID must still belong to this exact source/run.
gh api "repos/proerror77/monday/actions/runs/$producer" >"$destination/current-run.json"
gh api "repos/proerror77/monday/actions/runs/$producer/attempts/$attempt/jobs?per_page=100" >"$destination/jobs.json"
source .github/scripts/verify-ack-rust-batch.sh
jq -e --arg head "$head" --arg base "$base" --arg run "$producer" --argjson attempt "$attempt" '
  .head_sha==$head and .base_sha==$base and .public_run_id==$run and .public_run_attempt==$attempt' "$destination/receipt.json" >/dev/null
ack_verify_rust_job "$destination/receipt.json" "$destination/current-run.json" "$destination/jobs.json"
if [[ $GITHUB_EVENT_NAME == pull_request ]]; then
  number=$(jq -er .pull_request.number "$GITHUB_EVENT_PATH")
  gh api "repos/proerror77/monday/pulls/$number" >"$destination/current-source.json"
  jq -e --arg head "$head" --arg base "$base" '.state=="open" and .head.sha==$head and .base.sha==$base and
    .head.repo.full_name=="proerror77/monday"' "$destination/current-source.json" >/dev/null
fi
printf 'Strict Clippy verified from Monorepo batch %s attempt %s at %s\n' "$producer" "$attempt" "$source_sha"
