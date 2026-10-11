#!/usr/bin/env bash
# CI builds bytes and provenance; it never acquires research compute or runs a
# Campaign. Release admission and authorized deployment remain separate.
set -euo pipefail
product=$(bash "$(dirname "${BASH_SOURCE[0]}")/research-release-products.sh" normalize "${1:-all}")
cd "$(dirname "$0")/../../rust_hft"
source_sha=$(../.github/scripts/research-release-source-sha.sh)
export MONDAY_RELEASE_JOB_ID
MONDAY_RELEASE_JOB_ID=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" --jq '.jobs|map(select(.name=="Research image binaries" or .name=="Research release binaries"))|if length==1 then .[0].id else error("ambiguous release producer") end')
export MONDAY_SOURCE_REVISION=$source_sha
export CARGO_TARGET_DIR="$PWD/target"
binary_path() { bash ../.github/scripts/research-cache-layout.sh binary-path "$product" "$1"; }
recipe_pid='' recipe_starting=0 recipe_cancelled=0
cancel_recipe() {
  recipe_cancelled=$1
  if [[ -n $recipe_pid ]]; then
    kill -TERM "$recipe_pid" 2>/dev/null || true
  elif (( recipe_starting == 0 )); then exit "$recipe_cancelled"; fi
}
trap 'cancel_recipe 130' INT
trap 'cancel_recipe 143' TERM
run_recipe_phase() {
  local result=0
  recipe_starting=1
  bash ../.github/scripts/build-research-recipes.sh "$product" "$1" &
  recipe_pid=$!
  recipe_starting=0
  if (( recipe_cancelled != 0 )); then kill -TERM "$recipe_pid" 2>/dev/null || true; fi
  while true; do
    if wait "$recipe_pid"; then break; else result=$?; fi
    if (( recipe_cancelled == 0 )) || ! kill -0 "$recipe_pid" 2>/dev/null; then break; fi
  done
  recipe_pid=''
  if (( recipe_cancelled != 0 )); then return "$recipe_cancelled"; fi
  return "$result"
}
binary_hashes() {
  while IFS= read -r binary; do sha256sum "$(binary_path "$binary")"; done \
    < <(bash ../.github/scripts/research-release-products.sh binaries "$product")
}
run_recipe_phase after-cache-lookup
if [[ ${MONDAY_RESEARCH_CACHE_PROBE:-0} == 1 ]]; then
  # Probe only the current runner's target. Never save a PR cache or change recipes.
  before=$(mktemp)
  after=$(mktemp)
  trap 'rm -f "$before" "$after"' EXIT
  binary_hashes >"$before"
  run_recipe_phase warm-local
  binary_hashes >"$after"
  diff -u "$before" "$after"
  # Both A/B phases rebuild local code against the same warmed dependencies.
  # Only scheduling differs; source, inputs, profile and output hashes must agree.
  probe_inputs_sha=$(sha256sum "$MONDAY_BUILD_INPUTS_FILE" | awk '{print $1}')
  bash ../.github/scripts/research-cache-layout.sh cleanup "$MONDAY_BUILD_INPUTS_FILE"
  MONDAY_RESEARCH_BUILD_CONCURRENCY=1 run_recipe_phase dependency-warm-serial
  binary_hashes >"$before"
  bash ../.github/scripts/research-cache-layout.sh cleanup "$MONDAY_BUILD_INPUTS_FILE"
  MONDAY_RESEARCH_BUILD_CONCURRENCY=2 run_recipe_phase dependency-warm-parallel
  binary_hashes >"$after"
  diff -u "$before" "$after"
  [[ $(sha256sum "$MONDAY_BUILD_INPUTS_FILE" | awk '{print $1}') == "$probe_inputs_sha" ]]
fi
release=${RUNNER_TEMP:?}/research-release
mkdir -p "$release/research-bin"
while IFS= read -r binary; do
  install -m 0755 "$(binary_path "$binary")" "$release/research-bin/$binary"
done < <(bash ../.github/scripts/research-release-products.sh binaries "$product")
../.github/scripts/verify-research-runtime-abi.sh "$release/research-bin" "$product"
../.github/scripts/research-image-release-artifact.sh create "$release" "$source_sha" "$GITHUB_RUN_ID" . "$GITHUB_RUN_ATTEMPT" "$MONDAY_RELEASE_JOB_ID" "$product"
ruby ../.github/scripts/research-release-bundle.rb pack "${RUNNER_TEMP}/research-image-release.tar" "$release" "$product"
