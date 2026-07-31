#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
policy="$script_dir/binance-usdm-reference-shadow-gate-policy.jq"
service="$script_dir/binance-usdm-reference-collector-shadow@.service"
workflow="$script_dir/../../.github/workflows/acr-publish.yml"
candidate=$(printf 'a%.0s' {1..64})
bundle=$(printf 'b%.0s' {1..64})
source=$(printf 'c%.0s' {1..40})
data_sha=$(printf 'd%.0s' {1..64})
manifest_sha=$(printf 'e%.0s' {1..64})
invocation=$(printf 'f%.0s' {1..32})
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

artifact=$(jq -cn \
  --arg data "$data_sha" --arg manifest "$manifest_sha" \
  '{canonical_readback:true,dataset:"reference",venue:"binance_usdm",
    manifest_schema:"binance.usdm_reference_manifest.v1",
    data_schema:"binance.usdm_reference.v2",
    source_origin:"https://fapi.binance.com",
    max_staleness_ms:30000,
    source_endpoints:["https://fapi.binance.com/fapi/v1/time",
      "https://fapi.binance.com/fapi/v1/exchangeInfo",
      "https://fapi.binance.com/fapi/v1/premiumIndex",
      "https://fapi.binance.com/fapi/v1/openInterest"],
    data_sha256:$data,manifest_sha256:$manifest,success_sha256:$data,
    content_rows_verified:true,observed_at_ns:1700000000000000000,
    time_bounds:{min_source_time_ms:1699999999000,max_source_time_ms:1700000000000,
      min_received_at_ns:1699999999500000000,max_received_at_ns:1699999999900000000},
    coverage:{active_contracts:500,metadata_observations:500,
      mark_index_funding_observations:500,open_interest_observations:500,
      stale_metadata:0,stale_mark_index_funding:0,stale_open_interest:0,
    api_error_count:0}}')
artifacts=$(jq -cn --argjson artifact "$artifact" \
  '[range(0; 41) as $index
    | ($index | tostring) as $suffix
    | ("0" * (64 - ($suffix | length)) + $suffix) as $data
    | ("a" * (64 - ($suffix | length)) + $suffix) as $manifest
    | $artifact + {
        observed_at_ns:(1700000000000000000 + ($index * 90000000000)),
        data_sha256:$data,manifest_sha256:$manifest,success_sha256:$data}]')
jq -n \
  --arg candidate "$candidate" --arg bundle "$bundle" --arg source "$source" \
  --arg invocation "$invocation" --argjson artifacts "$artifacts" \
  '{schema:"monday.binance_usdm_reference_shadow_gate.v1",
    candidate_sha256:$candidate,deployment_bundle_sha256:$bundle,
    deployment_source_revision:$source,passed:true,production_eligible:true,
    duration_seconds:3600,
    service:{unit:("binance-usdm-reference-collector-shadow@"+$candidate+".service"),
      active:true,restart_count:0,binary_sha256:$candidate,
      invocation_id_start:$invocation,invocation_id_end:$invocation},
    health:{schema:"binance.usdm_reference_health.v1",status:"healthy",
      source_origin:"https://fapi.binance.com",api_error_count:0,total_api_errors:0,
      artifact_error_count:0,total_artifact_errors:0,
      last_success_at_ns:1700003600000000000,
      data_sha256:$artifacts[-1].data_sha256,
      manifest_sha256:$artifacts[-1].manifest_sha256},
    artifact_count:($artifacts|length),max_artifact_gap_ns:90000000000,
    artifacts:$artifacts}' \
  >"$tmp_dir/gate.json"

