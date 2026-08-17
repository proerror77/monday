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
    {schema:"binance.usdm_reference_manifest.v2",venue:"binance_usdm",
      dataset:"reference",data_schema:"binance.usdm_reference.v3",format:"ndjson",
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
      mark_index_funding:{observations:500,
        first_event_time_ms:1999999999000,last_event_time_ms:2000000000000,
        first_available_at_ns:1999999999500000000,
        last_available_at_ns:1999999999800000000,max_gap_ns:1000000},
      open_interest:{observations:500,
        first_event_time_ms:1999999995000,last_event_time_ms:1999999999000,
        first_available_at_ns:1999999999600000000,
        last_available_at_ns:1999999999900000000,max_gap_ns:1000000}}' >"$manifest"
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
write_artifact "$root" "$((start_ns + 4000000000))" post-window-health
latest_manifest=$(find "$spool" -name reference.ndjson.manifest.json -type f \
  | sort | tail -n 1)
latest_data=${latest_manifest%.manifest.json}
latest_data_sha=$(jq -r .sha256 "$latest_manifest")
latest_manifest_sha=$(sha256sum "$latest_manifest" | awk '{print $1}')
jq --arg path "$latest_data" --arg data "$latest_data_sha" \
  --arg manifest "$latest_manifest_sha" \
  --argjson success "$((start_ns + 4500000000))" '
  .data_path=$path | .data_sha256=$data | .manifest_sha256=$manifest
  | .last_attempt_at_ns=$success | .last_success_at_ns=$success
' "$spool/health.json" >"$spool/health.tmp" && mv "$spool/health.tmp" "$spool/health.json"
gate_json=$(run_gate)
jq -e '
  .schema == "monday.binance_usdm_reference_shadow_gate.v1"
  and .passed == false and .production_eligible == false
  and .duration_seconds == 2 and .artifact_count == 4
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

production_collector="$script_dir/binance-usdm-reference-collector.service"
production_upload_service="$script_dir/binance-usdm-reference-upload.service"
production_upload_timer="$script_dir/binance-usdm-reference-upload.timer"
production_upload_env="$script_dir/binance-usdm-reference-upload.env"
cutover="$script_dir/binance-usdm-reference-cutover.sh"

[[ -x $cutover ]] || {
  printf '%s\n' 'missing executable Binance USD-M reference cutover' >&2
  exit 1
}
grep -Fq 'must run as root' "$cutover"
grep -Fq 'flock -n 9' "$cutover"
grep -Fq 'mountpoint -q /data' "$cutover"
grep -Fq 'secure_regular_file' "$cutover"
grep -Fq 'PASSED.sha256' "$cutover"
grep -Fq 'binance-usdm-reference-shadow-gates' "$cutover"
grep -Fq '/opt/monday/bin/binance-usdm-reference-collector' "$cutover"
grep -Fq '/opt/monday/bin/binance-usdm-reference-upload' "$cutover"
grep -Fq 'monday.binance_usdm_reference_cutover.v1' "$cutover"
grep -Fq 'new-host' "$cutover"
grep -Fq 'OLD_MODE=upgrade' "$cutover"
grep -Fq 'STEP=drain-v2-with-old-uploader' "$cutover"
grep -Fq "run_uploader \"\$OLD_UPLOADER\"" "$cutover"
grep -Fq 'V2 backlog remains after the old uploader drain' "$cutover"
grep -Fq 'restore_old_production' "$cutover"
grep -Fq 'previous-release-restored' "$cutover"
grep -Fq "validate_upload_env \"\$UPLOAD_ENV\"" "$cutover"
grep -Fq 'validate_old_release_uploader' "$cutover"
grep -Fq 'previous_uploader_sha256:' "$cutover"
if grep -Fq 'previous_uploader_release_sha256' "$cutover"; then
  printf '%s\n' 'cutover receipt retained the obsolete standalone uploader release identity' >&2
  exit 1
