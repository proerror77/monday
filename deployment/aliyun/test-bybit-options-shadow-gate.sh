#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
policy="$script_dir/bybit-options-shadow-gate-policy.jq"
runtime_policy="$script_dir/bybit-options-runtime-health-policy.jq"
control_lib="$script_dir/bybit-options-control-plane-lib.sh"
shadow_gate="$script_dir/host-bybit-options-shadow-gate.sh"
cutover="$script_dir/host-bybit-options-cutover.sh"
candidate=$(printf 'a%.0s' {1..64})
bundle=$(printf 'b%.0s' {1..64})
source=$(printf 'c%.0s' {1..40})
health_sha=$(printf 'd%.0s' {1..64})
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

jq -n \
  --arg candidate "$candidate" --arg bundle "$bundle" --arg source "$source" \
  --arg health_sha "$health_sha" \
  '{schema:"monday.bybit_options_shadow_gate.v1",
    run_id:"20260807T000000Z-1",
    candidate_sha256:$candidate,deployment_bundle_sha256:$bundle,
    deployment_source_revision:$source,
    duration_seconds:3600,health_settle_seconds:2400,
    test_only:false,passed:true,production_eligible:true,
    health_samples:120,max_health_silence_seconds:5,
    health_sha256:$health_sha,
    service:{unit:"bybit-options-shadow.service",active:true,restart_count:0,
      binary_sha256:$candidate,spool_dir:"/data/monday/spool/bybit-options-shadow"},
    health:{schema:"monday.bybit_options_quote.v1",venue:"bybit",category:"option",
      symbols_expected:1500,symbols_seen:1450,connected_workers:1,events:10000,
      last_event_at_ms:1750000000000,disk_free_gb:120.5,disk_warning:false,
      spool_warning:false,upload_failure_count:0,upload_warning:false,
      updated_at_ms:1750000001000},
    upload_status:{failure_count:0,last_success_at:1750000002000},
    spool_drained:true}' \
  >"$tmp_dir/gate.json"

gate() {
  jq -e --arg candidate_sha256 "$candidate" \
    --arg deployment_bundle_sha256 "$bundle" \
    --arg deployment_source_revision "$source" \
    --argjson minimum_symbols 500 \
    --argjson test_only false \
    -f "$policy" "$1" >/dev/null
}

gate "$tmp_dir/gate.json"

reject() {
  local filter=$1 name=$2
  jq "$filter" "$tmp_dir/gate.json" >"$tmp_dir/$name.json"
  if gate "$tmp_dir/$name.json"; then
    printf 'gate accepted invalid evidence: %s\n' "$name" >&2
    exit 1
  fi
}

reject '.duration_seconds=3599' short-duration
reject '.test_only=true' test-shortened
reject '.production_eligible=false' not-eligible
reject '.passed=false' failed
reject '.service.unit="bybit-options-archiver.service"' wrong-unit
reject '.service.restart_count=1' restarted
reject '.service.binary_sha256=("9"*64)' wrong-binary
reject '.service.spool_dir="/data/monday/spool/bybit-options"' wrong-spool
reject '.health.symbols_seen=50' small-universe
reject '.health.symbols_seen=300' below-production-floor
reject '.health.symbols_expected=50' small-expected
reject '.health.connected_workers=0' no-workers
reject '.health.disk_warning=true' disk-warning
reject '.health.spool_warning=true' spool-warning
reject '.health.upload_warning=true' upload-warning
reject '.health.upload_failure_count=1' upload-failure
reject '.health.last_event_at_ms=0' stale-event
reject '.health.updated_at_ms=0' missing-update
reject '.health.updated_at_ms=1749999999999' updated-before-event
reject '.health_sha256="abc"' bad-health-sha
reject '.health_samples=0' no-samples
reject '.max_health_silence_seconds=121' silence-too-long
reject '.upload_status.failure_count=2' drain-failures
reject '.spool_drained=false' not-drained

# The runtime health policy (used by both the gate settle loop and the cutover
# verification) must independently accept a healthy production sample and reject
# a stale/unauthorized one.
healthy_sample=$(jq -cn '{schema:"monday.bybit_options_quote.v1",venue:"bybit",category:"option",
  disk_warning:false,spool_warning:false,upload_failure_count:0,
  upload_warning:false,last_upload_error_at:null,connected_workers:1,symbols_expected:1500,
  symbols_seen:1450,active_segment_bytes:0,last_event_at_ms:1750000000000,
  updated_at_ms:1750000001000}')
