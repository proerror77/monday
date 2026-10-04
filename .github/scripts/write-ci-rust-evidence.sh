#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
source .github/scripts/verify-ci-rust-evidence.sh
output=${1:?output required}
selected=$(mktemp)
trap 'rm -f "$selected"' EXIT
# Manual validation selects the full scope and binds both ends to its exact SHA.
head=$(jq -er '.pull_request.head.sha // .after // env.GITHUB_SHA' "$GITHUB_EVENT_PATH")
base=$(jq -er '.pull_request.base.sha // .before // env.GITHUB_SHA' "$GITHUB_EVENT_PATH")
source_repo=$(jq -er '.pull_request.head.repo.full_name // .repository.full_name' "$GITHUB_EVENT_PATH")
bash .github/scripts/select-rust-ci-scope.sh --event "$GITHUB_EVENT_NAME" --base "$base" --head "$head" --output "$selected"
scope=$(ci_expected_scope "$selected")
stages=$(jq -cn --argjson scope "$scope" --argjson steps "${CI_STEPS:?}" '["collector","loop","owning","handoff","json","ondo","control","focused","clippy_loop","clippy_handoff"]|map(. as $s|select(if $s=="owning" then $scope.owning_packages!=",," else $scope[$s]==true end)|{stage:$s,outcome:$steps[$s].outcome})')
[[ $GITHUB_RUN_ID =~ ^[1-9][0-9]*$ && $GITHUB_RUN_ATTEMPT =~ ^[1-9][0-9]*$ ]]
job_id=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" --jq '.jobs|map(select(.name=="Rust Workspace"))|if length==1 then .[0].id else error("ambiguous producer job") end')
command_sha=$(cat .github/workflows/ci.yml .github/scripts/write-ci-rust-evidence.sh .github/scripts/verify-ci-rust-evidence.sh rust_hft/scripts/cargo-scoped.sh rust_hft/scripts/workspace-metadata.sh rust_hft/workspaces.json | sha256sum | awk '{print $1}')
jq -n --arg repository "$GITHUB_REPOSITORY" --arg source "$source_repo" --arg event "$GITHUB_EVENT_NAME" --arg head "$head" --arg base "$base" --arg checkout "$(git rev-parse HEAD)" --arg run "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" --argjson job "$job_id" --arg command "$command_sha" --argjson scope "$scope" --argjson stages "$stages" '{schema:"monday.ci_rust_evidence.v1",repository:$repository,source_repository:$source,event:$event,head_sha:$head,base_sha:$base,checkout_sha:$checkout,run_id:$run,run_attempt:$attempt,job_id:$job,command_sha256:$command,scope:$scope,stages:$stages}' >"$output"
ci_verify_stages "$output" "$selected"

if [[ -n ${GITHUB_OUTPUT:-} ]]; then printf 'evidence=%s\n' "$(jq -c . "$output")" >>"$GITHUB_OUTPUT"; fi
