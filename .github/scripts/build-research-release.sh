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
target=x86_64-unknown-linux-gnu
bash ../.github/scripts/build-research-recipes.sh "$product" after-cache-lookup
if [[ ${MONDAY_RESEARCH_CACHE_PROBE:-0} == 1 ]]; then
  # Probe only the current runner's target. Never save a PR cache or change recipes.
  before=$(mktemp)
  after=$(mktemp)
  trap 'rm -f "$before" "$after"' EXIT
  while IFS= read -r binary; do sha256sum "target/$target/release/$binary"; done \
    < <(bash ../.github/scripts/research-release-products.sh binaries "$product") >"$before"
  bash ../.github/scripts/build-research-recipes.sh "$product" warm-local
  while IFS= read -r binary; do sha256sum "target/$target/release/$binary"; done \
    < <(bash ../.github/scripts/research-release-products.sh binaries "$product") >"$after"
  diff -u "$before" "$after"
fi
release=${RUNNER_TEMP:?}/research-release
mkdir -p "$release/research-bin"
while IFS= read -r binary; do
  install -m 0755 "target/$target/release/$binary" "$release/research-bin/$binary"
done < <(bash ../.github/scripts/research-release-products.sh binaries "$product")
../.github/scripts/verify-research-runtime-abi.sh "$release/research-bin" "$product"
../.github/scripts/research-image-release-artifact.sh create "$release" "$source_sha" "$GITHUB_RUN_ID" . "$GITHUB_RUN_ATTEMPT" "$MONDAY_RELEASE_JOB_ID" "$product"
ruby ../.github/scripts/research-release-bundle.rb pack "${RUNNER_TEMP}/research-image-release.tar" "$release" "$product"