printf '%s\n' "$healthy_sample" | jq -e \
  --argjson minimum_symbols 500 \
  --argjson minimum_updated_ms 1749999999000 \
  --argjson old_updated_ms 0 \
  --argjson upload_failure_baseline 0 \
  -f "$runtime_policy" >/dev/null
if printf '%s\n' "$healthy_sample" | jq -e \
  --argjson minimum_symbols 500 \
  --argjson minimum_updated_ms 1750000002000 \
  --argjson old_updated_ms 0 \
  --argjson upload_failure_baseline 0 \
  -f "$runtime_policy" >/dev/null; then
  printf '%s\n' 'runtime health policy accepted a stale updated_at_ms' >&2
  exit 1
fi

# Control-plane freshness transition.
# shellcheck disable=SC1090,SC1091
. "$control_lib"

runtime_sample_passes() {
  jq -e --argjson minimum_symbols 500 \
    --argjson minimum_updated_ms 1749999999000 \
    --argjson old_updated_ms 0 \
    --argjson upload_failure_baseline "$1" \
    -f "$runtime_policy" >/dev/null
}
resolved_sample=$(jq '.upload_failure_count=28' <<<"$healthy_sample")
runtime_sample_passes 28 <<<"$resolved_sample"
for filter in \
  '.upload_failure_count=29' \
  '.upload_failure_count=27' \
  '.upload_failure_count=28.5' \
  '.upload_failure_count="28"' \
  '.upload_warning=true' \
  '.last_upload_error_at=1750000000500' \
  'del(.last_upload_error_at)'; do
  if jq "$filter" <<<"$resolved_sample" | runtime_sample_passes 28; then
    printf 'runtime policy accepted an unsafe production sample: %s\n' "$filter" >&2
    exit 1
  fi
done
if runtime_sample_passes 0 <<<"$resolved_sample"; then
  printf 'fresh shadow runtime policy accepted historical failures\n' >&2
  exit 1
fi

# Exercise the actual host drain function with a bounded fake uploader. The
# policy must inspect the post-drain file, not only trust command exit zero.
eval "$(sed -n '/^run_candidate_drain() {/,/^}/p' "$cutover")"
CANONICAL_SPOOL="$tmp_dir/drain-spool"
# shellcheck disable=SC2034 # consumed by the actual host functions loaded above
{
  CANDIDATE_BINARY=/candidate/bybit-options-archiver
  SAFE_PATH=/usr/bin:/bin
  UPLOAD_FAILURE_BASELINE=28
  DRAIN_ENV_KEYS=()
}
mkdir "$CANONICAL_SPOOL"
canonical_spool_paths_safe() { return 0; }
require_empty_segment_spool() { [[ $spool_empty == true ]]; }
runuser() { cp "$tmp_dir/drain-result.json" "$CANONICAL_SPOOL/upload-status.json"; }
spool_empty=true
jq -n '{failure_count:28,last_success_at:20,last_error_at:null,last_error:null}' \
  >"$tmp_dir/resolved-status.json"
cp "$tmp_dir/resolved-status.json" "$tmp_dir/drain-result.json"
run_candidate_drain "$tmp_dir"
[[ $(bybit_options_upload_failure_count "$CANONICAL_SPOOL/upload-status.json") == 28 ]]
for filter in \
  '.failure_count=29' \
  '.failure_count=27' \
  '.failure_count=28.5' \
  'del(.failure_count)' \
  '.last_error_at=10' \
  '.last_error="current failure"' \
  'del(.last_error_at)' \
  'del(.last_error)'; do
  jq "$filter" "$tmp_dir/resolved-status.json" >"$tmp_dir/drain-result.json"
  if run_candidate_drain "$tmp_dir"; then
    printf 'candidate drain accepted unresolved/new upload failures: %s\n' "$filter" >&2
    exit 1
  fi
done
cp "$tmp_dir/resolved-status.json" "$tmp_dir/drain-result.json"
spool_empty=false
if run_candidate_drain "$tmp_dir"; then
  printf 'candidate drain accepted a nonempty raw spool\n' >&2
  exit 1
