#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
runner="$script_dir/binance-usdm-reference-shadow-gate.sh"
policy="$script_dir/binance-usdm-reference-shadow-gate-policy.jq"
service="$script_dir/binance-usdm-reference-collector-shadow@.service"
start_ns=2000000000000000000
tmp_dir=$(mktemp -d)
tmp_dir=$(cd -- "$tmp_dir" && pwd -P)
trap 'rm -rf "$tmp_dir"' EXIT
collector_fixture="$tmp_dir/binance-usdm-reference-collector"
printf '#!/bin/sh\nexit 0\n' >"$collector_fixture"
chmod 0755 "$collector_fixture"
candidate=$(sha256sum "$collector_fixture" | awk '{print $1}')

[[ -x $runner ]] || {
  printf '%s\n' 'missing executable Binance USD-M reference host gate' >&2
  exit 1
}

write_artifact() {
  local root=$1 observed_ns=$2 label=$3
  local batch data data_sha manifest
  batch="$root/data/monday/spool/binance-usdm-reference-shadow/$candidate/lake/raw/venue=binance_usdm/dataset=reference/date=2033-05-18/hour=03/batch=$observed_ns"
  mkdir -p "$batch"
  data="$batch/reference.ndjson"
  printf '{"fixture":"%s"}\n' "$label" >"$data"
  data_sha=$(sha256sum "$data" | awk '{print $1}')
  manifest="$data.manifest.json"
  jq -S -n --arg data_sha "$data_sha" --argjson observed "$observed_ns" '
    {schema:"binance.usdm_reference_manifest.v1",venue:"binance_usdm",
      dataset:"reference",data_schema:"binance.usdm_reference.v2",format:"ndjson",
      source_origin:"https://fapi.binance.com",
      source_endpoints:["https://fapi.binance.com/fapi/v1/time",
        "https://fapi.binance.com/fapi/v1/exchangeInfo",
        "https://fapi.binance.com/fapi/v1/premiumIndex",
        "https://fapi.binance.com/fapi/v1/openInterest"],
      file:"reference.ndjson",bytes:21,sha256:$data_sha,rows:1500,
      observed_at_ns:$observed,max_staleness_ms:30000,
      coverage:{active_contracts:500,metadata_observations:500,
        mark_index_funding_observations:500,open_interest_observations:500,
        stale_metadata:0,stale_mark_index_funding:0,stale_open_interest:0,
        api_error_count:0},
      time_bounds:{min_source_time_ms:1999999999000,
        max_source_time_ms:2000000000000,
        min_received_at_ns:1999999999500000000,
        max_received_at_ns:1999999999900000000}}' >"$manifest"
  printf '%s\n' "$data_sha" >"$data._SUCCESS"
}

setup_fixture() {
  local name=$1
  root="$tmp_dir/$name"
  release="$root/opt/monday/releases/binance-usdm-reference-collector/$candidate"
  deployment="$release/deployment"
  spool="$root/data/monday/spool/binance-usdm-reference-shadow/$candidate"
  evidence="$root/data/monday/evidence/binance-usdm-reference-shadow-gates"
  fake_bin="$root/fake-bin"
  state="$root/systemctl-state.json"
  mkdir -p "$deployment" "$spool" "$fake_bin" "$root/etc/systemd/system" \
    "$root/proc/4242" "$root/run/lock" "$evidence"

  cp "$collector_fixture" "$release/binance-usdm-reference-collector"
  chmod 0755 "$release/binance-usdm-reference-collector"
  cat >"$release/binance-usdm-reference-artifact-verifier" <<'VERIFIER'
#!/usr/bin/env bash
set -euo pipefail
while (($#)); do
  case "$1" in
    --data-path) data=$2; shift 2 ;;
    --data-sha256) data_sha=$2; shift 2 ;;
    --manifest-sha256) manifest_sha=$2; shift 2 ;;
    *) exit 2 ;;
  esac
done
manifest="$data.manifest.json"
success="$data._SUCCESS"
if [[ ! -f $data || -L $data || ! -f $manifest || -L $manifest \
  || ! -f $success || -L $success \
  || $(sha256sum "$data" | awk '{print $1}') != "$data_sha" \
  || $(sha256sum "$manifest" | awk '{print $1}') != "$manifest_sha" \
  || $(cat "$success") != "$data_sha" ]]; then
  exit 1
