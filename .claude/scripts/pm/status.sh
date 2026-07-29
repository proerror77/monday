#!/usr/bin/env bash
set -euo pipefail

issue_count() {
  # ponytail: this quick dashboard caps at 1000; use gh api pagination above it.
  gh issue list "$@" --limit 1000 --json number --jq length
}

echo "📊 GitHub Issues (live)"
echo "======================="
echo "GitHub is authoritative; local mirrors are optional."

if ! open=$(issue_count --state open) ||
  ! closed=$(issue_count --state closed) ||
  ! tracking=$(issue_count --state open --label tracking) ||
  ! runtime=$(issue_count --state open --label runtime); then
  echo "❌ GitHub status failed. Check repository access and gh auth status." >&2
  exit 1
fi

echo "  Open: $open"
echo "  Closed: $closed"
echo "  Total: $((open + closed))"
echo "  Tracking: $tracking open"
echo "  Runtime: $runtime open"
