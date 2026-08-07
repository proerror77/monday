#!/usr/bin/env bash
set -Eeuo pipefail

umask 027
export LC_ALL=C

readonly REQUIRED_DURATION_SECONDS=3600
readonly HEALTH_SETTLE_SECONDS=2400
readonly MAX_HEALTH_SILENCE_SECONDS=120
readonly MINIMUM_SYMBOLS=500
readonly RELEASE_ROOT=/opt/monday/releases/bybit-options-archiver
readonly SHADOW_BINARY=/opt/monday/bin/bybit-options-archiver-shadow
readonly EVIDENCE_ROOT=/data/monday/evidence/bybit-options-shadow-gates
readonly LOCK_FILE=/run/lock/monday-bybit-options-shadow-gate.lock
readonly SERVICE_USER=hftcollector
readonly SERVICE_HOME=/var/lib/hft-collector
readonly SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
readonly SHADOW_SPOOL=/data/monday/spool/bybit-options-shadow
readonly SHADOW_OSS_BUCKET=${BYBIT_OPTIONS_SHADOW_OSS_BUCKET:-monday-lob-apne1-1045353359}
readonly SHADOW_OSS_ENDPOINT=${BYBIT_OPTIONS_SHADOW_OSS_ENDPOINT:-oss-ap-northeast-1-internal.aliyuncs.com}
readonly SHADOW_OSS_REGION=${BYBIT_OPTIONS_SHADOW_OSS_REGION:-ap-northeast-1}
readonly SHADOW_ALIYUN_PROFILE=${BYBIT_OPTIONS_SHADOW_ALIYUN_PROFILE:-ecs-role}

die() {
  printf 'Bybit Options shadow gate failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage: host-bybit-options-shadow-gate.sh <candidate-sha256>' \
    '' \
    'Production gates always observe at least 3600 seconds.' \
    'Tests may set MONDAY_GATE_TEST_SECONDS only with' \
    'MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1; test evidence cannot pass cutover.' \
    'Test-only health settling may use MONDAY_TEST_HEALTH_SETTLE_SECONDS only' \
    'with MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1 and a value below 2400 seconds.'
}

resolve_health_settle_seconds() {
  health_settle_seconds=$HEALTH_SETTLE_SECONDS
  if [[ -n ${MONDAY_TEST_HEALTH_SETTLE_SECONDS:-} ]]; then
    [[ $test_only == true ]] \
      || die 'short health settles require a test-only gate'
    [[ ${MONDAY_ALLOW_SHORT_GATE_FOR_TESTS:-0} == 1 ]] \
      || die 'short health settles require MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1'
    [[ ${MONDAY_TEST_HEALTH_SETTLE_SECONDS} =~ ^[1-9][0-9]*$ ]] \
      || die 'test health settle duration must be a positive integer'
    (( MONDAY_TEST_HEALTH_SETTLE_SECONDS < HEALTH_SETTLE_SECONDS )) \
      || die 'test health settle duration must be shorter than the formal settle duration'
    health_settle_seconds=$MONDAY_TEST_HEALTH_SETTLE_SECONDS
  fi
}