fi
jq -c --arg path "$data" --arg data "$data_sha" --arg manifest "$manifest_sha" '
  {schema:"monday.binance_usdm_reference_artifact_verification.v1",
    data_path:$path,data_sha256:$data,manifest_sha256:$manifest,
    metadata_observations:.coverage.metadata_observations,
    mark_index_funding_observations:.coverage.mark_index_funding_observations,
    open_interest_observations:.coverage.open_interest_observations,
    content_rows_verified:true}' "$manifest"
VERIFIER
  chmod 0755 "$release/binance-usdm-reference-artifact-verifier"
  cp "$runner" "$policy" "$service" "$deployment/"
  cp "$service" "$root/etc/systemd/system/"
  (
    cd "$deployment"
    sha256sum binance-usdm-reference-shadow-gate.sh \
      binance-usdm-reference-shadow-gate-policy.jq \
      binance-usdm-reference-collector-shadow@.service \
      >"$release/binance-usdm-reference-control-assets.sha256"
  )
  printf 'fixture archive\n' >"$release/binance-usdm-reference-control.tar.gz"
  local collector_sha verifier_sha manifest_sha archive_sha
  collector_sha=$(sha256sum "$release/binance-usdm-reference-collector" | awk '{print $1}')
  [[ $collector_sha == "$candidate" ]]
  verifier_sha=$(sha256sum "$release/binance-usdm-reference-artifact-verifier" | awk '{print $1}')
  manifest_sha=$(sha256sum "$release/binance-usdm-reference-control-assets.sha256" | awk '{print $1}')
  archive_sha=$(sha256sum "$release/binance-usdm-reference-control.tar.gz" | awk '{print $1}')
  jq -S -n --arg candidate "$candidate" --arg verifier "$verifier_sha" \
    --arg manifest "$manifest_sha" --arg archive "$archive_sha" \
    '{schema:"monday.binance_usdm_reference_release.v1",source_revision:("c"*40),
      candidate:{file:"binance-usdm-reference-collector",sha256:$candidate},
      verifier:{file:"binance-usdm-reference-artifact-verifier",sha256:$verifier},
      control_manifest:{file:"binance-usdm-reference-control-assets.sha256",sha256:$manifest},
      control_archive:{file:"binance-usdm-reference-control.tar.gz",sha256:$archive}}' \
    >"$release/release.json"

  write_artifact "$root" "$start_ns" first
  write_artifact "$root" "$((start_ns + 1000000000))" second
  write_artifact "$root" "$((start_ns + 2000000000))" third
  latest_manifest=$(find "$spool" -name reference.ndjson.manifest.json -type f \
    | sort | tail -n 1)
  latest_data=${latest_manifest%.manifest.json}
  latest_data_sha=$(jq -r .sha256 "$latest_manifest")
  latest_manifest_sha=$(sha256sum "$latest_manifest" | awk '{print $1}')
  jq -S -n --arg path "$latest_data" --arg data "$latest_data_sha" \
    --arg manifest "$latest_manifest_sha" --argjson success "$((start_ns + 2500000000))" '
    {schema:"binance.usdm_reference_health.v1",status:"healthy",
      source_origin:"https://fapi.binance.com",last_attempt_at_ns:$success,
      last_success_at_ns:$success,api_error_count:0,total_api_errors:0,
      artifact_error_count:0,total_artifact_errors:0,last_error:null,
      data_path:$path,data_sha256:$data,manifest_sha256:$manifest}' >"$spool/health.json"

  jq -n --arg fragment "$root/etc/systemd/system/binance-usdm-reference-collector-shadow@.service" \
    '{fragment:$fragment,pid:"4242",restarts:"0",invocation:("f"*32),
      replacement:("0"*32),replace_after_first:false}' >"$state"
  cat >"$fake_bin/systemctl" <<'SYSTEMCTL'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  is-active) exit 0 ;;
  show)
    property=
    for arg in "$@"; do
      case "$arg" in --property=*) property=${arg#--property=} ;; esac
    done
    case "$property" in
      FragmentPath) jq -r .fragment "$FAKE_SYSTEMCTL_STATE" ;;
      DropInPaths) printf '\n' ;;
      MainPID) jq -r .pid "$FAKE_SYSTEMCTL_STATE" ;;
      NRestarts) jq -r .restarts "$FAKE_SYSTEMCTL_STATE" ;;
      InvocationID)
        count_file="$FAKE_SYSTEMCTL_STATE.count"
        count=0; [[ ! -f $count_file ]] || count=$(cat "$count_file")
        if [[ $(jq -r .replace_after_first "$FAKE_SYSTEMCTL_STATE") == true && $count -gt 0 ]]; then
          jq -r .replacement "$FAKE_SYSTEMCTL_STATE"
        else
          jq -r .invocation "$FAKE_SYSTEMCTL_STATE"
        fi
        printf '%s\n' "$((count + 1))" >"$count_file"
        ;;
      *) exit 2 ;;
    esac
    ;;
  *) exit 2 ;;
