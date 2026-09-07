#!/usr/bin/env bash
# Read-only admission shared by artifact workflows; never dispatch or merge here.
set -euo pipefail
source_sha=${1:?usage: wait-release-required-checks.sh SOURCE_SHA current-main|main-history}
policy=${2:?missing source policy}
[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid release source SHA' >&2; exit 1; }
[[ $policy == current-main || $policy == main-history ]] || { echo 'invalid release source policy' >&2; exit 1; }
: "${GITHUB_REPOSITORY:?missing repository}"
defer_pending=${RELEASE_DEFER_PENDING:-false}
[[ $defer_pending == true || $defer_pending == false ]] || { echo 'invalid defer policy' >&2; exit 1; }
timeout=${RELEASE_CHECK_TIMEOUT_SECONDS:-900}
[[ $timeout =~ ^[0-9]+$ ]] || { echo 'invalid release timeout' >&2; exit 1; }
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
deadline=$((SECONDS + timeout))
while :; do
  main_sha=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq '.object.sha')
  [[ $main_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid main identity' >&2; exit 1; }
  if [[ $source_sha != "$main_sha" ]]; then
    [[ $policy == main-history ]] || { echo 'release source is no longer current main' >&2; exit 1; }
    relationship=$(gh api "repos/$GITHUB_REPOSITORY/compare/$source_sha...$main_sha" --jq '.status')
    [[ $relationship == ahead || $relationship == identical ]] || { echo 'tagged source is outside main history' >&2; exit 1; }
  fi
  gh api --paginate --slurp \
    "repos/$GITHUB_REPOSITORY/commits/$source_sha/check-runs?filter=latest&per_page=100" > "$work/checks.json"
  "$script_dir/read-release-required-checks.sh" "$work/checks.json" "$work/states"
  pending=false
  for check in monorepo prediction security; do
    state=$(sed -n "s/^${check}_conclusion=//p" "$work/states")
    case "$state" in
      success) ;;
      missing|queued|in_progress|waiting|pending|requested) pending=true ;;
      *) printf 'release rejected: %s=%s\n' "$check" "$state" >&2; exit 1 ;;
    esac
  done
  [[ $pending == true ]] || { printf 'release admission passed: %s\n' "$source_sha"; exit 0; }
  if [[ $defer_pending == true ]]; then
    echo 'release deferred until the remaining CI workflows complete' >&2
    exit 75
  fi
  [[ $SECONDS -lt $deadline ]] || { echo 'release checks did not succeed before deadline' >&2; exit 1; }
  sleep 30
done
