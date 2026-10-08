#!/usr/bin/env bash
# Admit one main SHA for publication. Not-green checks exit 0 and publish nothing.
set +x
set -euo pipefail
source_sha=${1:?usage: release-orchestrator-admit.sh SOURCE_SHA}
[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid release source SHA' >&2; exit 1; }
: "${GITHUB_REPOSITORY:?missing repository}"
: "${GITHUB_OUTPUT:?missing output}"
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
printf 'ghcr=false\nacr=false\n' >"$work/published"
printf 'monorepo_conclusion=missing\nprediction_conclusion=missing\nsecurity_conclusion=missing\n' >"$work/states"

main_sha=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" | jq -er '.object.sha')
[[ $main_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid main identity' >&2; exit 1; }
if [[ $source_sha != "$main_sha" ]]; then
  SOURCE_SHA=$source_sha IS_CURRENT_MAIN=false \
    "$script_dir/decide-release-once.sh" "$work/states" "$work/published"
  exit 0
fi

if ! gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/commits/$source_sha/check-runs?filter=latest&per_page=100" \
  >"$work/checks.json"; then
  echo 'required check lookup failed' >&2
  exit 1
fi
"$script_dir/read-release-required-checks.sh" "$work/checks.json" "$work/states"
all_green=true
for check in monorepo prediction security; do
  state=$(sed -n "s/^${check}_conclusion=//p" "$work/states")
  [[ $state == success || $state == skipped ]] || all_green=false
done
if [[ $all_green == true ]]; then
  "$script_dir/read-release-published.sh" "$source_sha" "$work/published"
fi
SOURCE_SHA=$source_sha IS_CURRENT_MAIN=true \
  "$script_dir/decide-release-once.sh" "$work/states" "$work/published"