fi
grep -Fq 'CANDIDATE_MAY_HAVE_WRITTEN=1' "$cutover"
grep -Fq "controller: \$controller" "$cutover"
grep -Fq "runtime_matches_collector \"\$OLD_COLLECTOR\" true false" "$cutover"
grep -Fq "systemctl disable \"\$COLLECTOR_UNIT\" \"\$UPLOAD_TIMER\"" "$cutover"
grep -Fq "! systemctl is-active --quiet \"\$UPLOAD_SERVICE\"" "$cutover"
grep -Fq 'quarantine_reference_staging' "$cutover"

# A long-lived baseline accumulates planned RuntimeMaxSec restarts.
# Only the freshly started candidate and rollback process must remain unrestarted.
eval "$(awk '/^runtime_matches_collector\(\) \{/{copy=1} copy{print} copy && /^}/{exit}' "$cutover")"
COLLECTOR_UNIT=collector.service
UPLOAD_TIMER=upload.timer
runtime_expected=/release/binance-usdm-reference-collector
runtime_restart_count=42
# shellcheck disable=SC2317,SC2329 # Invoked by the extracted production validator.
systemctl() {
  case "$1" in
    is-active|is-enabled) return 0 ;;
    show)
      case " $* " in
        *' --property=NRestarts '*) printf '%s\n' "$runtime_restart_count" ;;
        *' --property=MainPID '*) printf '4242\n' ;;
        *) return 1 ;;
      esac
      ;;
    *) return 1 ;;
  esac
}
# shellcheck disable=SC2317,SC2329 # Invoked by the extracted production validator.
readlink() { printf '%s\n' "$runtime_expected"; }
# shellcheck disable=SC2218 # Function is extracted above with eval.
runtime_matches_collector "$runtime_expected" true false
if runtime_matches_collector "$runtime_expected" true true; then
  printf '%s\n' 'fresh candidate accepted a non-zero restart count' >&2
  exit 1
fi
runtime_restart_count=0
# shellcheck disable=SC2218 # Function is extracted above with eval.
runtime_matches_collector "$runtime_expected" true true
unset -f systemctl readlink runtime_matches_collector

# A release installed by this cutover keeps collector and uploader together.
# Exercise the real upgrade identity validator against that first-cutover shape.
eval "$(awk '/^validate_old_release_uploader\(\) \{/{copy=1} copy{print} copy && /^}/{exit}' "$cutover")"
first_release_root="$tmp_dir/first-cutover-releases"
first_release="$first_release_root/$candidate"
mkdir -p "$first_release"
cp "$collector_fixture" "$first_release/binance-usdm-reference-collector"
printf '#!/bin/sh\nexit 0\n' >"$first_release/binance-usdm-reference-upload"
chmod 0755 "$first_release/binance-usdm-reference-upload"
first_uploader="$first_release/binance-usdm-reference-upload"
first_uploader_sha=$(sha256sum "$first_uploader" | awk '{print $1}')
printf '%s  binance-usdm-reference-upload\n' "$first_uploader_sha" \
  >"$first_release/binance-usdm-reference-upload.sha256"
# shellcheck disable=SC2034 # Read by the extracted production validator.
RELEASE_ROOT=$first_release_root
# shellcheck disable=SC2034 # Read by the extracted production validator.
OLD_RELEASE_SHA256=$candidate
OLD_UPLOADER=$first_uploader
OLD_UPLOADER_SHA256=
# shellcheck disable=SC2317,SC2329 # Invoked by the extracted production validator.
secure_regular_file() {
  [[ -f $1 && ! -L $1 && ${untrusted_path:-} != "$1" ]] \
    || fail "untrusted fixture: $1"
}
fail() { printf '%s\n' "$*" >&2; exit 1; }
validate_old_release_uploader
[[ $OLD_UPLOADER_SHA256 == "$first_uploader_sha" ]]

if (untrusted_path="$first_release/binance-usdm-reference-upload.sha256"; \
  validate_old_release_uploader >/dev/null 2>&1); then
  printf '%s\n' 'upgrade preflight accepted an untrusted uploader sidecar' >&2
  exit 1
