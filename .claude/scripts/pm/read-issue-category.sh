#!/usr/bin/env bash
set -euo pipefail

category_file="${1:-}"
[ -f "$category_file" ] || exit 1

awk '
  NR == 1 { if ($0 != "---") invalid=1; next }
  $0 == "---" { closed=1; exit }
  /^category:[[:space:]]*/ {
    count++
    value=$0
    sub(/^category:[[:space:]]*/, "", value)
  }
  END {
    if (invalid || !closed || count != 1 || (value != "bug" && value != "enhancement")) exit 1
    print value
  }
' "$category_file"