[[ ${EUID:-$(id -u)} -eq 0 ]] || die 'must run as root'
[[ $# -eq 1 ]] || { usage >&2; exit 2; }

for command in awk chmod chown cmp date dirname flock grep id install jq mkdir mktemp \
  mountpoint mv readlink rm runuser sed sha256sum sleep sort stat systemctl systemd-run tr wc; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

mountpoint -q /data || die '/data must be a mount point'
[[ -r /proc/uptime ]] || die '/proc/uptime is required for monotonic timing'
id "$SERVICE_USER" >/dev/null 2>&1 || die "missing service user: $SERVICE_USER"
install -d -m 0755 "$(dirname "$LOCK_FILE")"
exec 9>"$LOCK_FILE"
flock -n 9 || die 'another Bybit Options gate or cutover is running'

candidate_sha=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
[[ $candidate_sha =~ ^[a-f0-9]{64}$ ]] || die 'candidate SHA-256 must be 64 hexadecimal characters'
candidate_release="$RELEASE_ROOT/$candidate_sha"
candidate_binary="$candidate_release/bybit-options-archiver"
candidate_deployment="$candidate_release/deployment"
release_json="$candidate_release/release.json"
control_plane_lib="$candidate_deployment/bybit-options-control-plane-lib.sh"
shadow_gate_policy="$candidate_deployment/bybit-options-shadow-gate-policy.jq"
runtime_health_policy="$candidate_deployment/bybit-options-runtime-health-policy.jq"

direct_directory() {
  local path=$1
  [[ -d $path && ! -L $path ]] || return 1
  [[ $(readlink -f -- "$path") == "$path" ]]
}

direct_directory_or_absent() {
  local path=$1
  [[ ! -e $path && ! -L $path ]] || direct_directory "$path"
}

secure_regular_file() {
  local path=$1 mode owner
  [[ -f $path && ! -L $path ]] || die "required regular file is missing or a symlink: $path"
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == 0 ]] || die "required file is not root-owned: $path"
  (( (8#$mode & 022) == 0 )) || die "required file is group/world writable: $path"
}

for path in /opt/monday /opt/monday/releases "$RELEASE_ROOT" "$candidate_release" \
  "$candidate_deployment"; do
  direct_directory "$path" || die "release path is missing, indirect, or a symlink: $path"
done
secure_regular_file "$release_json"
secure_regular_file "$control_plane_lib"
secure_regular_file "$shadow_gate_policy"
secure_regular_file "$runtime_health_policy"
# shellcheck disable=SC1090,SC1091
. "$control_plane_lib"
deployment_bundle_sha256=$(jq -er '.deployment_bundle_sha256' "$release_json")
deployment_source_revision=$(jq -er '.deployment_source_revision' "$release_json")
[[ $deployment_bundle_sha256 =~ ^[a-f0-9]{64}$ ]] \
  || die 'release metadata has an invalid deployment bundle SHA-256'
[[ $deployment_source_revision =~ ^[a-f0-9]{40,64}$ ]] \
  || die 'release metadata has an invalid deployment source revision'
jq -e --arg artifact "$candidate_sha" --arg bundle "$deployment_bundle_sha256" \
  '.artifact_sha256 == $artifact and .deployment_bundle_sha256 == $bundle' \
  "$release_json" >/dev/null || die 'release metadata does not match the candidate identity'

[[ -f $candidate_binary && -x $candidate_binary ]] || die "candidate is not executable: $candidate_binary"
secure_regular_file "$candidate_binary"
printf '%s  %s\n' "$candidate_sha" "$candidate_binary" | sha256sum --check --strict >/dev/null
[[ -L $SHADOW_BINARY ]] || die "$SHADOW_BINARY must be a symlink"
[[ $(readlink -f "$SHADOW_BINARY") == "$candidate_binary" ]] \
  || die 'shadow symlink does not point to the requested candidate'
printf '%s  %s\n' "$candidate_sha" "$SHADOW_BINARY" | sha256sum --check --strict >/dev/null

gate_seconds=${MONDAY_GATE_TEST_SECONDS:-$REQUIRED_DURATION_SECONDS}
[[ $gate_seconds =~ ^[1-9][0-9]*$ ]] || die 'gate duration must be a positive integer'
test_only=false
if ((gate_seconds < REQUIRED_DURATION_SECONDS)); then
  [[ ${MONDAY_ALLOW_SHORT_GATE_FOR_TESTS:-0} == 1 ]] \
    || die 'short gates require MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1'
  test_only=true
fi
resolve_health_settle_seconds

for asset in \
  bybit-options-archiver.service \
  bybit-options-upload.service \
  bybit-options-upload.timer \
  bybit-options-runtime-health-policy.jq \
  bybit-options-shadow-gate-policy.jq \
  bybit-options-control-plane-lib.sh; do
  secure_regular_file "$candidate_deployment/$asset"
done

if [[ -e $SHADOW_SPOOL || -L $SHADOW_SPOOL ]]; then
  direct_directory "$SHADOW_SPOOL" || die 'shadow spool is missing, indirect, or a symlink'
  find "$SHADOW_SPOOL" -mindepth 1 -delete \
    || die 'failed to clear the isolated shadow spool'
else
  install -d -o "$SERVICE_USER" -g "$SERVICE_USER" -m 0750 "$SHADOW_SPOOL"
fi

binary_evidence_dir="$EVIDENCE_ROOT/$candidate_sha"
bundle_evidence_dir="$binary_evidence_dir/$deployment_bundle_sha256"
runs_dir="$bundle_evidence_dir/runs"
gate_run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
evidence_dir="$runs_dir/$gate_run_id"
gate_json="$evidence_dir/gate.json"
passed_marker="$evidence_dir/PASSED.sha256"
gate_tmp="$evidence_dir/.gate.json.tmp"
marker_tmp="$evidence_dir/.PASSED.sha256.tmp"
for path in /data/monday /data/monday/evidence "$EVIDENCE_ROOT"; do
  direct_directory_or_absent "$path" || die "evidence path is indirect or a symlink: $path"
done
install -d -m 0755 /data/monday
install -d -m 0750 /data/monday/evidence "$EVIDENCE_ROOT"
install -d -m 0750 "$binary_evidence_dir"
direct_directory "$binary_evidence_dir" \
  || die 'binary evidence directory is indirect or a symlink'
install -d -m 0750 "$bundle_evidence_dir" "$runs_dir"
direct_directory "$bundle_evidence_dir" \
  || die 'bundle evidence directory is indirect or a symlink'
direct_directory "$runs_dir" || die 'gate runs directory is indirect or a symlink'
if [[ $test_only != true ]]; then
  shopt -s nullglob
  existing_passes=("$runs_dir"/*/PASSED.sha256)
  shopt -u nullglob
  (( ${#existing_passes[@]} == 0 )) \
    || die 'an immutable production-eligible gate already exists for this release identity'
fi
mkdir -m 0750 -- "$evidence_dir" \
  || die 'gate run evidence directory already exists or could not be created atomically'
direct_directory "$evidence_dir" || die 'gate run evidence directory is indirect or a symlink'
run_created_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
jq -n \
  --arg schema monday.bybit_options_shadow_gate_run.v1 \
  --arg run_id "$gate_run_id" \
  --arg created_at "$run_created_at" \
  --arg candidate_sha256 "$candidate_sha" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
  --arg deployment_source_revision "$deployment_source_revision" \
  --argjson requested_duration_seconds "$gate_seconds" \
  --argjson health_settle_seconds "$health_settle_seconds" \
  --argjson test_only "$test_only" \
  '{schema:$schema,run_id:$run_id,created_at:$created_at,
    candidate_sha256:$candidate_sha256,
    deployment_bundle_sha256:$deployment_bundle_sha256,
    deployment_source_revision:$deployment_source_revision,
    requested_duration_seconds:$requested_duration_seconds,
    health_settle_seconds:$health_settle_seconds,test_only:$test_only}' \
  >"$evidence_dir/run.json"
chmod 0640 "$evidence_dir/run.json"

tmp_dir=$(mktemp -d)
gate_finished=false
shadow_unit="bybit-options-shadow"
shadow_unit_full="bybit-options-shadow.service"
cleanup() {
  local status=$?
  rm -rf "$tmp_dir"
  rm -f "$gate_tmp" "$marker_tmp"
  if [[ $gate_finished != true ]]; then
    systemctl stop "$shadow_unit_full" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap 'exit 143' HUP INT TERM
trap cleanup EXIT

monotonic_seconds() {
  awk '{print int($1)}' /proc/uptime
}

shadow_env=(
  "BYBIT_OPTIONS_SPOOL_DIR=$SHADOW_SPOOL"
  BYBIT_OPTIONS_SEGMENT_SECONDS=3600
  BYBIT_OPTIONS_MAX_SEGMENT_BYTES=4294967296
  BYBIT_OPTIONS_LOCAL_ZST_RETENTION_SECONDS=172800
  MIN_FREE_GB=20.0
  BYBIT_OPTIONS_SPOOL_MAX_BYTES=53687091200
  "OSS_BUCKET=$SHADOW_OSS_BUCKET"
  "OSS_ENDPOINT=$SHADOW_OSS_ENDPOINT"
  "OSS_REGION=$SHADOW_OSS_REGION"
  "ALIYUN_PROFILE=$SHADOW_ALIYUN_PROFILE"
  RUST_LOG=info
)
env_args=()
for pair in "${shadow_env[@]}"; do
  env_args+=(--setenv="$pair")
done

assert_candidate() {
  [[ -L $SHADOW_BINARY ]] || die 'shadow candidate symlink disappeared'
  [[ $(readlink -f "$SHADOW_BINARY") == "$candidate_binary" ]] \
    || die 'shadow candidate symlink changed during the gate'
  printf '%s  %s\n' "$candidate_sha" "$SHADOW_BINARY" \
    | sha256sum --check --strict >/dev/null
}

systemctl reset-failed "$shadow_unit_full" >/dev/null 2>&1 || true
systemd-run --quiet --unit="$shadow_unit" \
  --property=User="$SERVICE_USER" \
  --property=WorkingDirectory=/opt/monday \
  --property=MemoryHigh=1G \
  --property=MemoryMax=1536M \
  "${env_args[@]}" \
  -- "$candidate_binary" \
  || die 'failed to start the shadow service'

gate_started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
gate_started_epoch=$(date +%s)
gate_started_ms=$((gate_started_epoch * 1000))

systemctl_is_active() {
  systemctl is-active --quiet "$shadow_unit_full"
}

health_passes() {
  local health="$SHADOW_SPOOL/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  jq -e \
    --argjson minimum_symbols "$MINIMUM_SYMBOLS" \
    --argjson minimum_updated_ms "$gate_started_ms" \
    --argjson old_updated_ms 0 \
    -f "$runtime_health_policy" "$health" >/dev/null
}

settle_deadline=$(( $(monotonic_seconds) + health_settle_seconds ))
while ! health_passes; do
  (( $(monotonic_seconds) < settle_deadline )) \
    || die 'shadow health did not reach the fail-closed gate before the settle deadline'
  systemctl_is_active || die 'shadow service stopped while settling'
  [[ $(systemctl show "$shadow_unit_full" --property=NRestarts --value) == 0 ]] \
    || die 'shadow service restarted while settling'
  sleep 5
done

observed_updated_ms=
last_health_advance_mono=
max_health_silence_seconds=
health_samples=0
health="$SHADOW_SPOOL/health.json"
observed_updated_ms=$(jq -er '.updated_at_ms' "$health")
last_health_advance_mono=$(monotonic_seconds)
max_health_silence_seconds=0

validate_observation_sample() {
  health_passes || die 'shadow health failed during observation'
  systemctl_is_active || die 'shadow service stopped during observation'
  [[ $(systemctl show "$shadow_unit_full" --property=NRestarts --value) == 0 ]] \
    || die 'shadow service restarted during observation'
  local current_updated_ms next_updated_ms next_advance_mono next_max_gap sample_increment
  current_updated_ms=$(jq -er '.updated_at_ms' "$health")
  local current_mono
  current_mono=$(monotonic_seconds)
  if ! read -r next_updated_ms next_advance_mono next_max_gap sample_increment < <(
    bybit_options_observe_health_freshness \
      "$observed_updated_ms" \
      "$last_health_advance_mono" \
      "$max_health_silence_seconds" \
      "$current_updated_ms" "$current_mono" "$MAX_HEALTH_SILENCE_SECONDS"
  ); then
    die 'shadow health timestamp regressed or stopped advancing for more than the allowed silence'
  fi
  observed_updated_ms=$next_updated_ms
  last_health_advance_mono=$next_advance_mono
  max_health_silence_seconds=$next_max_gap
  health_samples=$((health_samples + sample_increment))
}

observation_started_mono=$(monotonic_seconds)
observation_deadline=$((observation_started_mono + gate_seconds))
while (( $(monotonic_seconds) < observation_deadline )); do
  now_mono=$(monotonic_seconds)
  remaining=$((observation_deadline - now_mono))
  interval=30
  ((remaining < interval)) && interval=$remaining
  ((interval > 0)) && sleep "$interval"
  assert_candidate
  validate_observation_sample
done

assert_candidate
systemctl_is_active || die 'shadow service is not active at gate close'
restart_count=$(systemctl show "$shadow_unit_full" --property=NRestarts --value)
[[ $restart_count == 0 ]] || die 'shadow service has a non-zero restart count'

health_sha256=$(sha256sum "$health" | awk '{print $1}')
symbols_expected=$(jq -er '.symbols_expected' "$health")
symbols_seen=$(jq -er '.symbols_seen' "$health")
connected_workers=$(jq -er '.connected_workers' "$health")
events=$(jq -er '.events' "$health")
last_event_at_ms=$(jq -er '.last_event_at_ms' "$health")
updated_at_ms=$(jq -er '.updated_at_ms' "$health")
disk_free_gb=$(jq -er '.disk_free_gb' "$health")
install -m 0640 "$health" "$evidence_dir/health.json"

systemctl stop "$shadow_unit_full" || die 'failed to stop the shadow service'
systemctl_is_active && die 'shadow service remained active after stop'
assert_candidate

# Drain the isolated shadow spool with the candidate uploader.  Any segment
# completed during the gate is uploaded to OSS under the configured shadow
# bucket and the readback-verified source is recycled; the upload-status must
# show zero failures and the spool must be left empty of raw segments.
runuser --user "$SERVICE_USER" -- env -i \
  HOME="$SERVICE_HOME" \
  PATH="$SAFE_PATH" \
  "${shadow_env[@]}" \
  "$candidate_binary" --upload-only \
  || die 'shadow drain (upload-only) failed'
if [[ -f "$SHADOW_SPOOL/upload-status.json" ]]; then
  upload_failure_count=$(jq -er '.failure_count // 0' "$SHADOW_SPOOL/upload-status.json")
  last_upload_success_at=$(jq -er '.last_success_at // 0' "$SHADOW_SPOOL/upload-status.json")
  [[ $upload_failure_count == 0 ]] || die 'shadow drain recorded OSS upload failures'
else
  upload_failure_count=0
  last_upload_success_at=0
fi
remaining_raw=$(find "$SHADOW_SPOOL" -maxdepth 1 -type f \( \
  -name '*.ndjson' -o -name '*.ndjson.active' \) -print -quit)
[[ -z $remaining_raw ]] || die "shadow spool still contains a raw segment: $remaining_raw"

completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
jq -n \
  --arg schema monday.bybit_options_shadow_gate.v1 \
  --arg run_id "$gate_run_id" \
  --arg created_at "$run_created_at" \
  --arg completed_at "$completed_at" \
  --arg candidate_sha256 "$candidate_sha" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
  --arg deployment_source_revision "$deployment_source_revision" \
  --argjson duration_seconds "$gate_seconds" \
  --argjson health_settle_seconds "$health_settle_seconds" \
  --argjson test_only "$test_only" \
  --argjson passed true \
  --argjson production_eligible "$([[ $test_only != true ]] && printf true || printf false)" \
  --argjson health_samples "$health_samples" \
  --argjson max_health_silence_seconds "$max_health_silence_seconds" \
  --arg health_sha256 "$health_sha256" \
  --arg shadow_unit "$shadow_unit_full" \
  --argjson shadow_restart_count "$restart_count" \
  --argjson symbols_expected "$symbols_expected" \
  --argjson symbols_seen "$symbols_seen" \
  --argjson connected_workers "$connected_workers" \
  --argjson events "$events" \
  --argjson last_event_at_ms "$last_event_at_ms" \
  --argjson updated_at_ms "$updated_at_ms" \
  --argjson disk_free_gb "$disk_free_gb" \
  --argjson upload_failure_count "$upload_failure_count" \
  --argjson last_upload_success_at "$last_upload_success_at" \
  '{schema:$schema,run_id:$run_id,created_at:$created_at,completed_at:$completed_at,
    candidate_sha256:$candidate_sha256,
    deployment_bundle_sha256:$deployment_bundle_sha256,
    deployment_source_revision:$deployment_source_revision,
    duration_seconds:$duration_seconds,health_settle_seconds:$health_settle_seconds,
    test_only:$test_only,passed:$passed,production_eligible:$production_eligible,
    health_samples:$health_samples,max_health_silence_seconds:$max_health_silence_seconds,
    health_sha256:$health_sha256,
    service:{unit:$shadow_unit,active:true,restart_count:$shadow_restart_count,
      binary_sha256:$candidate_sha256,spool_dir:$SHADOW_SPOOL},
    health:{schema:"monday.bybit_options_quote.v1",venue:"bybit",category:"option",
      symbols_expected:$symbols_expected,symbols_seen:$symbols_seen,
      connected_workers:$connected_workers,events:$events,
      last_event_at_ms:$last_event_at_ms,disk_free_gb:$disk_free_gb,
      disk_warning:false,spool_warning:false,upload_failure_count:0,
      upload_warning:false,updated_at_ms:$updated_at_ms},
    upload_status:{failure_count:$upload_failure_count,
      last_success_at:$last_upload_success_at},
    spool_drained:true}' \
  >"$gate_tmp"
chmod 0640 "$gate_tmp"
mv -f "$gate_tmp" "$gate_json"

jq -e \
  --arg candidate_sha256 "$candidate_sha" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
  --arg deployment_source_revision "$deployment_source_revision" \
  --argjson minimum_symbols "$MINIMUM_SYMBOLS" \
  --argjson test_only "$test_only" \
  -f "$shadow_gate_policy" "$gate_json" >/dev/null \
  || die 'shadow gate evidence did not satisfy the gate policy'

printf '%s  %s\n' "$candidate_sha" "$candidate_binary" \
  | sha256sum --strict >"$marker_tmp"
chmod 0440 "$marker_tmp"
mv -f "$marker_tmp" "$passed_marker"
gate_finished=true

printf 'Bybit Options shadow gate passed candidate=%s duration=%s samples=%s\n' \
  "$candidate_sha" "$gate_seconds" "$health_samples"