fi

legacy_release="$tmp_dir/legacy-uploader-release/$first_uploader_sha"
mkdir -p "$legacy_release"
cp "$OLD_UPLOADER" "$legacy_release/binance-usdm-reference-upload"
# shellcheck disable=SC2030 # Deliberately isolate the rejected path fixture.
if (OLD_UPLOADER="$legacy_release/binance-usdm-reference-upload"; \
  validate_old_release_uploader >/dev/null 2>&1); then
  printf '%s\n' 'upgrade preflight accepted a non-collocated uploader' >&2
  exit 1
fi

printf 'tampered\n' >>"$first_uploader"
if (validate_old_release_uploader >/dev/null 2>&1); then
  printf '%s\n' 'upgrade preflight accepted uploader drift from its sidecar' >&2
  exit 1
fi

drain_line=$(grep -n 'STEP=drain-v2-with-old-uploader' "$cutover" | cut -d: -f1)
switch_line=$(grep -n 'STEP=switch-production-symlink' "$cutover" | cut -d: -f1)
(( drain_line < switch_line ))

# Preserve an interrupted V2 staging directory outside the canonical lake.
eval "$(awk '/^quarantine_reference_staging\(\) \{/{copy=1} copy{print} copy && /^}/{exit}' "$cutover")"
CANONICAL_SPOOL="$tmp_dir/staging-behavior/spool"
EVIDENCE_DIR="$tmp_dir/staging-behavior/evidence"
staging="$CANONICAL_SPOOL/lake/raw/venue=binance_usdm/dataset=reference/date=2033-05-18/hour=03/.reference-staging.fixture"
mkdir -p "$staging" "$EVIDENCE_DIR"
printf 'partial\n' >"$staging/reference.ndjson"
quarantine_reference_staging
[[ ! -e $staging ]]
grep -Fxq 'partial' "$EVIDENCE_DIR/quarantined-v2-staging/staging-1/reference.ndjson"
grep -Fq "$staging" "$EVIDENCE_DIR/quarantined-v2-staging/paths.tsv"

# Exercise the actual rollback function with production effects mocked. Once
# candidate execution is possible, a failed V3 drain must prevent V2 restore.
eval "$(awk '/^restore_old_production\(\) \{/{copy=1} copy{print} copy && /^}/{exit}' "$cutover")"
EVIDENCE_DIR="$tmp_dir/rollback-behavior"
rollback="$EVIDENCE_DIR/rollback-assets"
mkdir -p "$rollback"
for asset in binance-usdm-reference-collector.service \
  binance-usdm-reference-upload.service binance-usdm-reference-upload.timer \
  binance-usdm-reference-upload.env; do
  printf '%s\n' "$asset" >"$rollback/$asset"
