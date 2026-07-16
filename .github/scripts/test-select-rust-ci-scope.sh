#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
selector="$script_dir/select-rust-ci-scope.sh"
fixtures="$script_dir/fixtures/rust-ci-scope"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

run_case() {
  local name=$1 event=$2 changed=$3
  local output="$tmp_dir/$name.out"
  "$selector" --event "$event" --changed-files "$fixtures/$changed" \
    --metadata "$fixtures/metadata.fixture" --output "$output"
  printf '%s\n' "$output"
}

assert_flag() {
  local output=$1 flag=$2 expected=$3
  grep -qx "$flag=$expected" "$output" || {
    printf '%s: expected %s=%s\n' "$output" "$flag" "$expected" >&2
    cat "$output" >&2
    exit 1
  }
}

collector=$(run_case collector pull_request collector.txt)
for flag in loop collector control toolchain; do assert_flag "$collector" "$flag" true; done
for flag in handoff json ondo focused; do assert_flag "$collector" "$flag" false; done

live=$(run_case live pull_request live.txt)
for flag in handoff json ondo focused toolchain; do assert_flag "$live" "$flag" true; done
for flag in loop collector control; do assert_flag "$live" "$flag" false; done

control=$(run_case control pull_request control.txt)
for flag in control toolchain; do assert_flag "$control" "$flag" true; done
for flag in loop handoff json ondo collector focused; do assert_flag "$control" "$flag" false; done

docs=$(run_case docs pull_request docs.txt)
for flag in loop handoff json ondo collector control focused toolchain; do
  assert_flag "$docs" "$flag" false
done

full=$(run_case full push collector.txt)
for flag in loop handoff json ondo collector control focused toolchain; do
  assert_flag "$full" "$flag" true
done

deletion_repo="$tmp_dir/deletion-repo"
mkdir -p "$deletion_repo/rust_hft/tools/collector/src"
git -C "$deletion_repo" init -q
touch "$deletion_repo/rust_hft/tools/collector/src/removed.rs"
git -C "$deletion_repo" add .
git -C "$deletion_repo" -c user.name=CI -c user.email=ci@example.invalid commit -qm base
deletion_base=$(git -C "$deletion_repo" rev-parse HEAD)
rm "$deletion_repo/rust_hft/tools/collector/src/removed.rs"
git -C "$deletion_repo" add -u
git -C "$deletion_repo" -c user.name=CI -c user.email=ci@example.invalid commit -qm deletion
deletion="$tmp_dir/deletion.out"
(cd "$deletion_repo" && "$selector" --event pull_request --base "$deletion_base" \
  --head HEAD --metadata "$fixtures/metadata.fixture" --output "$deletion")
for flag in loop collector control toolchain; do assert_flag "$deletion" "$flag" true; done

rename_repo="$tmp_dir/rename-repo"
mkdir -p "$rename_repo/rust_hft/tools/collector/src" "$rename_repo/docs"
git -C "$rename_repo" init -q
touch "$rename_repo/rust_hft/tools/collector/src/moved.rs"
git -C "$rename_repo" add .
git -C "$rename_repo" -c user.name=CI -c user.email=ci@example.invalid commit -qm base
rename_base=$(git -C "$rename_repo" rev-parse HEAD)
git -C "$rename_repo" mv rust_hft/tools/collector/src/moved.rs docs/moved.rs
git -C "$rename_repo" -c user.name=CI -c user.email=ci@example.invalid commit -qm rename
rename="$tmp_dir/rename.out"
(cd "$rename_repo" && "$selector" --event pull_request --base "$rename_base" \
  --head HEAD --metadata "$fixtures/metadata.fixture" --output "$rename")
for flag in loop collector control toolchain; do assert_flag "$rename" "$flag" true; done

printf 'rust CI scope selector tests passed\n'
