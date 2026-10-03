#!/usr/bin/env bash
# CI builds bytes and provenance; it never acquires research compute or runs a
# Campaign. Release admission and authorized deployment remain separate.
set -euo pipefail
product=${1:-paired}
cd "$(dirname "$0")/../../rust_hft"
source_sha=$(../.github/scripts/research-release-source-sha.sh)
export MONDAY_RELEASE_JOB_ID
MONDAY_RELEASE_JOB_ID=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" --jq '.jobs|map(select(.name=="Research image binaries" or .name=="Research release binaries"))|if length==1 then .[0].id else error("ambiguous release producer") end')
export MONDAY_SOURCE_REVISION=$source_sha
export CARGO_TARGET_DIR="$PWD/target"
while IFS= read -r recipe; do
  args=(build --manifest-path "$(jq -r .manifest <<<"$recipe")" --release --locked -p "$(jq -r .package <<<"$recipe")")
  features=$(jq -r .features <<<"$recipe")
  [[ -z $features ]] || args+=(--features "$features")
  while IFS= read -r binary; do args+=(--bin "$binary"); done < <(jq -r '.binaries[]' <<<"$recipe")
  cargo "${args[@]}"
done < <(bash ../.github/scripts/research-release-products.sh recipes "$product")
release=${RUNNER_TEMP:?}/research-release
mkdir -p "$release/research-bin"
while IFS= read -r binary; do
  install -m 0755 "target/release/$binary" "$release/research-bin/$binary"
done < <(bash ../.github/scripts/research-release-products.sh binaries "$product")
../.github/scripts/verify-research-runtime-abi.sh "$release/research-bin" "$product"
../.github/scripts/research-image-release-artifact.sh create "$release" "$source_sha" "$GITHUB_RUN_ID" . "$GITHUB_RUN_ATTEMPT" "$MONDAY_RELEASE_JOB_ID" "$product"
ruby ../.github/scripts/research-release-bundle.rb pack "${RUNNER_TEMP}/research-image-release.tar" "$release" "$product"