done
(
  cd "$rollback"
  sha256sum binance-usdm-reference-collector.service \
    binance-usdm-reference-upload.service binance-usdm-reference-upload.timer \
    binance-usdm-reference-upload.env > rollback-assets.sha256
)
# shellcheck disable=SC2034
ROLLBACK_ASSETS_SHA256=$(sha256sum "$rollback/rollback-assets.sha256" | awk '{print $1}')
# shellcheck disable=SC2034
CANDIDATE_MAY_HAVE_WRITTEN=1
# shellcheck disable=SC2034
CANDIDATE_UPLOADER=/candidate/uploader
OLD_COLLECTOR=/old/collector
OLD_UPLOADER=/old/uploader
COLLECTOR_LINK=/collector-link
# shellcheck disable=SC2034
UPLOADER_LINK=/uploader-link
# shellcheck disable=SC2034
UPLOAD_ENV=/upload-env
# shellcheck disable=SC2034
COLLECTOR_UNIT=collector.service
# shellcheck disable=SC2034
UPLOAD_SERVICE=upload.service
# shellcheck disable=SC2034
UPLOAD_TIMER=upload.timer
# shellcheck disable=SC2034
HEALTH_TIMEOUT_SECONDS=1
rollback_trace="$tmp_dir/rollback-trace"
secure_regular_file() { :; }
run_uploader() { printf 'drain %s\n' "$1" >>"$rollback_trace"; return "${drain_result:-0}"; }
require_empty_lake() { printf 'empty-check\n' >>"$rollback_trace"; }
atomic_install() { printf 'install %s\n' "$3" >>"$rollback_trace"; }
atomic_symlink() { printf 'symlink %s\n' "$2" >>"$rollback_trace"; }
systemctl() { :; }
health_ready_for_release() { printf 'health-readback\n' >>"$rollback_trace"; }
runtime_matches_collector() {
  printf 'runtime %s %s %s\n' "$1" "$2" "$3" >>"$rollback_trace"
}
readlink() {
  local path=${!#}
  [[ $path == "$COLLECTOR_LINK" ]] && printf '%s\n' "$OLD_COLLECTOR" \
    || printf '%s\n' "$OLD_UPLOADER"
}

drain_result=1
if restore_old_production; then
  printf '%s\n' 'rollback restored V2 after the candidate drain failed' >&2
  exit 1
fi
[[ $(wc -l <"$rollback_trace") -eq 1 ]]
grep -Fxq 'drain /candidate/uploader' "$rollback_trace"

: >"$rollback_trace"
drain_result=0
restore_old_production
[[ $(sed -n '1p' "$rollback_trace") == 'drain /candidate/uploader' ]]
[[ $(sed -n '2p' "$rollback_trace") == 'empty-check' ]]
grep -Fq 'symlink /collector-link' "$rollback_trace"
grep -Fq 'symlink /uploader-link' "$rollback_trace"
grep -Fq 'health-readback' "$rollback_trace"
grep -Fq 'runtime /old/collector false true' "$rollback_trace"
grep -Fq 'runtime /old/collector true true' "$rollback_trace"

grep -Fxq 'ConditionPathIsMountPoint=/data' "$production_collector"
grep -Fxq 'ExecStart=/opt/monday/bin/binance-usdm-reference-collector --output-root /data/monday/spool/binance-usdm-reference --interval-seconds 30 --request-timeout-seconds 10 --oi-concurrency 8 --max-staleness-ms 30000' \
  "$production_collector"
grep -Fxq 'ReadWritePaths=/data/monday/spool/binance-usdm-reference' "$production_collector"
grep -Fxq 'WantedBy=multi-user.target' "$production_collector"
grep -Fxq 'Type=oneshot' "$production_upload_service"
grep -Fxq 'ConditionPathIsMountPoint=/data' "$production_upload_service"
grep -Fxq 'EnvironmentFile=/etc/monday/binance-usdm-reference-upload.env' \
  "$production_upload_service"
grep -Fxq 'ExecStart=/opt/monday/bin/binance-usdm-reference-upload --output-root /data/monday/spool/binance-usdm-reference' \
  "$production_upload_service"
grep -Fxq 'ReadWritePaths=/data/monday/spool/binance-usdm-reference' \
  "$production_upload_service"
grep -Fxq 'Unit=binance-usdm-reference-upload.service' "$production_upload_timer"
grep -Fxq 'OnUnitActiveSec=5min' "$production_upload_timer"
grep -Fxq 'WantedBy=timers.target' "$production_upload_timer"
grep -Fxq 'OSS_BUCKET=monday-lob-apne1-1045353359' "$production_upload_env"
grep -Fxq 'OSS_ENDPOINT=oss-ap-northeast-1-internal.aliyuncs.com' "$production_upload_env"
grep -Fxq 'OSS_REGION=ap-northeast-1' "$production_upload_env"
grep -Fxq 'ALIYUN_PROFILE=ecs-role' "$production_upload_env"
grep -Fxq 'OSS_COPY_TIMEOUT_SECONDS=300' "$production_upload_env"

printf '%s\n' 'Binance USD-M reference host gate tests passed'
