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

while IFS= read -r manifest; do
  [[ -n $manifest ]] || continue
  dir=${manifest%/Cargo.toml}
  rel=${dir#"$repo/"}
  [[ $rel != "$dir" ]] || { printf 'package path is outside the repository: %s\n' "$manifest" >&2; exit 1; }
  if git -C "$repo" grep -n -E '^[[:space:]]*(///|//!).*```' -- "$rel"; then
    printf 'loop package %s has a doctest; nextest does not run it\n' "$rel" >&2
    exit 1
  fi
done < <(jq -r --argjson selected "$selected_json" '
  .packages[] | select(.name as $name | $selected | index($name)) | .manifest_path
' "$meta")

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
