#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
selector="$script_dir/select-prediction-image-smoke.sh"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

run_case() {
  local name=$1 event=$2
  shift 2
  local changed="$tmp_dir/$name.changed"
  local output="$tmp_dir/$name.out"
  printf '%s\n' "$@" >"$changed"
  "$selector" --event "$event" --changed-files "$changed" --output "$output"
  printf '%s\n' "$output"
}

assert_selected() {
  local output=$1 expected=$2
  grep -qx "research_image_smoke=$expected" "$output" || {
    printf '%s: expected research_image_smoke=%s\n' "$output" "$expected" >&2
    cat "$output" >&2
    exit 1
  }
}

ordinary_rs=$(run_case ordinary-rs pull_request \
  rust_hft/prediction-markets/crates/ploy-research/src/lib.rs)
assert_selected "$ordinary_rs" false

for path in \
  rust_hft/deployment/docker/Dockerfile.research \
  rust_hft/.dockerignore \
  rust_hft/Cargo.toml \
  rust_hft/Cargo.lock \
  rust_hft/prediction-markets/Cargo.toml \
  rust_hft/prediction-markets/Cargo.lock \
  rust_hft/prediction-markets/rust-toolchain.toml \
  rust_hft/.cargo/config.toml \
  .cargo/config.toml \
  .github/workflows/ploy-ci.yml \
  .github/scripts/select-prediction-image-smoke.sh \
  .github/scripts/test-select-prediction-image-smoke.sh
do
  name=$(printf '%s' "$path" | tr '/.' '--')
  selected=$(run_case "$name" pull_request "$path")
  assert_selected "$selected" true
done

push=$(run_case push push rust_hft/prediction-markets/crates/ploy-research/src/lib.rs)
assert_selected "$push" true

manual=$(run_case manual workflow_dispatch docs/ci.md)
assert_selected "$manual" true

printf 'prediction image smoke selector tests passed\n'
