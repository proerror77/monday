#!/usr/bin/env bash
# Resolve original CI source through authenticated native run and job metadata.
set -euo pipefail
[[ ${GITHUB_EVENT_NAME:-} == workflow_run ]] || { echo 'release source readback requires workflow_run' >&2; exit 1; }
: "${GITHUB_REPOSITORY:?missing repository}" "${GITHUB_EVENT_PATH:?missing event}"
output=${1:-${GITHUB_OUTPUT:-/dev/stdout}}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
release_id=$(jq -er '.workflow_run.id | select(type=="number" and .>0 and floor==.)' "$GITHUB_EVENT_PATH")
release_attempt=$(jq -er '.workflow_run.run_attempt | select(type=="number" and .>0 and floor==.)' "$GITHUB_EVENT_PATH")
[[ $release_id =~ ^[1-9][0-9]*$ && $release_attempt =~ ^[1-9][0-9]*$ ]]
gh api "repos/$GITHUB_REPOSITORY/actions/runs/$release_id/attempts/$release_attempt" >"$work/release.json"
jq -e --arg repo "$GITHUB_REPOSITORY" --argjson id "$release_id" --argjson attempt "$release_attempt" '
  .id==$id and .run_attempt==$attempt and .repository.full_name==$repo and .head_repository.full_name==$repo and
  .path==".github/workflows/release.yml" and .event=="workflow_run" and .head_branch=="main" and
  .status=="completed" and (.head_sha | type=="string" and test("^[0-9a-f]{40}$"))
' "$work/release.json" >/dev/null || { echo 'invalid Release run identity' >&2; exit 1; }
controller_sha=$(jq -er .head_sha "$work/release.json")
gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/actions/runs/$release_id/attempts/$release_attempt/jobs?per_page=100" >"$work/jobs.json"
marker=$(jq -er --argjson id "$release_id" --argjson attempt "$release_attempt" --arg head "$controller_sha" '
  if type!="array" or length==0 or (all(.[]; .jobs|type=="array")|not)
  then error("invalid Release jobs response") else . end |
  [.[].jobs[] | select(.name | type=="string" and startswith("Release source v1"))] |
  if length!=1 then error("missing or duplicate Release source marker") else .[0] end |
  if .run_id!=$id or .run_attempt!=$attempt or .head_sha!=$head or .status!="completed" or .conclusion!="success"
  then error("invalid Release source marker identity") else .name end |
  capture("^Release source v1 \\[(?<source>[0-9a-f]{40})\\] \\[(?<run>[1-9][0-9]*)/(?<attempt>[1-9][0-9]*)\\]$") |
  [.source,.run,.attempt] | @tsv
' "$work/jobs.json")
IFS=$'\t' read -r source_sha ci_id ci_attempt <<<"$marker"
[[ $source_sha =~ ^[0-9a-f]{40}$ && $ci_id =~ ^[1-9][0-9]*$ && $ci_attempt =~ ^[1-9][0-9]*$ ]]
gh api "repos/$GITHUB_REPOSITORY/actions/runs/$ci_id/attempts/$ci_attempt" >"$work/ci.json"
jq -e --arg repo "$GITHUB_REPOSITORY" --argjson id "$ci_id" --argjson attempt "$ci_attempt" --arg source "$source_sha" '
  .id==$id and .run_attempt==$attempt and .repository.full_name==$repo and .head_repository.full_name==$repo and
  (.path==".github/workflows/ci.yml" or .path==".github/workflows/ploy-ci.yml" or .path==".github/workflows/security-enabled.yml") and
  .event=="push" and .head_branch=="main" and .head_sha==$source and .status=="completed"
' "$work/ci.json" >/dev/null || { echo 'invalid original CI run identity' >&2; exit 1; }
# This routes source identity only. Existing current-main and artifact gates follow.
printf 'source_sha=%s\nsource_ci_run_id=%s\nsource_ci_run_attempt=%s\n' "$source_sha" "$ci_id" "$ci_attempt" >>"$output"
