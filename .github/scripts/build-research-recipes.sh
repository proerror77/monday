#!/usr/bin/env bash
# Compile owners concurrently. Keep each owner's recipes and target writes serial.
set -euo pipefail
[[ $# == 2 || $# == 5 ]] || exit 2
product=${1:?product required}
phase=${2:?recipe measurement phase required}
[[ $phase == after-cache-lookup || $phase == warm-local || $phase == dependency-warm-serial || $phase == dependency-warm-parallel ]] || exit 2
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
cpu_model=$(awk -F: '/model name/ {sub(/^[[:space:]]*/, "", $2); print $2; exit}' /proc/cpuinfo)
cpus=$(nproc)
[[ $cpus =~ ^[1-9][0-9]*$ ]]
memory_mib=$(awk '/^MemAvailable:/ {print int($2/1024); found=1} END {if (!found) exit 1}' /proc/meminfo)
[[ $memory_mib =~ ^[0-9]+$ ]]
# A container may expose host memory. Respect its remaining cgroup allowance.
if [[ -r /sys/fs/cgroup/memory.max && -r /sys/fs/cgroup/memory.current ]]; then
  memory_max=$(cat /sys/fs/cgroup/memory.max)
  memory_current=$(cat /sys/fs/cgroup/memory.current)
  if [[ $memory_max =~ ^[0-9]+$ && $memory_current =~ ^[0-9]+$ ]]; then
    container_mib=$(( (memory_max - memory_current) / 1024 / 1024 ))
    (( container_mib >= 0 )) || container_mib=0
    if (( container_mib < memory_mib )); then memory_mib=$container_mib; fi
  fi
fi
runner_context=$(jq -cn --arg os "${RUNNER_OS:-$(uname -s)}" --arg arch "${RUNNER_ARCH:-$(uname -m)}" \
  --arg image "${ImageVersion:-unknown}" --arg cpu "$cpu_model" --argjson cpus "$cpus" --argjson memory "$memory_mib" \
  '{os:$os,arch:$arch,image_version:$image,cpu_model:$cpu,logical_cpus:$cpus,available_memory_mib:$memory}')
inputs_sha=$(sha256sum "$inputs" | awk '{print $1}')
work=${RUNNER_TEMP:?}/research-recipe-probe
mkdir -p "$work"
if [[ $# == 2 ]]; then
  timing_files=()
  while IFS= read -r manifest; do
    owner=${manifest%/Cargo.toml}
    timing_files+=("$work/$phase-${owner//\//--}.timings.jsonl")
    : >"${timing_files[${#timing_files[@]}-1]}"
  done < <(jq -r 'map(.manifest)|unique|.[]' <<<"$recipes")
  phase_start=$(date +%s)
  scheduler_pid='' scheduler_starting=1 scheduler_cancelled=0
  # Invoked by the signal traps below.
  # shellcheck disable=SC2329
  cancel_scheduler() {
    scheduler_cancelled=$1
    if [[ -n $scheduler_pid ]]; then
      kill -TERM "$scheduler_pid" 2>/dev/null || true
    elif (( scheduler_starting == 0 )); then exit "$scheduler_cancelled"; fi
  }
  trap 'cancel_scheduler 130' INT
  trap 'cancel_scheduler 143' TERM
  # Background execution lets Bash handle a signal sent only to its own PID.
  # The Ruby supervisor receives that signal and reaps its owned process groups.
  ruby -rjson - "$root/.github/scripts/build-research-recipes.sh" "$product" "$phase" "$inputs" "$cpus" "$memory_mib" <<'RUBY' &
script, product, phase, inputs, cpus, memory = ARGV
cpus, memory = Integer(cpus), Integer(memory)
requested = ENV.fetch('MONDAY_RESEARCH_BUILD_CONCURRENCY', '2')
abort 'research owner concurrency must be 1 or 2' unless %w[1 2].include?(requested)
budget = ENV.fetch('CARGO_BUILD_JOBS', cpus.to_s)
abort 'Cargo jobs must fit the runner CPU budget' unless budget.match?(/\A[1-9][0-9]*\z/) && Integer(budget) <= cpus
budget = Integer(budget)
owners = JSON.parse(File.read(inputs)).fetch('recipes').map { |recipe| recipe.fetch('manifest') }.uniq
# Reserve 4 GiB per active owner. This limits overlap, not a compiler's peak RSS.
concurrency = [Integer(requested), owners.length, budget, [memory / 4096, 1].max].min
jobs = budget / concurrency
pending, active = owners.dup, {}
failed_groups = []
status = 0
interrupted = nil
wake_read, wake_write = IO.pipe
%w[INT TERM CHLD].each do |signal|
  Signal.trap(signal) do
    interrupted = signal unless signal == 'CHLD'
    wake_write.write_nonblock('.', exception: false)
  end
end
begin
  until pending.empty? && active.empty?
    while !interrupted && active.length < concurrency && !pending.empty?
      owner = pending.shift
      pid = Process.spawn('bash', script, product, phase, owner, jobs.to_s, concurrency.to_s, pgroup: true)
      active[pid] = owner
    end
    if interrupted
      status = 128 + Signal.list.fetch(interrupted)
      break
    end
    completed = Process.wait2(-1, Process::WNOHANG)
    unless completed
      IO.select([wake_read])
      wake_read.read_nonblock(4096, exception: false)
      next
    end
    pid, result = completed
    owner = active.delete(pid)
    next if result.success?
    failed_groups << pid
    status = result.exitstatus || 128 + result.termsig
    warn "research recipe owner failed: #{owner} status=#{status}"
    break
  end
ensure
  # Only these task-owned process groups may be terminated. Kill descendants
  # even if their shell exits before a compiler which ignores TERM.
  groups = active.keys + failed_groups
  groups.each { |pid| Process.kill('TERM', -pid) rescue Errno::ESRCH }
  sleep 0.2 unless groups.empty?
  groups.each { |pid| Process.kill('KILL', -pid) rescue Errno::ESRCH }
  groups.each { |pid| Process.wait(pid) rescue Errno::ECHILD }
  wake_read.close
  wake_write.close
end
exit status
RUBY
  scheduler_pid=$!
  scheduler_starting=0
  if (( scheduler_cancelled != 0 )); then kill -TERM "$scheduler_pid" 2>/dev/null || true; fi
  status=0
  while true; do
    if wait "$scheduler_pid"; then break; else status=$?; fi
    # A trapped signal interrupts wait before the child finishes its cleanup.
    if (( scheduler_cancelled == 0 )) || ! kill -0 "$scheduler_pid" 2>/dev/null; then break; fi
  done
  scheduler_pid=''
  if (( scheduler_cancelled != 0 )); then status=$scheduler_cancelled; fi
  trap - INT TERM
  phase_seconds=$(( $(date +%s) - phase_start ))
  # Validate the complete recipe set before emitting successful phase evidence.
  # Failed phases retain completed measurements without admitting an artifact.
  jq -sc --argjson status "$status" --argjson recipes "$recipes" --arg source "$source_sha" --arg inputs "$inputs_sha" \
    --argjson phase_seconds "$phase_seconds" --slurpfile build_inputs "$inputs" '
    if $status == 0 then
      if length != ($recipes|length) or (all(.[]; .source_sha==$source and .compilation_inputs_sha256==$inputs)|not)
      then error("incomplete or mismatched owner evidence") else
        . as $measurements | $recipes[] as $recipe |
        [$measurements[]|select(.recipe==$recipe)] | if length==1 then .[] else error("duplicate or missing recipe") end
      end
    else .[] end | . + {phase_seconds:$phase_seconds,build_inputs:$build_inputs[0]}' \
    "${timing_files[@]}" | tee -a "$work/timings.jsonl"
  [[ $(sha256sum "$inputs" | awk '{print $1}') == "$inputs_sha" ]]
  exit "$status"
fi
manifest=$3 cargo_jobs=$4 concurrency=$5
[[ $cargo_jobs =~ ^[1-9][0-9]*$ && $concurrency =~ ^[12]$ ]]
(( cargo_jobs * concurrency <= cpus ))
jq -e --arg manifest "$manifest" 'any(.[]; .manifest==$manifest)' <<<"$recipes" >/dev/null
owner=${manifest%/Cargo.toml}
timings="$work/$phase-${owner//\//--}.timings.jsonl"
while IFS= read -r recipe; do
  package=$(jq -r .package <<<"$recipe")
  start=$(date +%s)
  recipe_target=$(bash "$root/.github/scripts/research-cache-layout.sh" target-dir "$(jq -r .manifest <<<"$recipe")")
  # Cargo emits dirty reasons to stderr; keep stdout as compiler JSON only.
  # Logging belongs to orchestration, not the dependency compilation identity.
  fingerprint_log="$work/$phase-$package.fingerprint.log"
  status=0
  { CARGO_LOG=cargo::core::compiler::fingerprint=info CARGO_TERM_COLOR=never \
      CARGO_TARGET_DIR="$recipe_target" bash "$root/.github/scripts/compile-research-recipe.sh" "$recipe" "$cargo_jobs" \
      >"$work/$phase-$package.jsonl"; } 2>&1 | tee "$fingerprint_log" >&2 || status=$?
  (( status == 0 )) || exit "$status"
  jq -es 'any(.[]; .reason=="compiler-artifact") and any(.[]; .reason=="build-finished" and .success==true)' \
    "$work/$phase-$package.jsonl" >/dev/null
  seconds=$(( $(date +%s) - start ))
  jq -s -c --arg cache_match "$cache_match" --arg phase "$phase" --arg source "$source_sha" --arg inputs "$inputs_sha" \
    --argjson requested_concurrency "${MONDAY_RESEARCH_BUILD_CONCURRENCY:-2}" \
    --argjson cargo_jobs "$cargo_jobs" --argjson concurrency "$concurrency" \
    --rawfile diagnostics "$fingerprint_log" \
    --argjson runner "$runner_context" --arg recipe_target "$recipe_target" --argjson recipe "$recipe" --argjson seconds "$seconds" \
    '{phase:$phase,runner:$runner,recipe_target:$recipe_target,cache_exact_match:$cache_match,source_sha:$source,compilation_inputs_sha256:$inputs,recipe:$recipe,seconds:$seconds,
      scheduler:{requested_owner_concurrency:$requested_concurrency,owner_concurrency:$concurrency,cargo_jobs:$cargo_jobs},
      compiler_artifacts:([.[]|select(.reason=="compiler-artifact")]|length),
      native_fingerprint_diagnostics:[$diagnostics|split("\n")[]|select(contains("fingerprint") and contains("libduckdb"))],
      native_artifacts:[.[]|select(.reason=="compiler-artifact" and (.package_id|contains("libduckdb-sys")))|{package_id,target:.target.name,fresh}],
      fresh:([.[]|select(.reason=="compiler-artifact" and .fresh==true)]|length),
      rebuilt:([.[]|select(.reason=="compiler-artifact" and .fresh==false)]|length)}' \
    "$work/$phase-$package.jsonl" >>"$timings"
done < <(jq -c --arg manifest "$manifest" '.[]|select(.manifest==$manifest)' <<<"$recipes")
[[ $(sha256sum "$inputs" | awk '{print $1}') == "$inputs_sha" ]]
