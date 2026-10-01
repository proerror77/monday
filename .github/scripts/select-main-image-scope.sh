#!/usr/bin/env bash
# Carry unpublished impact through superseding main commits. A per-commit diff
# would lose a live change when its publication is superseded by a paper change.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
head=${1:?} output=${2:?}
metadata=${3:-}
base=$(bash "$root/read-published-image-source.sh" "$head")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# Workflow policy/history is not an input to these runtime images. Filtering
# it also avoids needing retired CI path mappings to plan cumulative image work.
git diff --no-renames --name-only "$base" "$head" -- rust_hft/ deploy/ .dockerignore .cargo/ >"$tmp/paths"
args=()
[[ -z $metadata ]] || args+=(--metadata "$metadata")
GITHUB_REF=refs/heads/main bash "$root/select-rust-ci-scope.sh" --event push --base "$base" --head "$head" \
  --changed-files "$tmp/paths" --output "$tmp/plan" "${args[@]}"
accumulated=$(sed -n 's/^image_matrix=//p' "$tmp/plan")
current=${SELECTED_IMAGE_MATRIX:-'{"include":[]}'}
# Only hft-core is published to this registry tag. Other images retain their
# per-change checks; a paper-only edit must not rebuild forever while core stays
# at its older (still correct) published revision.
matrix=$(jq -cn --argjson current "$current" --argjson accumulated "$accumulated" \
  '{include:($current.include + [$accumulated.include[]|select(.name=="hft-core")] | unique_by(.name))}')
jobs=${SELECTED_SECURITY_JOBS:-,,}
[[ $jobs =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,$ ]] || exit 2
jobs=$(jq -nr --arg jobs "$jobs" --argjson matrix "$matrix" '
  ($jobs|split(",")|map(select(.!="" and .!="security/container-scan"))) +
  (if ($matrix.include|length)>0 then ["security/container-scan"] else [] end) |
  "," + join(",") + ","')
printf 'image_base_sha=%s\nimage_matrix=%s\nsecurity_jobs=%s\n' "$base" "$matrix" "$jobs" >>"$output"
