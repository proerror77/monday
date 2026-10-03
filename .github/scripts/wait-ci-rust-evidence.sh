#!/usr/bin/env bash
# Consume one native CI producer. No cloud dispatch, resource lease or receipt
# branch exists here. Read run, attempt, job and PR source independently.
set -euo pipefail
cd "$(dirname "$0")/../.."
source .github/scripts/verify-ci-rust-evidence.sh
checkout=${1:?actual checkout SHA required}
[[ $checkout =~ ^[0-9a-f]{40}$ ]]
case $GITHUB_EVENT_NAME in push|pull_request) ;; *) exit 2 ;; esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
head=$(jq -er '.pull_request.head.sha // .after' "$GITHUB_EVENT_PATH")
base=$(jq -er '.pull_request.base.sha // .before' "$GITHUB_EVENT_PATH")
source_repo=$(jq -er '.pull_request.head.repo.full_name // .repository.full_name' "$GITHUB_EVENT_PATH")
number=$(jq -r '.pull_request.number // 0' "$GITHUB_EVENT_PATH")
bash .github/scripts/select-rust-ci-scope.sh --event "$GITHUB_EVENT_NAME" --base "$base" --head "$head" --output "$work/scope"
timeout=${CI_EVIDENCE_TIMEOUT_SECONDS:-5400}
[[ $timeout =~ ^[0-9]+$ ]] && ((timeout>=60 && timeout<=7200))
deadline=$(( $(date +%s)+timeout ))
while (( $(date +%s)<deadline )); do
  gh api --method GET "repos/$GITHUB_REPOSITORY/actions/workflows/ci.yml/runs" -f head_sha="$head" -f event="$GITHUB_EVENT_NAME" -f per_page=100 >"$work/runs"
  producer=$(jq -r --arg head "$head" --arg base "$base" --arg source "$source_repo" --arg event "$GITHUB_EVENT_NAME" --argjson number "$number" '[.workflow_runs[]|select(.head_sha==$head and .event==$event and .head_repository.full_name==$source and .path==".github/workflows/ci.yml" and ($event!="pull_request" or any(.pull_requests[];.number==$number and .head.sha==$head and .base.sha==$base)))]|sort_by(.id)|last|.id//empty' "$work/runs")
  if [[ $producer =~ ^[1-9][0-9]*$ ]]; then
    gh api "repos/$GITHUB_REPOSITORY/actions/runs/$producer" >"$work/run"
    attempt=$(jq -er '.run_attempt|select(type=="number" and .>0 and floor==.)' "$work/run")
    gh api "repos/$GITHUB_REPOSITORY/actions/runs/$producer/attempts/$attempt/jobs?per_page=100" >"$work/jobs"
    state=$(jq -r '.jobs|map(select(.name=="Rust Workspace"))|if length==1 then .[0]|if .status=="completed" then .conclusion else "pending" end else "pending" end' "$work/jobs")
    if [[ $state == success ]]; then
      name="ci-rust-evidence-$checkout-$attempt"
      gh api "repos/$GITHUB_REPOSITORY/actions/runs/$producer/artifacts?per_page=100" >"$work/artifacts"
      artifact=$(jq -r --arg name "$name" --arg sha "$head" '[.artifacts[]|select(.name==$name and .expired==false and .workflow_run.head_sha==$sha)]|if length==1 then .[0].id else empty end' "$work/artifacts")
      if [[ $artifact =~ ^[1-9][0-9]*$ ]]; then
        gh api "repos/$GITHUB_REPOSITORY/actions/artifacts/$artifact/zip" >"$work/evidence.zip"
        [[ $(wc -c <"$work/evidence.zip") -le 2097152 ]]
        [[ $(unzip -Z1 "$work/evidence.zip") == rust-batch.json ]]
        unzip -p "$work/evidence.zip" rust-batch.json >"$work/receipt"
        gh api "repos/$GITHUB_REPOSITORY/actions/runs/$producer" >"$work/current"
        gh api "repos/$GITHUB_REPOSITORY/actions/runs/$producer/attempts/$attempt/jobs?per_page=100" >"$work/jobs"
        ci_verify_stages "$work/receipt" "$work/scope"
        ci_verify_producer "$work/receipt" "$work/current" "$work/jobs" "$GITHUB_EVENT_PATH" "$checkout"
        command_sha=$(cat .github/workflows/ci.yml .github/scripts/write-ci-rust-evidence.sh .github/scripts/verify-ci-rust-evidence.sh | sha256sum | awk '{print $1}')
        jq -e --arg sha "$command_sha" '.command_sha256==$sha' "$work/receipt" >/dev/null
        if [[ $GITHUB_EVENT_NAME == pull_request ]]; then
          gh api "repos/$GITHUB_REPOSITORY/pulls/$number" >"$work/current-pr"
          jq -e --arg head "$head" --arg base "$base" --arg source "$source_repo" '.state=="open" and .head.sha==$head and .base.sha==$base and .head.repo.full_name==$source' "$work/current-pr" >/dev/null
        fi
        printf 'Strict Clippy verified from CI run %s attempt %s checkout %s\n' "$producer" "$attempt" "$checkout"
        exit 0
      fi
    elif [[ $state != pending ]]; then
      printf 'Producing Rust job ended %s\n' "$state" >&2; exit 1
    fi
  fi
  sleep 15
done
echo 'No exact-source CI Rust evidence before deadline' >&2
exit 1
