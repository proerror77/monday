#!/usr/bin/env bash
# Build one nextest archive for the selected loop packages and record the
# runnable test list. Shards run that archive; they do not compile.
set -euo pipefail

repo=${GITHUB_WORKSPACE:-$(git rev-parse --show-toplevel)}
cd "$repo/rust_hft"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=loop-nextest-counts.sh
source "$repo/.github/scripts/loop-nextest-counts.sh"

work=${LOOP_NEXTEST_WORK:?}
mkdir -p "$work"
packages=${LOOP_PACKAGES#,}
packages=${packages%,}
[[ -n $packages ]] || { printf 'loop package selection is empty\n' >&2; exit 1; }

args=()
seen=' '
selected_json='[]'
while IFS= read -r package; do
  [[ -n $package ]] || continue
  [[ $package =~ ^[A-Za-z0-9_-]+$ ]] || { printf 'invalid package name: %s\n' "$package" >&2; exit 1; }
  [[ $seen != *" $package "* ]] || { printf 'duplicate package: %s\n' "$package" >&2; exit 1; }
  seen+=" $package "
  args+=(-p "$package")
  selected_json=$(jq -c --arg package "$package" '. + [$package]' <<<"$selected_json")
done < <(printf '%s\n' "$packages" | tr ',' '\n')

meta=$(mktemp)
trap 'rm -f "$meta"' EXIT
cargo metadata --manifest-path research-core/Cargo.toml --locked --no-deps --format-version 1 >"$meta"
missing=$(jq -r --argjson selected "$selected_json" '($selected - [.packages[].name])[]' "$meta")
[[ -z $missing ]] || { printf 'selected package is outside research-core:\n%s\n' "$missing" >&2; exit 1; }

config=research-core/.config/nextest.toml
[[ -f $config ]] || { printf 'missing %s\n' "$config" >&2; exit 1; }
start=$(date +%s)
cargo nextest archive \
  --manifest-path research-core/Cargo.toml \
  --locked \
  --config-file "$config" \
  --user-config-file none \
  "${args[@]}" \
  --archive-file "$work/archive.tar.zst"
build_seconds=$(( $(date +%s) - start ))
cargo nextest list \
  --archive-file "$work/archive.tar.zst" \
  --config-file "$config" \
  --user-config-file none \
  --message-format json >"$work/nextest-list.json"
loop_nextest_write_expected "$work/nextest-list.json" "$work/expected-counts.json" \
  "$build_seconds" 0 0
# shellcheck source-path=SCRIPTDIR
# shellcheck source=loop-nextest-doctests.sh
source "$repo/.github/scripts/loop-nextest-doctests.sh"
loop_nextest_run_doctests "$work" "$work/nextest-list.json" "${args[@]}"
jq --argjson listed "$LOOP_DOC_LISTED" --argjson passed "$LOOP_DOC_PASSED" \
  '.doctests_listed=$listed | .doctests_passed=$passed' "$work/expected-counts.json" >"$work/docs-counts.json"
mv "$work/docs-counts.json" "$work/expected-counts.json"

# Use nextest's own hash membership. Do not reimplement its hash algorithm.
for shard in 1 2 3 4; do
  cargo nextest list --archive-file "$work/archive.tar.zst" \
    --partition "hash:$shard/4" --config-file "$config" \
    --user-config-file none --message-format json >"$work/hash-$shard.json"
done
ruby "$repo/.github/scripts/loop-nextest-plan.rb" "$work"

if [[ -n ${GITHUB_OUTPUT:-} ]]; then printf 'producer_attempt=%s\n' "${GITHUB_RUN_ATTEMPT:?}" >>"$GITHUB_OUTPUT"; fi
