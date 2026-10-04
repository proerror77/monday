#!/usr/bin/env bash
set -euo pipefail

directory=${1:?expected research-runner binary directory}
product=$(bash "$(dirname "${BASH_SOURCE[0]}")/research-release-products.sh" normalize "${2:-all}")
expected=()
while IFS= read -r binary; do expected+=("$binary"); done < <(bash "$(dirname "${BASH_SOURCE[0]}")/research-release-products.sh" binaries "$product")
[[ ${#expected[@]} -gt 0 ]] || exit 2

fail() { printf 'research release validation: %s\n' "$*" >&2; exit 1; }
test -d "$directory" || fail "binary directory missing: $directory"
test "$(find "$directory" -mindepth 1 -maxdepth 1 -print | wc -l)" -eq "${#expected[@]}" || fail 'unexpected binary file count'

for binary in "${expected[@]}"; do
  test -f "$directory/$binary" && test ! -L "$directory/$binary" || fail "regular binary missing: $binary"
  test -x "$directory/$binary" || fail "executable mode lost: $binary; transport release bytes in the verified tar bundle"
done
