#!/usr/bin/env bash
# Static contract greps intentionally use literal shell expressions.
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly WORKFLOW="$SCRIPT_DIR/../../.github/workflows/acr-publish.yml"
readonly ARTIFACT_HELPER="$SCRIPT_DIR/../../.github/scripts/polymarket-market-recorder-release-artifact.sh"
readonly DOCKERFILE="$SCRIPT_DIR/../../rust_hft/deployment/docker/Dockerfile.polymarket-market-recorder"
readonly RUNNER="$SCRIPT_DIR/../../rust_hft/prediction-markets/apps/new-ploy-runner/src/main.rs"

shellcheck "$0"
test -x "$ARTIFACT_HELPER"

grep -Fq 'ARG SOURCE_REVISION' "$DOCKERFILE"
grep -Fq "grep -Eq '^[0-9a-f]{40}$'" "$DOCKERFILE"
grep -Fq 'MONDAY_SOURCE_REVISION="$SOURCE_REVISION" cargo' "$DOCKERFILE"

grep -Fq 'option_env!("MONDAY_SOURCE_REVISION")' "$RUNNER"
grep -Fq 'new-ploy-runner {BUILD_SOURCE_REVISION}' "$RUNNER"

grep -Fq 'Extract bare-metal Polymarket market recorder' "$WORKFLOW"
grep -Fq 'polymarket-market-recorder-release-artifact.sh create' "$WORKFLOW"
grep -Fq 'polymarket-market-recorder-release-artifact.sh verify' "$WORKFLOW"
grep -Fq 'new-ploy-runner $source_revision' "$ARTIFACT_HELPER"
grep -Fq 'monday.polymarket_market_recorder_release.v1' "$ARTIFACT_HELPER"
grep -Fq 'polymarket-market-recorder-linux-amd64-${{ needs.selector.outputs.source_sha }}' "$WORKFLOW"

if [[ ${MONDAY_TEST_RECORDER_IMAGE:-0} == 1 ]]; then
  : "${SOURCE_REVISION:?set SOURCE_REVISION for the image contract test}"
  [[ $SOURCE_REVISION =~ ^[0-9a-f]{40}$ ]]
  for command in docker jq sha256sum; do
    command -v "$command" >/dev/null
  done

  tmp_root=$(mktemp -d)
  release_dir="$tmp_root/release"
  mkdir "$release_dir"
  trap 'rm -rf "$tmp_root"' EXIT
  image="monday-polymarket-market-recorder-test:$SOURCE_REVISION"
  docker build --quiet \
    --build-arg "SOURCE_REVISION=$SOURCE_REVISION" \
    -f "$DOCKERFILE" \
    -t "$image" \
    "$SCRIPT_DIR/../../rust_hft" >/dev/null
  if docker build --quiet \
    --build-arg SOURCE_REVISION=invalid \
    -f "$DOCKERFILE" \
    -t monday-polymarket-market-recorder-invalid-test \
    "$SCRIPT_DIR/../../rust_hft" >/dev/null 2>&1; then
    printf 'market-recorder image accepted an invalid source revision\n' >&2
    exit 1
  fi

  container_id=$(docker create "$image")
  trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true; rm -rf "$tmp_root"' EXIT
  docker cp "$container_id:/usr/local/bin/new-ploy-runner" \
    "$release_dir/new-ploy-runner"
  chmod 0755 "$release_dir/new-ploy-runner"
  image_id=$(docker image inspect --format '{{.Id}}' "$image")
  "$ARTIFACT_HELPER" create "$release_dir" "$SOURCE_REVISION" "$image_id"
  "$ARTIFACT_HELPER" verify "$release_dir" "$SOURCE_REVISION" "$image_id"

  cp "$release_dir/polymarket-market-recorder-release.json" \
    "$tmp_root/release.json.good"
  jq '.source_revision = "0000000000000000000000000000000000000000"' \
    "$tmp_root/release.json.good" \
    > "$release_dir/polymarket-market-recorder-release.json"
  (
    cd "$release_dir"
    sha256sum polymarket-market-recorder-release.json \
      > polymarket-market-recorder-release.json.sha256
  )
  if "$ARTIFACT_HELPER" verify "$release_dir" "$SOURCE_REVISION" "$image_id"; then
    printf 'release verifier accepted the wrong source revision\n' >&2
    exit 1
  fi
fi

printf 'Polymarket market-recorder release contract tests passed\n'