esac
SYSTEMCTL
  chmod 0755 "$fake_bin/systemctl"
  ln -s "$release/binance-usdm-reference-collector" "$root/proc/4242/exe"
  printf '%s\0' "$release/binance-usdm-reference-collector" --output-root "$spool" \
    --interval-seconds 30 --request-timeout-seconds 10 --oi-concurrency 8 \
    --max-staleness-ms 30000 >"$root/proc/4242/cmdline"
}

run_gate() {
  MONDAY_ALLOW_REFERENCE_GATE_TEST_MODE=1 \
  MONDAY_REFERENCE_GATE_TEST_ROOT="$root" \
  MONDAY_REFERENCE_GATE_TEST_SECONDS=2 \
  MONDAY_REFERENCE_GATE_TEST_GRACE_SECONDS="${test_grace_seconds:-1}" \
  MONDAY_REFERENCE_GATE_TEST_NOW_NS="$start_ns" \
  FAKE_SYSTEMCTL_STATE="$state" \
  PATH="$fake_bin:$PATH" \
    "$deployment/binance-usdm-reference-shadow-gate.sh" "$candidate"
}

expect_failure() {
  local label=$1
  if run_gate >"$root/$label.stdout" 2>"$root/$label.stderr"; then
    printf 'host gate accepted invalid fixture: %s\n' "$label" >&2
    exit 1
  fi
}

setup_fixture positive
gate_json=$(run_gate)
jq -e '
  .schema == "monday.binance_usdm_reference_shadow_gate.v1"
  and .passed == false and .production_eligible == false
  and .duration_seconds == 2 and .artifact_count == 3
  and .service.active == true and .service.restart_count == 0
  and all(.artifacts[]; .canonical_readback and .content_rows_verified)
' "$gate_json" >/dev/null
[[ ! -e ${gate_json%/gate.json}/PASSED.sha256 ]]

if grep -Eq 'systemctl[[:space:]]+(start|stop|restart|enable|disable|daemon-reload)' "$runner"; then
  printf '%s\n' 'host gate must not mutate systemd service state' >&2
  exit 1
fi

setup_fixture tampered
printf 'tampered\n' >>"$latest_data"
expect_failure tampered-data

setup_fixture symlink
mv "$latest_manifest" "$latest_manifest.real"
ln -s "$latest_manifest.real" "$latest_manifest"
expect_failure symlinked-manifest

setup_fixture invocation
jq '.replace_after_first=true' "$state" >"$state.tmp" && mv "$state.tmp" "$state"
expect_failure replaced-invocation

setup_fixture health
jq '.total_api_errors=1' "$spool/health.json" >"$spool/health.tmp" \
  && mv "$spool/health.tmp" "$spool/health.json"
expect_failure historical-api-error

setup_fixture discontinuity
second_manifest=$(find "$spool" -name reference.ndjson.manifest.json -type f | sort | sed -n '2p')
jq ".observed_at_ns=$((start_ns + 91000000000))" "$second_manifest" \
  >"$second_manifest.tmp" && mv "$second_manifest.tmp" "$second_manifest"
third_manifest=$(find "$spool" -name reference.ndjson.manifest.json -type f | sort | tail -n 1)
jq ".observed_at_ns=$((start_ns + 92000000000))" "$third_manifest" \
  >"$third_manifest.tmp" && mv "$third_manifest.tmp" "$third_manifest"
test_grace_seconds=100
expect_failure artifact-discontinuity
unset test_grace_seconds

setup_fixture verifier
printf 'drift\n' >>"$release/binance-usdm-reference-artifact-verifier"
expect_failure verifier-sha-drift

printf '%s\n' 'Binance USD-M reference host gate tests passed'
