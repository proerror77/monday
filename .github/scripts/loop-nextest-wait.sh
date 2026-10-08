#!/usr/bin/env bash
# Wait until the archive job and all four shard jobs succeed, then check that
# the shard counts add up to the archive list.
set -euo pipefail

repo=${GITHUB_WORKSPACE:?}
# shellcheck source-path=SCRIPTDIR
# shellcheck source=loop-nextest-counts.sh
source "$repo/.github/scripts/loop-nextest-counts.sh"

run_id=${GITHUB_RUN_ID:?}
attempt=${GITHUB_RUN_ATTEMPT:?}
repo_name=${GITHUB_REPOSITORY:?}
deadline=$(( $(date +%s) + 180 * 60 ))
names=(
  "Loop nextest archive"
  "Loop nextest shard (1)"
  "Loop nextest shard (2)"
  "Loop nextest shard (3)"
  "Loop nextest shard (4)"
)

while true; do
  jobs=$(gh api --method GET "repos/${repo_name}/actions/runs/${run_id}/attempts/${attempt}/jobs?per_page=100")
  total=$(jq -er '.total_count' <<<"$jobs")
  count=$(jq -er '.jobs | length' <<<"$jobs")
  [[ $total == "$count" && $total -le 100 ]] || {
    printf 'jobs page is incomplete: total=%s returned=%s\n' "$total" "$count" >&2
    exit 1
  }
  pending=false
  pending_names=
  for name in "${names[@]}"; do
    row=$(jq -c --arg name "$name" '[.jobs[] | select(.name == $name)]' <<<"$jobs")
    rows=$(jq -r 'length' <<<"$row")
    if [[ $rows -eq 0 ]]; then
      pending=true
      pending_names+="$name missing; "
      continue
    fi
    [[ $rows -eq 1 ]] || { printf 'duplicate job named %s\n' "$name" >&2; exit 1; }
    status=$(jq -r '.[0].status' <<<"$row")
    conclusion=$(jq -r '.[0].conclusion // "null"' <<<"$row")
    case $conclusion in
      success) ;;
      null)
        if [[ $status == completed ]]; then
          printf '%s completed without a conclusion\n' "$name" >&2
          exit 1
        fi
        pending=true
        pending_names+="$name $status; "
        ;;
      *)
        printf 'loop nextest job %s ended %s\n' "$name" "$conclusion" >&2
        exit 1
        ;;
    esac
  done
  if [[ $pending == false ]]; then
    break
  fi
  printf 'LOOP_NEXTEST_WAIT %s\n' "$pending_names"
  [[ $(date +%s) -lt $deadline ]] || {
    printf 'loop nextest jobs did not finish before the wait deadline\n' >&2
    exit 1
  }
  sleep 15
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
artifact_tries=${LOOP_NEXTEST_ARTIFACT_TRIES:-6}
artifact_wait=${LOOP_NEXTEST_ARTIFACT_WAIT_SECONDS:-5}
artifact_names=(
  "loop-nextest-expected-${run_id}-${attempt}"
  "loop-nextest-shard-1-${run_id}-${attempt}"
  "loop-nextest-shard-2-${run_id}-${attempt}"
  "loop-nextest-shard-3-${run_id}-${attempt}"
  "loop-nextest-shard-4-${run_id}-${attempt}"
)
artifacts=
try=1
while [[ $try -le $artifact_tries ]]; do
  artifacts=$(gh api "repos/${repo_name}/actions/runs/${run_id}/artifacts?per_page=100")
  total=$(jq -r '.total_count' <<<"$artifacts")
  count=$(jq -r '.artifacts | length' <<<"$artifacts")
  [[ $total == "$count" && $total -le 100 ]] || {
    printf 'artifacts page is incomplete: total=%s returned=%s\n' "$total" "$count" >&2
    exit 1
  }
  missing=false
  for name in "${artifact_names[@]}"; do
    found=$(jq -r --arg name "$name" '[.artifacts[] | select(.name == $name and .expired == false)] | length' <<<"$artifacts")
    if [[ $found -ne 1 ]]; then
      missing=true
    fi
  done
  if [[ $missing == false ]]; then
    break
  fi
  if [[ $try -eq $artifact_tries ]]; then
    printf 'loop nextest artifacts are missing\n' >&2
    exit 1
  fi
  sleep "$artifact_wait"
  try=$((try + 1))
done
download_artifact() {
  local name=$1 dest=$2 id
  id=$(jq -er --arg name "$name" '
    [.artifacts[] | select(.name == $name and .expired == false) | .id]
    | if length == 1 then .[0] else error("artifact \($name) count \(length)") end
  ' <<<"$artifacts")
  gh api "repos/${repo_name}/actions/artifacts/${id}/zip" >"$work/${dest}.zip"
  mkdir -p "$work/$dest"
  unzip -q -o "$work/${dest}.zip" -d "$work/$dest"
}
download_artifact "loop-nextest-expected-${run_id}-${attempt}" expected
for shard_n in 1 2 3 4; do
  download_artifact "loop-nextest-shard-${shard_n}-${run_id}-${attempt}" "shard${shard_n}"
done
mkdir -p "$work/reports"
cp "$work/expected/expected-counts.json" "$work/reports/expected-counts.json"
for shard_n in 1 2 3 4; do
  cp "$work/shard${shard_n}/shard-${shard_n}.json" "$work/reports/shard-${shard_n}.json"
done
loop_nextest_verify_dir "$work/reports"
