#!/usr/bin/env bash
# The existing deployed images define release products. This does not admit
# new control/data workers or scientific compute.
set -euo pipefail
mode=${1:?expected binaries or recipes}
product=${2:-paired}
case "$product" in runner|controller|paired) ;; *) echo 'invalid research release product' >&2; exit 2 ;; esac
catalog=$(dirname "${BASH_SOURCE[0]}")/research-release-products.json
# shellcheck disable=SC2016
filter='(.products | if $product == "paired" then [.runner[],.controller[]] else .[$product] end | unique) as $binaries | '
case "$mode" in
  binaries) jq -r --arg product "$product" "$filter\$binaries[]" "$catalog" ;;
  recipes) jq -c --arg product "$product" "$filter.recipes[] | .binaries = [.binaries[] | select(. as \$binary | \$binaries | index(\$binary))] | select(.binaries | length > 0)" "$catalog" ;;
  *) exit 2 ;;
esac
