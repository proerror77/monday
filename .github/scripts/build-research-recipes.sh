#!/usr/bin/env bash
# Measure the admitted recipes. Keep separate Cargo calls and serial target writes.
set -euo pipefail
product=${1:?product required}
phase=${2:?after-cache-lookup, warm-local or dependency-warm-local required}
[[ $phase == after-cache-lookup || $phase == warm-local || $phase == dependency-warm-local ]] || exit 2
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root/rust_hft"
inputs=${MONDAY_BUILD_INPUTS_FILE:?}
source_sha=${MONDAY_SOURCE_REVISION:?}
[[ $source_sha =~ ^[0-9a-f]{40}$ ]]
[[ $(jq -r .target "$inputs") == x86_64-unknown-linux-gnu ]]
[[ $(jq -r .profile "$inputs") == release ]]
[[ ${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER:?} == "$root/.github/scripts/research-release-linker.sh" ]]
[[ -x $CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER ]]
recipes=$(bash "$root/.github/scripts/research-release-products.sh" recipes "$product" | jq -s .)
jq -en --argjson recipes "$recipes" --slurpfile inputs "$inputs" '$recipes == $inputs[0].recipes' >/dev/null
cache_match=${MONDAY_CACHE_EXACT_MATCH:-unknown}
[[ $cache_match == true || $cache_match == false || $cache_match == unknown ]]
cpu_model=$(awk -F: '/model name/ {sub(/^[[:space:]]*/, "", $2); print $2; exit}' /proc/cpuinfo)
runner_context=$(jq -cn --arg os "${RUNNER_OS:-$(uname -s)}" --arg arch "${RUNNER_ARCH:-$(uname -m)}" \
  --arg image "${ImageVersion:-unknown}" --arg cpu "$cpu_model" --argjson cpus "$(nproc)" \
  '{os:$os,arch:$arch,image_version:$image,cpu_model:$cpu,logical_cpus:$cpus}')
inputs_sha=$(sha256sum "$inputs" | awk '{print $1}')
work=${RUNNER_TEMP:?}/research-recipe-probe
mkdir -p "$work"
while IFS= read -r recipe; do
  package=$(jq -r .package <<<"$recipe")
  args=(build --manifest-path "$(jq -r .manifest <<<"$recipe")" --target x86_64-unknown-linux-gnu --release --locked --timings -p "$package")
  features=$(jq -r .features <<<"$recipe")
  [[ -z $features ]] || args+=(--features "$features")
  while IFS= read -r binary; do args+=(--bin "$binary"); done < <(jq -r '.binaries[]' <<<"$recipe")
  start=$(date +%s)
  recipe_target=$(bash "$root/.github/scripts/research-cache-layout.sh" target-dir "$(jq -r .manifest <<<"$recipe")")
  prefix="$work/$phase-$package"
  mkdir -p "$prefix-links"
  # A failed recipe must not upload an earlier recipe's HTML report.
  rm -f "$recipe_target/cargo-timings/cargo-timing.html"
  status=0
  CARGO_TARGET_DIR="$recipe_target" MONDAY_RESEARCH_LINK_TIMINGS_DIR="$prefix-links" LC_ALL=C \
    command time --quiet \
    --format '{"wall_seconds":%e,"user_seconds":%U,"system_seconds":%S,"max_rss_kib":%M,"exit_code":%x}' \
    --output "$prefix-resources.json" \
    cargo "${args[@]}" --message-format json-render-diagnostics >"$prefix.jsonl" || status=$?
  if [[ -f $recipe_target/cargo-timings/cargo-timing.html ]]; then
    cp "$recipe_target/cargo-timings/cargo-timing.html" "$prefix-timing.html"
  fi
  if [[ $status == 0 ]]; then
    jq -es 'any(.[]; .reason=="compiler-artifact") and any(.[]; .reason=="build-finished" and .success==true)' \
      "$prefix.jsonl" >/dev/null || status=1
    [[ -s $prefix-timing.html ]] || status=1
  fi
  seconds=$(( $(date +%s) - start ))
  jq -s -c --arg cache_match "$cache_match" --arg phase "$phase" --arg source "$source_sha" --arg inputs "$inputs_sha" \
    --argjson runner "$runner_context" --arg recipe_target "$recipe_target" --argjson recipe "$recipe" --argjson seconds "$seconds" \
    --arg cache_restored_key "${MONDAY_CACHE_RESTORED_KEY:-}" --argjson status "$status" \
    --arg run "${GITHUB_RUN_ID:-}" --arg attempt "${GITHUB_RUN_ATTEMPT:-}" --arg job "${MONDAY_RELEASE_JOB_ID:-}" \
    --slurpfile resources "$prefix-resources.json" \
    '{phase:$phase,runner:$runner,recipe_target:$recipe_target,cache_exact_match:$cache_match,cache_restored_key:$cache_restored_key,
      source_sha:$source,run_id:$run,run_attempt:$attempt,producer_job_id:$job,
      compilation_inputs_sha256:$inputs,recipe:$recipe,seconds:$seconds,exit_code:$status,success:($status==0),
      resource_usage:$resources[0],linker:"clang -fuse-ld=mold",
      compiler_artifacts:([.[]|select(.reason=="compiler-artifact")]|length),
      build_scripts:[.[]|select(.reason=="build-script-executed")|{package_id}],
      native_artifacts:[.[]|select(.reason=="compiler-artifact" and (.package_id|contains("libduckdb-sys")))|{package_id,target:.target.name,fresh}],
      fresh:([.[]|select(.reason=="compiler-artifact" and .fresh==true)]|length),
      rebuilt:([.[]|select(.reason=="compiler-artifact" and .fresh==false)]|length)}' \
    "$prefix.jsonl" | tee -a "$work/timings.jsonl"
  [[ $status == 0 ]] || exit "$status"
done < <(jq -c '.[]' <<<"$recipes")
[[ $(sha256sum "$inputs" | awk '{print $1}') == "$inputs_sha" ]]
