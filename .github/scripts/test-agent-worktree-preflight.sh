#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
gate="$script_dir/agent-worktree-preflight.sh"

output=$($gate check)
grep -qx 'verdict=ok' <<<"$output"

primary=$(git worktree list --porcelain | awk '$1 == "worktree" { print substr($0, 10); exit }')
if (cd "$primary" && "$gate" check) >/dev/null 2>&1; then
  echo 'primary checkout unexpectedly passed preflight' >&2
  exit 1
fi

"$gate" report | grep -Eq 'state=(active|dirty|prunable)'
printf 'agent worktree preflight tests passed\n'
