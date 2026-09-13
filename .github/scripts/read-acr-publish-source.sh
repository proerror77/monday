#!/usr/bin/env bash
# Read-only automatic ACR admission. Pending CI is resumed by workflow completion.
set -euo pipefail
source_sha=${1:?expected source SHA}
current_run_id=${2:?expected current publisher run ID}
output=${3:-${GITHUB_OUTPUT:-/dev/stdout}}
: "${GITHUB_REPOSITORY:?missing repository}"
[[ $source_sha =~ ^[0-9a-f]{40}$ && $current_run_id =~ ^[1-9][0-9]*$ ]] || exit 1
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
main_sha=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq '.object.sha')
[[ $main_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid current main identity' >&2; exit 1; }
artifact_run_id=$current_run_id
binaries_conclusion=missing
smoke_conclusion=missing
finish() {
  printf '%s\n' "automation_state=$1" "main_sha=$main_sha" \
    "artifact_run_id=$artifact_run_id" "binaries_conclusion=$binaries_conclusion" \
    "smoke_conclusion=$smoke_conclusion" >> "$output"
  if [[ -f $work/states ]]; then cat "$work/states" >> "$output"; fi
  printf 'automatic ACR source %s: %s\n' "$source_sha" "$1" >&2
  exit 0
}
[[ $source_sha == "$main_sha" ]] || finish stale

gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/commits/$source_sha/check-runs?filter=latest&per_page=100" > "$work/checks.json"
"$script_dir/read-release-required-checks.sh" "$work/checks.json" "$work/states"
pending=false
while IFS='=' read -r check state; do
  case "$state" in
    success) ;;
    missing|queued|in_progress|waiting|pending|requested) pending=true ;;
    *) printf 'ACR release rejected: %s=%s\n' "$check" "$state" >&2; exit 1 ;;
  esac
done < "$work/states"
[[ $pending == false ]] || finish deferred

# The completing workflow can be Monorepo or Security. Its run ID is never the
# binary provenance: resolve the latest exact-source Prediction main-push run.
gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/actions/workflows/ploy-ci.yml/runs?head_sha=$source_sha&branch=main&event=push&per_page=100" > "$work/prediction.json"
jq -ec --arg source "$source_sha" --arg repo "$GITHUB_REPOSITORY" '
  [.[].workflow_runs[]? | select(.head_sha == $source and .head_branch == "main"
    and .event == "push" and .path == ".github/workflows/ploy-ci.yml"
    and .head_repository.full_name == $repo)] | sort_by(.id) | last // {}' \
  "$work/prediction.json" > "$work/run.json"
[[ $(jq -r '.id // empty' "$work/run.json") ]] || {
  echo 'green required checks have no authenticated exact-source Prediction producer' >&2; exit 1;
}
artifact_run_id=$(jq -er '.id' "$work/run.json")
attempt=$(jq -er '.run_attempt' "$work/run.json")
[[ $artifact_run_id =~ ^[1-9][0-9]*$ && $attempt =~ ^[1-9][0-9]*$ ]] || exit 1
[[ $(jq -er '.status' "$work/run.json") == completed ]] || finish deferred
[[ $(jq -er '.conclusion' "$work/run.json") == success ]] || {
  echo 'latest exact-source Prediction run did not succeed' >&2; exit 1;
}
gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/actions/runs/$artifact_run_id/attempts/$attempt/jobs?per_page=100" > "$work/jobs.json"
job_state() {
  jq -er --arg name "$1" --argjson run "$artifact_run_id" --argjson attempt "$attempt" '
    [.[].jobs[]? | select(.name == $name and .run_id == $run and .run_attempt == $attempt)]
    | if length != 1 then error("research job identity missing or duplicated")
      else .[0] | if .status == "completed" then .conclusion else .status end end' "$work/jobs.json"
}
binaries_conclusion=$(job_state 'Research image binaries')
smoke_conclusion=$(job_state 'Research image smoke')
case "$binaries_conclusion/$smoke_conclusion" in
  skipped/skipped) finish out_of_scope ;;
  success/success) ;;
  *) echo 'exact-source research binaries and smoke must both succeed' >&2; exit 1 ;;
esac

# Existing workflow concurrency serializes these reads with earlier publishers.
# A deferred (successful but no-op) workflow is not publication evidence. Only
# the dedicated marker after both image readbacks suppresses another wakeup.
gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/actions/workflows/acr-publish.yml/runs?head_sha=$source_sha&branch=main&status=success&per_page=100" > "$work/publishers.json"
jq -r --arg source "$source_sha" --arg repo "$GITHUB_REPOSITORY" --argjson current "$current_run_id" '
  [.[].workflow_runs[]? | select(.head_sha == $source and .head_branch == "main"
    and .path == ".github/workflows/acr-publish.yml" and .head_repository.full_name == $repo
    and (.event == "workflow_run" or .event == "workflow_dispatch")
    and .status == "completed" and .conclusion == "success" and .id < $current)]
  | sort_by(.id) | reverse | .[] | [.id,.run_attempt] | @tsv' "$work/publishers.json" > "$work/prior.tsv"
while IFS=$'\t' read -r prior_id prior_attempt; do
  [[ $prior_id =~ ^[1-9][0-9]*$ && $prior_attempt =~ ^[1-9][0-9]*$ ]] || exit 1
  gh api --paginate --slurp \
    "repos/$GITHUB_REPOSITORY/actions/runs/$prior_id/attempts/$prior_attempt/jobs?per_page=100" > "$work/prior-jobs.json"
  if jq -e --arg marker "Research release complete ($source_sha)" --argjson run "$prior_id" --argjson attempt "$prior_attempt" '[.[].jobs[]? | select(.name == $marker and .run_id == $run and .run_attempt == $attempt and .status == "completed" and .conclusion == "success")] | length == 1' "$work/prior-jobs.json" >/dev/null; then
    finish already_published
  fi
done < "$work/prior.tsv"

# Do not silently rebuild when the admitted producer artifact has expired.
gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/actions/runs/$artifact_run_id/artifacts?per_page=100" > "$work/artifacts.json"
jq -e --arg name "research-image-release-$source_sha" --arg source "$source_sha" --argjson run "$artifact_run_id" '
  [.[].artifacts[]? | select(.name == $name)] | length == 1 and
  (.[0] | .expired == false and .workflow_run.id == $run and .workflow_run.head_sha == $source)' \
  "$work/artifacts.json" >/dev/null || { echo 'exact-source research release artifact is missing, ambiguous or expired' >&2; exit 1; }
finish ready