fi
if bybit_options_upload_status_ready "$tmp_dir/missing-status.json" 28; then
  printf 'production status disappearance erased historical failures\n' >&2
  exit 1
fi
if bybit_options_upload_status_ready "$tmp_dir/missing-status.json" 0; then
  printf 'zero-baseline readiness accepted missing persisted status\n' >&2
  exit 1
fi
printf '{bad json' >"$tmp_dir/bad-status.json"
if bybit_options_upload_failure_count "$tmp_dir/bad-status.json" >/dev/null 2>&1; then
  printf 'malformed production status erased historical failures\n' >&2
  exit 1
fi

# The host runtime readback must also inspect current persisted upload status:
# a cached healthy sample cannot hide an upload failure after that sample.
eval "$(sed -n '/^health_ready_for_release() {/,/^}/p' "$cutover")"
# shellcheck disable=SC2034 # consumed by the actual host function loaded above
RUNTIME_HEALTH_POLICY=$runtime_policy
printf '%s\n' "$resolved_sample" >"$CANONICAL_SPOOL/health.json"
cp "$tmp_dir/resolved-status.json" "$CANONICAL_SPOOL/upload-status.json"
health_ready_for_release 500 1749999999000
for filter in '.failure_count=29' '.last_error="current failure"'; do
  jq "$filter" "$tmp_dir/resolved-status.json" >"$CANONICAL_SPOOL/upload-status.json"
  if health_ready_for_release 500 1749999999000; then
    printf 'cached health hid a new persisted upload failure: %s\n' "$filter" >&2
    exit 1
  fi
done

# Only an explicitly new, empty host can capture an absent status as baseline
# zero. Its candidate uploader must still write status before readiness passes.
eval "$(sed -n '/^capture_upload_failure_baseline() {/,/^}/p' "$cutover")"
CANONICAL_SPOOL="$tmp_dir/bootstrap-spool"
mkdir "$CANONICAL_SPOOL"
# shellcheck disable=SC2034 # consumed by the actual host function loaded above
OLD_MODE=upgrade
if capture_upload_failure_baseline; then
  printf 'upgrade accepted missing cumulative upload history\n' >&2
  exit 1
fi
# shellcheck disable=SC2034 # consumed by the actual host function loaded above
OLD_MODE=new-host
spool_empty=false
if capture_upload_failure_baseline; then
  printf 'new host accepted missing history with a nonempty spool\n' >&2
  exit 1
fi
spool_empty=true
UPLOAD_FAILURE_BASELINE=$(capture_upload_failure_baseline)
[[ $UPLOAD_FAILURE_BASELINE == 0 ]]
if bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" 0; then
  printf 'new host became ready before the candidate persisted status\n' >&2
  exit 1
fi
jq '.failure_count=0 | .last_success_at=null' "$tmp_dir/resolved-status.json" >"$tmp_dir/drain-result.json"
run_candidate_drain "$tmp_dir"
bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" 0
rm "$CANONICAL_SPOOL/upload-status.json"
if bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" 0; then
  printf 'new host remained ready after persisted status disappeared\n' >&2
  exit 1
fi

# Render the real unit template with a digest, then validate exact command
# identity. A bare release-root prefix was previously compared as a whole line.
old_binary="/opt/monday/releases/bybit-options-archiver/$candidate/bybit-options-archiver"
rendered_unit=$(sed "s/@BYBIT_OPTIONS_ARCHIVER_SHA256@/$candidate/g" \
  "$script_dir/bybit-options-archiver.service")
bybit_options_unit_exec_start_matches "$old_binary" "$rendered_unit"
for invalid_unit in \
  'ExecStart=/opt/monday/releases/bybit-options-archiver/' \
  "ExecStart=$old_binary --upload-only" \
  "ExecStart=/opt/monday/releases/bybit-options-archiver/$bundle/bybit-options-archiver" \
  "${rendered_unit}"$'\n''ExecStart=/untrusted/binary' \
  "${rendered_unit}"$'\n'' ExecStart = /untrusted/binary'; do
  if bybit_options_unit_exec_start_matches "$old_binary" "$invalid_unit"; then
    printf 'collector unit accepted a mismatched/duplicate ExecStart\n' >&2
    exit 1
  fi
done

advance=$(bybit_options_observe_health_freshness \
  100 10 0 200 20 120)
