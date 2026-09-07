#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
gate="$script_dir/agent-worktree-preflight.sh"

primary=$(git worktree list --porcelain | awk '$1 == "worktree" && !found { print substr($0, 10); found=1 }')
if (cd "$primary" && "$gate" check-managed) >/dev/null 2>&1; then
  echo 'primary checkout unexpectedly passed preflight' >&2
  exit 1
fi

# Invoking the helper without selecting a managed-writer check never rejects
# an otherwise legitimate primary checkout.
(cd "$primary" && "$gate") | grep -q '^usage:'
report=$($gate report)
grep -Eq 'state=(registered-clean|dirty|prunable)' <<<"$report"
grep -Eq 'checkout=(branch|detached)' <<<"$report"
grep -Eq 'head=[0-9a-f]{40}' <<<"$report"

fixture=$(mktemp -d)
fixture=$(cd "$fixture" && pwd -P)
trap 'rm -rf "$fixture"' EXIT
git -C "$fixture" init -q -b main
git -C "$fixture" config user.email test@example.invalid
git -C "$fixture" config user.name test
git -C "$fixture" commit -q --allow-empty -m initial
base=$(git -C "$fixture" rev-parse HEAD)
managed="$fixture/workspaces/owned"
git -C "$fixture" worktree add -q -b reviewed-branch "$managed" HEAD
record=$(git -C "$managed" rev-parse --git-path agent-worktree.yml)
printf '%s\n' \
  'contract: test' \
  'owner: test' \
  "worktree: $managed" \
  'branch: reviewed-branch' \
  "base_sha: $base" \
  'allowed_files: test' \
  'dependency: none' >"$record"
output=$(cd "$managed" && "$gate" check-managed)
grep -qx 'verdict=ok' <<<"$output"
git -C "$fixture" worktree add -q --detach "$fixture/detached" HEAD
fixture_report=$(cd "$fixture" && "$gate" report)
grep -Eq "worktree=$fixture/detached.*checkout=detached.*state=registered-clean" <<<"$fixture_report"
printf 'agent worktree preflight tests passed\n'
