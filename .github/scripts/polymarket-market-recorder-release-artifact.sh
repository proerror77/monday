#!/usr/bin/env bash
set -euo pipefail

mode=${1:?expected create or verify}
release=${2:?expected release directory}
source_revision=${3:?expected source revision}
image_digest=${4:?expected image digest}
manifest="$release/polymarket-market-recorder-release.json"
binary="$release/new-ploy-runner"

[[ $source_revision =~ ^[0-9a-f]{40}$ ]] || {
  printf 'invalid source revision: %s\n' "$source_revision" >&2
  exit 1
}
[[ $image_digest =~ ^sha256:[0-9a-f]{64}$ ]] || {
  printf 'invalid image digest: %s\n' "$image_digest" >&2
  exit 1
}

verify_binary() {
  local expected_sha actual_sha
  [[ -f $binary && -x $binary && ! -L $binary ]]
  "$binary" --version | grep -Fqx "new-ploy-runner $source_revision"
  expected_sha=$(awk 'NR == 1 && NF == 2 && $2 == "new-ploy-runner" {print $1}' \
    "$release/new-ploy-runner.sha256")
  [[ $expected_sha =~ ^[0-9a-f]{64}$ ]]
  actual_sha=$(sha256sum "$binary" | awk '{print $1}')
  [[ $actual_sha == "$expected_sha" ]]
}

case "$mode" in
  create)
    [[ -d $release && ! -e $manifest ]]
    [[ -f $binary && -x $binary && ! -L $binary ]]
    "$binary" --version | grep -Fqx "new-ploy-runner $source_revision"
    (
      cd "$release"
      sha256sum new-ploy-runner > new-ploy-runner.sha256
    )
    candidate_sha=$(awk '{print $1}' "$release/new-ploy-runner.sha256")
    jq -S -n \
      --arg source_revision "$source_revision" \
      --arg candidate_sha256 "$candidate_sha" \
      --arg image_digest "$image_digest" \
      '{schema:"monday.polymarket_market_recorder_release.v1",
        source_revision:$source_revision,
        candidate:{file:"new-ploy-runner",sha256:$candidate_sha256},
        image_digest:$image_digest,
        platform:{os:"linux",architecture:"amd64"}}' > "$manifest"
    (
      cd "$release"
      sha256sum polymarket-market-recorder-release.json \
        > polymarket-market-recorder-release.json.sha256
    )
    ;;
  verify)
    expected_files=$'new-ploy-runner\nnew-ploy-runner.sha256\npolymarket-market-recorder-release.json\npolymarket-market-recorder-release.json.sha256'
    actual_files=$(find "$release" -mindepth 1 -maxdepth 1 -print \
      | sed 's|.*/||' | sort)
    [[ $actual_files == "$expected_files" ]]
    verify_binary
    (
      cd "$release"
      sha256sum --check --strict polymarket-market-recorder-release.json.sha256 \
        >/dev/null
    )
    candidate_sha=$(sha256sum "$binary" | awk '{print $1}')
    jq -e \
      --arg source_revision "$source_revision" \
      --arg candidate_sha256 "$candidate_sha" \
      --arg image_digest "$image_digest" '
      (keys | sort) == ["candidate", "image_digest", "platform", "schema", "source_revision"]
      and .schema == "monday.polymarket_market_recorder_release.v1"
      and .source_revision == $source_revision
      and .candidate == {file:"new-ploy-runner",sha256:$candidate_sha256}
      and .image_digest == $image_digest
      and .platform == {os:"linux",architecture:"amd64"}' "$manifest" >/dev/null
    ;;
  *)
    printf 'unsupported artifact mode: %s\n' "$mode" >&2
    exit 2
    ;;
esac