[[ $advance == '200 20 10 1' ]] || {
  printf 'unexpected freshness advance: %s\n' "$advance" >&2
  exit 1
}
hold=$(bybit_options_observe_health_freshness \
  200 20 10 200 25 120)
[[ $hold == '200 20 10 0' ]] || {
  printf 'unexpected freshness hold: %s\n' "$hold" >&2
  exit 1
}
if bybit_options_observe_health_freshness \
  200 20 10 199 25 120 >/dev/null 2>&1; then
  printf '%s\n' 'freshness transition accepted a regressing timestamp' >&2
  exit 1
fi
if bybit_options_observe_health_freshness \
  100 10 0 100 200 120 >/dev/null 2>&1; then
  printf '%s\n' 'freshness transition accepted a too-long silence' >&2
  exit 1
fi

# Host script contract: the shadow service must run under the fixed unit name
# the gate policy requires, against the isolated shadow spool, with the
# fail-closed disk/spool env baked in.
# shellcheck disable=SC2016 # literal contract assertion, $shadow_unit must not expand
grep -Fq -- '--unit="$shadow_unit"' "$shadow_gate"
# shellcheck disable=SC2016 # literal contract assertion, $shadow_unit must not expand
grep -Fq 'shadow_unit="bybit-options-shadow"' "$shadow_gate"
grep -Fq 'shadow_unit_full="bybit-options-shadow.service"' "$shadow_gate"
grep -Fq 'SHADOW_SPOOL=/data/monday/spool/bybit-options-shadow' "$shadow_gate"
grep -Fq 'MIN_FREE_GB=20.0' "$shadow_gate"
grep -Fq 'BYBIT_OPTIONS_SPOOL_MAX_BYTES=53687091200' "$shadow_gate"
grep -Fq 'bybit_options_observe_health_freshness' "$shadow_gate"
grep -Fq 'bybit-options-runtime-health-policy.jq' "$shadow_gate"
grep -Fq 'bybit-options-shadow-gate-policy.jq' "$shadow_gate"
grep -Fq 'bybit-options-control-plane-lib.sh' "$shadow_gate"
grep -Fq 'spool_drained:true' "$shadow_gate"
grep -Fq 'upload_status:{failure_count:' "$shadow_gate"
# The receipt jq program references $SHADOW_SPOOL in the service block; the
# argument must actually be passed or the gate dies after a passing
# observation window with "jq: $SHADOW_SPOOL is not defined".
# shellcheck disable=SC2016 # literal contract assertion, $SHADOW_SPOOL must not expand
grep -Fq -- '--arg SHADOW_SPOOL "$SHADOW_SPOOL"' "$shadow_gate"
# The PASSED marker must be the gate.json checksum entry the cutover verifies
# with sha256sum --check --strict, and sha256sum --strict is invalid without
# --check anywhere in the script.
# shellcheck disable=SC2016 # literal contract assertion, variables must not expand
grep -Fq '(cd "$evidence_dir" && sha256sum gate.json) >"$marker_tmp"' "$shadow_gate"
if grep 'sha256sum' "$shadow_gate" | grep -F -- '--strict' | grep -vF -- '--check' | grep -q .; then
  printf 'sha256sum --strict without --check must not appear\n' >&2
  exit 1
fi

# Cutover contract: the candidate must clear a full shadow gate before the
# production unit can be started, and the production env must stay fail-closed.
grep -Fq 'GATE_ROOT=/data/monday/evidence/bybit-options-shadow-gates' "$cutover"
grep -Fq 'PASSED.sha256' "$cutover"
grep -Fq 'RuntimeMaxSec=21600' "$script_dir/bybit-options-archiver.service"
grep -Fq 'RuntimeMaxSec=21600' "$cutover"
grep -Fq 'MIN_FREE_GB 20.0' "$cutover" \
  || grep -Fq 'MIN_FREE_GB=20.0' "$cutover"
grep -Fq 'BYBIT_OPTIONS_SPOOL_MAX_BYTES 53687091200' "$cutover" \
  || grep -Fq 'BYBIT_OPTIONS_SPOOL_MAX_BYTES=53687091200' "$cutover"
grep -Fq 'AssertPathIsMountPoint=/data' "$script_dir/bybit-options-archiver.service"
grep -Fq 'AssertPathIsMountPoint=/data' "$script_dir/bybit-options-upload.service"

printf '%s\n' 'Bybit Options shadow gate tests passed'
