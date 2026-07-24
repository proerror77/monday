#!/usr/bin/env bash
set -euo pipefail

needs=$(cat)
failed=$(jq -r '
  to_entries[]
  | select(.value.result != "success" and .value.result != "skipped")
  | "\(.key)=\(.value.result)"
' <<<"$needs")

if [[ -n $failed ]]; then
  printf 'CI gate rejected:\n%s\n' "$failed" >&2
  exit 1
fi