gate() {
  jq -e --arg candidate_sha256 "$candidate" \
    --arg deployment_bundle_sha256 "$bundle" \
    --arg deployment_source_revision "$source" \
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
reject '.duration_seconds=3601' insufficient-artifact-span
reject '.service.restart_count=1' restarted
reject '.service.invocation_id_end=("0"*32)' replaced-invocation
reject 'del(.service.invocation_id_start)' missing-invocation
reject '.health.api_error_count=1' api-error
reject '.health.total_artifact_errors=1' artifact-error
reject '.artifacts[1].coverage.stale_open_interest=1' stale-oi
reject '.artifacts[1].coverage.active_contracts=399
  | .artifacts[1].coverage.metadata_observations=399
  | .artifacts[1].coverage.mark_index_funding_observations=399
  | .artifacts[1].coverage.open_interest_observations=399' small-universe
reject '.artifacts[1].max_staleness_ms=300000' relaxed-staleness
reject 'del(.artifacts[1].max_staleness_ms)' missing-staleness
reject '.artifacts[2].success_sha256=("9"*64)' bad-success
reject '.artifacts[1].canonical_readback=false' no-readback
reject '.artifacts[1].observed_at_ns=1700000200000000000' discontinuous
reject '.artifacts[0].source_endpoints[3]="https://example.com/openInterest"' wrong-endpoint

grep -Fq 'ExecStart=/opt/monday/releases/binance-usdm-reference-collector/%i/binance-usdm-reference-collector' "$service"
grep -Fq -- '--output-root /data/monday/spool/binance-usdm-reference-shadow/%i' "$service"
grep -Fq 'ReadWritePaths=/data/monday/spool/binance-usdm-reference-shadow/%i' "$service"
grep -Fq 'binance-usdm-reference-collector-shadow@.service' "$workflow"
grep -Fq 'binance-usdm-reference-shadow-gate.sh' "$workflow"
grep -Fq 'binance-usdm-reference-shadow-gate-policy.jq' "$workflow"
grep -Fq 'binance-usdm-reference-control-assets.sha256' "$workflow"
grep -Fq 'binance-usdm-reference-control.tar.gz' "$workflow"
shared_assets=$(sed -n '/^          control_assets=(/,/^          )/p' "$workflow")
reference_assets=$(sed -n '/^          reference_control_assets=(/,/^          )/p' "$workflow")
reference_release=$(sed -n \
  '/schema:"monday.binance_usdm_reference_release.v1"/,/> binance-usdm-reference-release.json/p' \
  "$workflow")
if grep -Fq 'binance-usdm-reference-' <<<"$shared_assets"; then
  printf '%s\n' 'Reference assets must not change the Polymarket control bundle' >&2
  exit 1
fi
grep -Fq 'binance-usdm-reference-collector-shadow@.service' <<<"$reference_assets"
grep -Fq 'binance-usdm-reference-shadow-gate.sh' <<<"$reference_assets"
grep -Fq 'binance-usdm-reference-shadow-gate-policy.jq' <<<"$reference_assets"
grep -Fq 'control_manifest:{file:"binance-usdm-reference-control-assets.sha256"' \
  <<<"$reference_release"
grep -Fq 'control_archive:{file:"binance-usdm-reference-control.tar.gz"' \
  <<<"$reference_release"
if grep -Fq 'polymarket-raw-ops-control' <<<"$reference_release"; then
  printf '%s\n' 'Reference release must not point at the Polymarket control bundle' >&2
  exit 1
fi

reference_production_assets=$(sed -n \
  '/^          reference_production_assets=(/,/^          )/p' "$workflow")
[[ $(grep -cE '^            binance-usdm-reference-' <<<"$reference_assets") == 3 ]] || {
  printf '%s\n' 'Reference shadow bundle must keep exactly three assets' >&2
  exit 1
}
for asset in \
  binance-usdm-reference-collector.service \
  binance-usdm-reference-upload.service \
  binance-usdm-reference-upload.timer \
  binance-usdm-reference-upload.env \
  binance-usdm-reference-cutover.sh; do
  grep -Fq "$asset" <<<"$reference_production_assets"
  [[ -f $script_dir/$asset ]] || {
    printf 'reference production asset is missing: %s\n' "$asset" >&2
    exit 1
  }
  if grep -Fq "$asset" <<<"$reference_assets" || grep -Fq "$asset" <<<"$shared_assets"; then
    printf '%s\n' \
      'Reference production assets must stay out of the shadow and Polymarket bundles' >&2
    exit 1
  fi
done
[[ $(grep -cE '^            binance-usdm-reference-' <<<"$reference_production_assets") == 5 ]] \
  || {
    printf '%s\n' 'Reference production bundle must contain exactly five assets' >&2
    exit 1
  }
grep -Fq 'binance-usdm-reference-production-control-assets.sha256' "$workflow"
grep -Fq 'binance-usdm-reference-production-control.tar.gz' "$workflow"

printf '%s\n' 'Binance USD-M reference shadow gate tests passed'
