#!/usr/bin/env bash
# One producer builds the union once. Each image consumes its own exact subset.
set -euo pipefail
mode=${1:?expected normalize, merge, products, binaries, recipes or contains}
product=${2-all}
catalog=$(dirname "${BASH_SOURCE[0]}")/research-release-products.json
normalize() {
  jq -er --arg product "$1" '
    .products as $catalog |
    (if $product == "all" then $catalog | keys else $product | split(",") end) as $selected |
    if ($selected|length)>0 and ($selected|unique|length)==($selected|length) and
       all($selected[]; . as $name | $catalog | has($name))
    then $selected | sort | join(",") else error("invalid research release products") end
  ' "$catalog"
}
if [[ $mode == merge ]]; then
  left='' right=''
  [[ $product == none ]] || left=$(normalize "$product")
  [[ ${3:?second selection required} == none ]] || right=$(normalize "$3")
  joined=$(jq -nr --arg left "$left" --arg right "$right" '[$left,$right] | map(select(length>0)|split(",")) | flatten | unique | join(",")')
  printf '%s\n' "${joined:-none}"
  exit
fi
product=$(normalize "$product")
case "$mode" in
  normalize) printf '%s\n' "$product" ;;
  products) tr ',' '\n' <<<"$product" ;;
  contains) [[ ,$product, == *,${3:?product required},* ]] ;;
  binaries|recipes)
    # shellcheck disable=SC2016
    filter='(.products as $catalog | [$product | split(",")[] | $catalog[.][]] | unique) as $binaries | '
    if [[ $mode == binaries ]]; then
      jq -r --arg product "$product" "$filter\$binaries[]" "$catalog"
    else
      jq -c --arg product "$product" "$filter.recipes[] | .binaries = [.binaries[] | select(. as \$binary | \$binaries | index(\$binary))] | select(.binaries | length > 0)" "$catalog"
    fi
    ;;
  *) exit 2 ;;
esac
