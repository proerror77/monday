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
  upload_warning:false,connected_workers:1,symbols_expected:1500,
  symbols_seen:1450,active_segment_bytes:0,last_event_at_ms:1750000000000,
  updated_at_ms:1750000001000}')
printf '%s\n' "$healthy_sample" | jq -e \
  --argjson minimum_symbols 500 \
  --argjson minimum_updated_ms 1749999999000 \
  --argjson old_updated_ms 0 \
  -f "$runtime_policy" >/dev/null
if printf '%s\n' "$healthy_sample" | jq -e \
  --argjson minimum_symbols 500 \
  --argjson minimum_updated_ms 1750000002000 \
  --argjson old_updated_ms 0 \
  -f "$runtime_policy" >/dev/null; then
  printf '%s\n' 'runtime health policy accepted a stale updated_at_ms' >&2
  exit 1
fi

# Control-plane freshness transition.
# shellcheck disable=SC1090,SC1091
. "$control_lib"
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
