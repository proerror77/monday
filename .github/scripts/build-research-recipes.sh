#!/usr/bin/env bash
# Measure the admitted recipes. Keep separate Cargo calls and serial target writes.
set -euo pipefail
product=${1:?product required}
phase=${2:?after-cache-lookup or warm-local required}
[[ $phase == after-cache-lookup || $phase == warm-local ]] || exit 2
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root/rust_hft"
inputs=${MONDAY_BUILD_INPUTS_FILE:?}
source_sha=${MONDAY_SOURCE_REVISION:?}
[[ $source_sha =~ ^[0-9a-f]{40}$ ]]
[[ $(jq -r .target "$inputs") == x86_64-unknown-linux-gnu ]]
[[ $(jq -r .profile "$inputs") == release ]]
recipes=$(bash "$root/.github/scripts/research-release-products.sh" recipes "$product" | jq -s .)
jq -en --argjson recipes "$recipes" --slurpfile inputs "$inputs" '$recipes == $inputs[0].recipes' >/dev/null
cache_match=${MONDAY_CACHE_EXACT_MATCH:-unknown}
[[ $cache_match == true || $cache_match == false || $cache_match == unknown ]]
inputs_sha=$(sha256sum "$inputs" | awk '{print $1}')
work=${RUNNER_TEMP:?}/research-recipe-probe
mkdir -p "$work"
while IFS= read -r recipe; do
  package=$(jq -r .package <<<"$recipe")
  args=(build --manifest-path "$(jq -r .manifest <<<"$recipe")" --target x86_64-unknown-linux-gnu --release --locked -p "$package")
  features=$(jq -r .features <<<"$recipe")
  [[ -z $features ]] || args+=(--features "$features")
  while IFS= read -r binary; do args+=(--bin "$binary"); done < <(jq -r '.binaries[]' <<<"$recipe")
  start=$(date +%s)
  cargo "${args[@]}" --message-format json-render-diagnostics >"$work/$phase-$package.jsonl"
  jq -es 'any(.[]; .reason=="compiler-artifact") and any(.[]; .reason=="build-finished" and .success==true)' \
    "$work/$phase-$package.jsonl" >/dev/null
  seconds=$(( $(date +%s) - start ))
  jq -s -c --arg cache_match "$cache_match" --arg phase "$phase" --arg source "$source_sha" --arg inputs "$inputs_sha" \
    --argjson recipe "$recipe" --argjson seconds "$seconds" \
    '{phase:$phase,cache_exact_match:$cache_match,source_sha:$source,compilation_inputs_sha256:$inputs,recipe:$recipe,seconds:$seconds,
      compiler_artifacts:([.[]|select(.reason=="compiler-artifact")]|length),
      fresh:([.[]|select(.reason=="compiler-artifact" and .fresh==true)]|length),
      rebuilt:([.[]|select(.reason=="compiler-artifact" and .fresh==false)]|length)}' \
    "$work/$phase-$package.jsonl" | tee -a "$work/timings.jsonl"
done < <(jq -c '.[]' <<<"$recipes")
[[ $(sha256sum "$inputs" | awk '{print $1}') == "$inputs_sha" ]]
