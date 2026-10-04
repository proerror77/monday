#!/usr/bin/env bash
# Exercise the actual source reader against Git's foreign-owner check, without
# chown, compiling, network access, or modifying any global Git configuration.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$script_dir/../.." && pwd)
expected=$(git -C "$repo_root" rev-parse HEAD)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
: > "$work/gitconfig"
export GIT_CONFIG_GLOBAL="$work/gitconfig" GIT_CONFIG_NOSYSTEM=1
export GIT_TEST_ASSUME_DIFFERENT_OWNER=1
if git -C "$repo_root" rev-parse HEAD > "$work/rejected" 2>&1; then
  echo 'foreign checkout owner fixture did not trigger Git protection' >&2; exit 1
fi
grep -Fq 'dubious ownership' "$work/rejected"
test "$(bash "$script_dir/research-release-source-sha.sh")" = "$expected"
if git -c safe.directory="$work" -C "$repo_root" rev-parse HEAD > "$work/rejected" 2>&1; then
  echo 'unrelated trusted directory admitted this checkout' >&2; exit 1
fi
grep -Fq 'dubious ownership' "$work/rejected"
test ! -s "$work/gitconfig"
echo 'PASS: foreign checkout rejected; exact source read succeeds; unrelated directory stays rejected; global Git policy untouched'
