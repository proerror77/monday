#!/usr/bin/env bash
set -euo pipefail

umask 027
export LC_ALL=C

readonly REQUIRED_DURATION_SECONDS=3600
readonly HEALTH_SETTLE_SECONDS=2400
readonly MAX_HEALTH_SILENCE_SECONDS=120
readonly MAX_SEGMENT_GAP_NS=90000000000
readonly SHADOW_BINARY=/opt/monday/bin/binance-lob-archiver-shadow
readonly RELEASE_ROOT=/opt/monday/releases/binance-lob-archiver
readonly EVIDENCE_ROOT=/data/monday/evidence/shadow-gates
readonly LOCK_FILE=/run/lock/monday-rust-lob-release.lock
readonly SERVICE_USER=hftcollector
readonly SERVICE_HOME=/var/lib/hft-collector
readonly SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

die() {
  printf 'shadow gate failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage: host-rust-lob-shadow-gate.sh <candidate-sha256>' \
    '' \
    'Production gates always observe at least 3600 seconds.' \
    'Tests may set MONDAY_GATE_TEST_SECONDS only with' \
    'MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1; test evidence cannot pass cutover.'
}

[[ ${EUID} -eq 0 ]] || die 'must run as root'
[[ $# -eq 1 ]] || {
  usage >&2
  exit 2
}

for command in aliyun awk chmod chown cmp date dirname find flock grep id install jq mkdir mktemp \
  mountpoint mv readlink rm runuser sed sha256sum sleep sort stat systemctl tr wc zstd; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

mountpoint -q /data || die '/data must be a mount point'
[[ -r /proc/uptime ]] || die '/proc/uptime is required for monotonic timing'
id "$SERVICE_USER" >/dev/null 2>&1 || die "missing service user: $SERVICE_USER"
install -d -m 0755 "$(dirname "$LOCK_FILE")"
exec 9>"$LOCK_FILE"
flock -n 9 || die 'another Rust collector release operation is running'

candidate_sha=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
[[ $candidate_sha =~ ^[a-f0-9]{64}$ ]] || die 'candidate SHA-256 must be 64 hexadecimal characters'
candidate_release="$RELEASE_ROOT/$candidate_sha"
candidate_binary="$candidate_release/binance-lob-archiver"
candidate_deployment="$candidate_release/deployment"
release_json="$candidate_release/release.json"
control_plane_lib="$candidate_deployment/rust-lob-control-plane-lib.sh"

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

env_value() {
  local file=$1
  local key=$2
  local count value
  count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || die "$file must contain exactly one ${key}= entry"
  value=$(sed -n "s/^${key}=//p" "$file")
  [[ -n $value ]] || die "$file has an empty $key"
  printf '%s\n' "$value"
}

readonly -a markets=(spot usdm)
declare -A env_file spool_dir dataset shard_id oss_bucket oss_endpoint oss_region
declare -A aliyun_profile oss_copy_timeout min_symbols unit
for market in "${markets[@]}"; do
  env_file[$market]="/etc/monday/binance-lob-archiver-rust-${market}.env"
  [[ -f ${env_file[$market]} ]] || die "missing ${env_file[$market]}"
  [[ $(env_value "${env_file[$market]}" MARKET) == "$market" ]] \
    || die "${env_file[$market]} has the wrong MARKET"
  [[ $(env_value "${env_file[$market]}" SYMBOLS) == ALL ]] \
    || die "${env_file[$market]} must set SYMBOLS=ALL"
  shard_id[$market]=$(env_value "${env_file[$market]}" SHARD_ID)
  [[ ${shard_id[$market]} == all ]] || die "${env_file[$market]} must set SHARD_ID=all"
  spool_dir[$market]=$(env_value "${env_file[$market]}" SPOOL_DIR)
  dataset[$market]=$(env_value "${env_file[$market]}" DATASET)
  oss_bucket[$market]=$(env_value "${env_file[$market]}" OSS_BUCKET)
  oss_endpoint[$market]=$(env_value "${env_file[$market]}" OSS_ENDPOINT)
  oss_region[$market]=$(env_value "${env_file[$market]}" OSS_REGION)
  aliyun_profile[$market]=$(env_value "${env_file[$market]}" ALIYUN_PROFILE)
  oss_copy_timeout[$market]=$(env_value "${env_file[$market]}" OSS_COPY_TIMEOUT_SECONDS)
  [[ ${oss_copy_timeout[$market]} =~ ^[1-9][0-9]*$ ]] \
    || die "${env_file[$market]} has an invalid OSS_COPY_TIMEOUT_SECONDS"
  [[ ${oss_bucket[$market]} =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]] \
    || die "${env_file[$market]} has an invalid OSS_BUCKET"
  [[ ${oss_region[$market]} == ap-northeast-1 ]] \
    || die "${env_file[$market]} must use the Tokyo OSS region"
  [[ ${oss_endpoint[$market]} == oss-ap-northeast-1-internal.aliyuncs.com ]] \
    || die "${env_file[$market]} must use the Tokyo internal OSS endpoint"
  [[ ${aliyun_profile[$market]} == ecs-role ]] \
    || die "${env_file[$market]} must use the ECS RAM-role profile"
  unit[$market]="binance-lob-archiver-rust@${market}.service"
done

for asset in \
  binance-lob-archiver-rust@.service \
  binance-lob-archiver-rust-upload@.service \
  binance-lob-archiver-rust-spot.env \
  binance-lob-archiver-rust-usdm.env; do
  secure_regular_file "$candidate_deployment/$asset"
  case "$asset" in
    *.service) installed_asset="/etc/systemd/system/$asset" ;;
    *.env) installed_asset="/etc/monday/$asset" ;;
  esac
  secure_regular_file "$installed_asset"
  cmp -s "$candidate_deployment/$asset" "$installed_asset" \
    || die "installed shadow asset differs from the gated deployment bundle: $asset"
done

[[ ${spool_dir[spot]} == /data/monday/spool/binance-lob-rust-shadow/spot ]] \
  || die 'Spot shadow spool path is not isolated'
[[ ${spool_dir[usdm]} == /data/monday/spool/binance-lob-rust-shadow/usdm ]] \
  || die 'USD-M shadow spool path is not isolated'
for path in \
  /data/monday/spool/binance-lob-rust-shadow \
  "${spool_dir[spot]}" \
  "${spool_dir[usdm]}"; do
  [[ -d $path && ! -L $path ]] || die "shadow spool is missing or a symlink: $path"
  [[ $(readlink -f "$path") == "$path" ]] \
    || die "shadow spool resolved outside its canonical path: $path"
done
[[ ${dataset[spot]} == spot_all_rust_shadow ]] || die 'Spot shadow dataset is not isolated'
[[ ${dataset[usdm]} == usdm_perpetual_all_rust_shadow ]] \
  || die 'USD-M shadow dataset is not isolated'
min_symbols[spot]=1000
min_symbols[usdm]=400

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
for path in /data/monday /data/monday/evidence "$EVIDENCE_ROOT"; do
  direct_directory "$path" || die "evidence path is indirect or a symlink: $path"
done
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
  --arg schema monday.rust_lob_shadow_gate_run.v1 \
  --arg run_id "$gate_run_id" \
  --arg created_at "$run_created_at" \
  --arg candidate_sha256 "$candidate_sha" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
  --arg deployment_source_revision "$deployment_source_revision" \
  --argjson requested_duration_seconds "$gate_seconds" \
  --argjson test_only "$test_only" \
  '{schema:$schema,run_id:$run_id,created_at:$created_at,
    candidate_sha256:$candidate_sha256,
    deployment_bundle_sha256:$deployment_bundle_sha256,
    deployment_source_revision:$deployment_source_revision,
    requested_duration_seconds:$requested_duration_seconds,test_only:$test_only}' \
  >"$evidence_dir/run.json"
chmod 0640 "$evidence_dir/run.json"

tmp_dir=$(mktemp -d)
chown "$SERVICE_USER:$SERVICE_USER" "$tmp_dir"
chmod 0750 "$tmp_dir"
gate_finished=false
cleanup() {
  local status=$?
  rm -rf "$tmp_dir"
  rm -f "$gate_tmp" "$marker_tmp"
  if [[ $gate_finished != true ]]; then
    systemctl stop "${unit[spot]}" "${unit[usdm]}" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup EXIT

assert_candidate() {
  [[ -L $SHADOW_BINARY ]] || die 'shadow candidate symlink disappeared'
  [[ $(readlink -f "$SHADOW_BINARY") == "$candidate_binary" ]] \
    || die 'shadow candidate symlink changed during the gate'
  printf '%s  %s\n' "$candidate_sha" "$SHADOW_BINARY" \
    | sha256sum --check --strict >/dev/null
}

assert_spool_drained() {
  local market=$1
  local remaining
  remaining=$(find "${spool_dir[$market]}" \( -type f -o -type l \) \( \
    -name '*.manifest.json' -o -name '*.jsonl.part' -o \
    -name '*.zst.tmp' -o -name '*.part.corrupt' -o \
    -name '*.jsonl.zst' -o -name '*._SUCCESS' -o \
    -name '*.uploaded-cleanup.json' -o -name '*.uploaded-cleanup.json.tmp' \
    \) -print -quit)
  [[ -z $remaining ]] || die "$market shadow spool still contains segment artifact: $remaining"
}

run_candidate_drain() {
  local market=$1
  runuser --user "$SERVICE_USER" -- env -i \
    HOME="$SERVICE_HOME" \
    PATH="$SAFE_PATH" \
    RUST_LOG=info \
    SPOOL_DIR="${spool_dir[$market]}" \
    OSS_BUCKET="${oss_bucket[$market]}" \
    OSS_ENDPOINT="${oss_endpoint[$market]}" \
    OSS_REGION="${oss_region[$market]}" \
    ALIYUN_PROFILE="${aliyun_profile[$market]}" \
    OSS_COPY_TIMEOUT_SECONDS="${oss_copy_timeout[$market]}" \
    "$candidate_binary" --upload-only
  assert_spool_drained "$market"
}

systemctl stop "${unit[spot]}" "${unit[usdm]}"
for market in "${markets[@]}"; do
  run_candidate_drain "$market"
  rm -f "${spool_dir[$market]}/health.json"
done
candidate_units=(
  "${unit[spot]}"
  "${unit[usdm]}"
  binance-lob-archiver-rust-upload@spot.service
  binance-lob-archiver-rust-upload@usdm.service
)
for candidate_unit in "${candidate_units[@]}"; do
  # A never-started template instance is legitimately not loaded. Reset each
  # unit independently so that condition cannot skip the remaining cleanup;
  # the identity assertion and start below remain fail-closed.
  systemctl reset-failed "$candidate_unit" >/dev/null 2>&1 || true
done
assert_candidate

gate_started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
gate_started_epoch=$(date +%s)
gate_started_ns="${gate_started_epoch}000000000"
systemctl start "${unit[spot]}" "${unit[usdm]}"

systemctl_value() {
  local market=$1
  local property=$2
  systemctl show "${unit[$market]}" --property="$property" --value
}

require_uint() {
  local value=$1
  local label=$2
  [[ $value =~ ^[0-9]+$ ]] || die "$label is unavailable: $value"
  printf '%s\n' "$value"
}

timespan_to_us() {
  local value=$1
  case "$value" in
    *us)
      value=${value%us}
      [[ $value =~ ^[0-9]+$ ]] || return 1
      printf '%s\n' "$value"
      ;;
    *ms)
      value=${value%ms}
      [[ $value =~ ^[0-9]+$ ]] || return 1
      printf '%s\n' "$((value * 1000))"
      ;;
    *min)
      value=${value%min}
      [[ $value =~ ^[0-9]+$ ]] || return 1
      printf '%s\n' "$((value * 60 * 1000000))"
      ;;
    *s)
      value=${value%s}
      [[ $value =~ ^[0-9]+$ ]] || return 1
      printf '%s\n' "$((value * 1000000))"
      ;;
    [0-9]*)
      [[ $value =~ ^[0-9]+$ ]] || return 1
      printf '%s\n' "$value"
      ;;
    *) return 1 ;;
  esac
}

monotonic_seconds() {
  awk '{print int($1)}' /proc/uptime
}

declare -A active_enter_us cpu_start_ns cpu_quota_us memory_max_bytes max_memory_bytes
for market in "${markets[@]}"; do
  systemctl is-active --quiet "${unit[$market]}" || die "$market shadow service is not active"
  [[ $(systemctl_value "$market" ActiveState) == active ]] \
    || die "$market shadow service did not enter ActiveState=active"
  [[ $(systemctl_value "$market" SubState) == running ]] \
    || die "$market shadow service did not enter SubState=running"
  [[ $(systemctl_value "$market" NRestarts) == 0 ]] \
    || die "$market shadow service restarted during startup"
  active_enter_us[$market]=$(require_uint \
    "$(systemctl_value "$market" ActiveEnterTimestampMonotonic)" \
    "$market ActiveEnterTimestampMonotonic")
  cpu_start_ns[$market]=$(require_uint "$(systemctl_value "$market" CPUUsageNSec)" \
    "$market CPUUsageNSec")
  quota_raw=$(systemctl_value "$market" CPUQuotaPerSecUSec)
  cpu_quota_us[$market]=$(timespan_to_us "$quota_raw") \
    || die "$market CPU quota is unavailable: $quota_raw"
  ((cpu_quota_us[$market] > 0)) || die "$market CPU quota must be finite"
  memory_max_bytes[$market]=$(require_uint "$(systemctl_value "$market" MemoryMax)" \
    "$market MemoryMax")
  ((memory_max_bytes[$market] > 0)) || die "$market MemoryMax must be finite"
  max_memory_bytes[$market]=$(require_uint "$(systemctl_value "$market" MemoryCurrent)" \
    "$market MemoryCurrent")
done

health_passes() {
  local market=$1
  local health="${spool_dir[$market]}/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  jq -e \
    --arg market "$market" \
    --arg dataset "${dataset[$market]}" \
    --argjson minimum_symbols "${min_symbols[$market]}" \
    --argjson gate_started_ns "$gate_started_ns" \
    '.market == $market
      and .dataset == $dataset
      and .updated_at_ns >= $gate_started_ns
      and .status == "synced"
      and .sequence_gaps == 0
      and (.symbol_count | type) == "number"
      and .symbol_count == (.symbol_count | floor)
      and .symbol_count >= $minimum_symbols
      and (.snapshot_ready_count | type) == "number"
      and .snapshot_ready_count == (.snapshot_ready_count | floor)
      and .snapshot_ready_count == .symbol_count
      and .bridged_count == .symbol_count
      and .stream_coverage_verified_count == .symbol_count
      and .snapshot_only_symbols == []
      and .all_symbols_bridged == true
      and .all_stream_coverage_verified == true
      and .queue_saturated == false
      and .disk_warning == false
      and .upload_warning == false
      and (.upload_failure_count | type) == "number"
      and .upload_failure_count >= 0
      and .upload_failure_count == (.upload_failure_count | floor)
      and (.session_id | type) == "string"
      and (.session_id | length) > 0' \
    "$health" >/dev/null
}

health_catalog_sha256() {
  local market=$1
  jq -c '.symbols | keys | sort' "${spool_dir[$market]}/health.json" \
    | sha256sum | awk '{print $1}'
}

settle_deadline=$(( $(monotonic_seconds) + HEALTH_SETTLE_SECONDS ))
while ! health_passes spot || ! health_passes usdm; do
  (( $(monotonic_seconds) < settle_deadline )) \
    || die 'shadow health did not reach the fail-closed gate before the settle deadline'
  for market in "${markets[@]}"; do
    systemctl is-active --quiet "${unit[$market]}" || die "$market shadow service stopped while settling"
    [[ $(systemctl_value "$market" NRestarts) == 0 ]] \
      || die "$market shadow service restarted while settling"
  done
  sleep 10
done

declare -A observed_session frozen_symbol_count frozen_catalog_sha256
declare -A initial_upload_failure_count last_health_updated_ns health_samples
declare -A last_health_advance_mono max_health_silence_seconds
for market in "${markets[@]}"; do
  health="${spool_dir[$market]}/health.json"
  observed_session[$market]=$(jq -er '.session_id' "$health")
  frozen_symbol_count[$market]=$(jq -er '.symbol_count' "$health")
  frozen_catalog_sha256[$market]=$(health_catalog_sha256 "$market")
  initial_upload_failure_count[$market]=$(jq -er '.upload_failure_count' "$health")
  last_health_updated_ns[$market]=$(jq -er '.updated_at_ns' "$health")
  last_health_advance_mono[$market]=$(monotonic_seconds)
  max_health_silence_seconds[$market]=0
  health_samples[$market]=1
done

validate_observation_sample() {
  local market=$1 health session symbols catalog upload_failures updated_ns
  local current_mono next_updated_ns next_advance_mono next_max_gap sample_increment
  health="${spool_dir[$market]}/health.json"
  health_passes "$market" || die "$market health failed during observation"
  session=$(jq -er '.session_id' "$health")
  [[ $session == "${observed_session[$market]}" ]] \
    || die "$market collector session changed during observation"
  symbols=$(jq -er '.symbol_count' "$health")
  [[ $symbols == "${frozen_symbol_count[$market]}" ]] \
    || die "$market full catalog changed during observation"
  catalog=$(health_catalog_sha256 "$market")
  [[ $catalog == "${frozen_catalog_sha256[$market]}" ]] \
    || die "$market catalog membership changed during observation"
  upload_failures=$(jq -er '.upload_failure_count' "$health")
  [[ $upload_failures == "${initial_upload_failure_count[$market]}" ]] \
    || die "$market recorded an OSS upload failure during observation"
  updated_ns=$(jq -er '.updated_at_ns' "$health")
  current_mono=$(monotonic_seconds)
  if ! read -r next_updated_ns next_advance_mono next_max_gap sample_increment < <(
    monday_observe_health_freshness \
      "${last_health_updated_ns[$market]}" \
      "${last_health_advance_mono[$market]}" \
      "${max_health_silence_seconds[$market]}" \
      "$updated_ns" "$current_mono" "$MAX_HEALTH_SILENCE_SECONDS"
  ); then
    die "$market health timestamp regressed or stopped advancing for more than ${MAX_HEALTH_SILENCE_SECONDS}s"
  fi
  last_health_updated_ns[$market]=$next_updated_ns
  last_health_advance_mono[$market]=$next_advance_mono
  max_health_silence_seconds[$market]=$next_max_gap
  health_samples[$market]=$((health_samples[$market] + sample_increment))
}

observation_started_ns=$(date +%s%N)
observation_started_mono=$(monotonic_seconds)
observation_deadline=$((observation_started_mono + gate_seconds))
while (( $(monotonic_seconds) < observation_deadline )); do
  now_mono=$(monotonic_seconds)
  remaining=$((observation_deadline - now_mono))
  interval=30
  ((remaining < interval)) && interval=$remaining
  ((interval > 0)) && sleep "$interval"
  assert_candidate
  for market in "${markets[@]}"; do
    systemctl is-active --quiet "${unit[$market]}" || die "$market shadow service stopped during observation"
    [[ $(systemctl_value "$market" NRestarts) == 0 ]] \
      || die "$market shadow service restarted during observation"
    memory_now=$(require_uint "$(systemctl_value "$market" MemoryCurrent)" \
      "$market MemoryCurrent")
    ((memory_now > max_memory_bytes[$market])) && max_memory_bytes[$market]=$memory_now
    ((memory_now <= memory_max_bytes[$market])) \
      || die "$market memory usage exceeded MemoryMax"
    validate_observation_sample "$market"
  done
done

if [[ $test_only != true ]]; then
  minimum_health_samples=$((REQUIRED_DURATION_SECONDS / 90))
  for market in "${markets[@]}"; do
    ((health_samples[$market] >= minimum_health_samples)) \
      || die "$market health did not advance often enough during observation"
  done
fi

declare -A observed_runtime_seconds cpu_usage_ns memory_peak_bytes health_sha256
declare -A symbol_count snapshot_ready_count stream_coverage_verified_count sequence_gaps
now_monotonic_us=$(awk '{printf "%.0f\n", $1 * 1000000}' /proc/uptime)
for market in "${markets[@]}"; do
  assert_candidate
  systemctl is-active --quiet "${unit[$market]}" || die "$market shadow service is not active at gate close"
  [[ $(systemctl_value "$market" NRestarts) == 0 ]] \
    || die "$market shadow service has a non-zero restart count"
  observed_runtime_seconds[$market]=$(( \
    (now_monotonic_us - active_enter_us[$market]) / 1000000 \
  ))
  ((observed_runtime_seconds[$market] >= gate_seconds)) \
    || die "$market actual runtime was shorter than the requested gate"

  cpu_end_ns=$(require_uint "$(systemctl_value "$market" CPUUsageNSec)" "$market CPUUsageNSec")
  ((cpu_end_ns >= cpu_start_ns[$market])) || die "$market CPU accounting moved backwards"
  cpu_usage_ns[$market]=$((cpu_end_ns - cpu_start_ns[$market]))
  allowed_cpu_ns=$(( \
    observed_runtime_seconds[$market] * cpu_quota_us[$market] * 1000 \
    + cpu_quota_us[$market] * 1000 \
  ))
  ((cpu_usage_ns[$market] <= allowed_cpu_ns)) || die "$market average CPU exceeded CPUQuota"

  memory_now=$(require_uint "$(systemctl_value "$market" MemoryCurrent)" "$market MemoryCurrent")
  ((memory_now > max_memory_bytes[$market])) && max_memory_bytes[$market]=$memory_now
  peak_raw=$(systemctl_value "$market" MemoryPeak)
  if [[ $peak_raw =~ ^[0-9]+$ ]] && ((peak_raw > max_memory_bytes[$market])); then
    max_memory_bytes[$market]=$peak_raw
  fi
  memory_peak_bytes[$market]=${max_memory_bytes[$market]}
  ((memory_peak_bytes[$market] <= memory_max_bytes[$market])) \
    || die "$market memory peak exceeded MemoryMax"

  validate_observation_sample "$market"
  health="${spool_dir[$market]}/health.json"
  health_copy="$evidence_dir/${market}-health.json"
  install -m 0640 "$health" "$health_copy"
  health_sha256[$market]=$(sha256sum "$health_copy" | awk '{print $1}')
  symbol_count[$market]=$(jq -er '.symbol_count' "$health")
  snapshot_ready_count[$market]=$(jq -er '.snapshot_ready_count' "$health")
  stream_coverage_verified_count[$market]=$(jq -er '.stream_coverage_verified_count' "$health")
  sequence_gaps[$market]=$(jq -er '.sequence_gaps' "$health")
done

systemctl stop "${unit[spot]}" "${unit[usdm]}"
for market in "${markets[@]}"; do
  systemctl is-active --quiet "${unit[$market]}" \
    && die "$market shadow service remained active after stop"
  assert_candidate
  run_candidate_drain "$market"
done

run_oss() {
  local market=$1
  shift
  runuser --user "$SERVICE_USER" -- env -i \
    HOME="$SERVICE_HOME" \
    PATH="$SAFE_PATH" \
    aliyun ossutil "$@" \
    --profile "${aliyun_profile[$market]}" \
    --endpoint "${oss_endpoint[$market]}" \
    --region "${oss_region[$market]}"
}

manifest_uris() {
  local market=$1
  local listing=$2
  local prefix line token max_age_seconds
  prefix="oss://${oss_bucket[$market]}/lake/raw/venue=binance/market=${market}/dataset=${dataset[$market]}/shard=${shard_id[$market]}/"
  max_age_seconds=$((gate_seconds + HEALTH_SETTLE_SECONDS + 3600))
  run_oss "$market" ls "$prefix" --recursive --short-format \
    --max-age "${max_age_seconds}s" >"$listing"
  while IFS= read -r line; do
    line=${line%$'\r'}
    if [[ $line =~ (oss://[^[:space:]]+\.manifest\.json) ]]; then
      printf '%s\n' "${BASH_REMATCH[1]}"
      continue
    fi
    token=${line##*[$' \t']}
    token=${token#/}
    if [[ $token == *.manifest.json && $token == lake/* ]]; then
      printf 'oss://%s/%s\n' "${oss_bucket[$market]}" "$token"
    fi
  done <"$listing" | sort -u
}

verify_oss_round_trips() {
  local market=$1
  local listing="$tmp_dir/${market}-oss-list.txt"
  local uris="$tmp_dir/${market}-manifest-uris.txt"
  local candidates="$tmp_dir/${market}-manifest-candidates.tsv"
  local index=0
  local uri manifest start_ns end_ns file digest zst_uri zst_path success_uri success_path
  local segment_dir manifest_path manifest_digest actual_manifest_digest
  local actual_digest bytes agg_trade_count manifest_agg_trade_count gap_ns
  local previous_end_ns=0
  local round_trips='[]'
  local -a strict_verifier_args=(--require-lob-continuity)

  manifest_uris "$market" "$listing" >"$uris"
  : >"$candidates"
  while IFS= read -r uri; do
    [[ -n $uri ]] || continue
    manifest="$tmp_dir/${market}-scan-$index.json"
    index=$((index + 1))
    run_oss "$market" cp "$uri" "$manifest" --force --no-progress >/dev/null
    if ! jq -e \
      --arg market "$market" \
      --arg dataset "${dataset[$market]}" \
      --arg shard "${shard_id[$market]}" \
      '.market == $market
        and .dataset == $dataset
        and .shard_id == $shard
        and (.start_received_at_ns | type) == "number"
        and .start_received_at_ns == (.start_received_at_ns | floor)
        and (.end_received_at_ns | type) == "number"
        and .end_received_at_ns == (.end_received_at_ns | floor)
        and .end_received_at_ns >= .start_received_at_ns
        and (.file | type == "string" and test("^[A-Za-z0-9._-]+\\.jsonl\\.zst$"))
        and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))' \
      "$manifest" >/dev/null; then
      die "$market has a malformed manifest during gate discovery: $uri"
    fi
    start_ns=$(jq -er '.start_received_at_ns' "$manifest")
    end_ns=$(jq -er '.end_received_at_ns' "$manifest")
    ((start_ns < observation_started_ns)) && continue
    jq -e --arg session_id "${observed_session[$market]}" \
      '.schema == "binance.market_tape.v1"
        and .trade_summary_contract == "binance.aggregate_trade_summary.v1"
        and (.trade_summaries | type) == "object"
        and (.trade_summaries | length) > 0
        and .lob_continuity.contract == "binance.lob_continuity.v1"
        and .lob_continuity.capture_session_id == $session_id
        and (.lob_continuity.reconnect_boundary | type) == "boolean"
        and .lob_continuity.sequence_gaps == 0
        and .lob_continuity.source_time_rollbacks == 0
        and .lob_continuity.declared_symbol_count == (.symbols | length)
        and .lob_continuity.covered_symbol_count == (.symbols | length)
        and .lob_continuity.missing_symbols == []
        and .stream_coverage_verified_count == (.symbols | length)
        and .all_stream_coverage_verified == true
        and (.lob_continuity.symbols | type) == "object"
        and (.lob_continuity.symbols | length) == (.symbols | length)
        and all(.lob_continuity.symbols[];
          .snapshot_seed_count > 0
          and .checkpoint_count > 0
          and .stream_coverage_verified == true
          and ((.diff_count > 0
              and (.first_update_id | type) == "number"
              and (.last_update_id | type) == "number"
              and .last_update_id >= .first_update_id
              and (.first_source_time_ms | type) == "number"
              and (.last_source_time_ms | type) == "number"
              and .last_source_time_ms >= .first_source_time_ms
              and (.min_source_latency_ms | type) == "number"
              and (.max_source_latency_ms | type) == "number"
              and .min_source_latency_ms >= -1000
              and .max_source_latency_ms <= 30000
              and .max_source_latency_ms >= .min_source_latency_ms)
            or (.diff_count == 0
              and .first_update_id == null
              and .last_update_id == null
              and .first_source_time_ms == null
              and .last_source_time_ms == null
              and .min_source_latency_ms == null
              and .max_source_latency_ms == null))
          and (.first_received_at_ns | type) == "number"
          and (.last_received_at_ns | type) == "number"
          and .last_received_at_ns >= .first_received_at_ns
          and (.min_bid_levels | type) == "number"
          and (.max_bid_levels | type) == "number"
          and .min_bid_levels > 0
          and .max_bid_levels >= .min_bid_levels
          and (.min_ask_levels | type) == "number"
          and (.max_ask_levels | type) == "number"
          and .min_ask_levels > 0
          and .max_ask_levels >= .min_ask_levels)
        and (.event_types.agg_trade | type) == "number"
        and .event_types.agg_trade == (.event_types.agg_trade | floor)
        and .event_types.agg_trade > 0' \
      "$manifest" >/dev/null \
      || die "$market has an incomplete market-tape manifest after gate start: $uri"
    file=$(jq -er '.file' "$manifest")
    digest=$(jq -er '.sha256' "$manifest")
    manifest_digest=$(sha256sum "$manifest" | awk '{print $1}')
    manifest_agg_trade_count=$(jq -er '.event_types.agg_trade' "$manifest")
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$start_ns" "$end_ns" "$uri" "$file" "$digest" "$manifest_digest" "$manifest" \
      "$manifest_agg_trade_count" \
      >>"$candidates"
  done <"$uris"

  candidate_count=$(wc -l <"$candidates" | tr -d ' ')
  ((candidate_count >= 2)) \
    || die "$market has fewer than two OSS manifests created after gate start"

  index=0
  while IFS=$'\t' read -r start_ns end_ns uri file digest manifest_digest manifest \
    manifest_agg_trade_count; do
    index=$((index + 1))
    if ((previous_end_ns > 0 && start_ns < previous_end_ns)); then
      die "$market gate segments overlap at $uri"
    fi
    gap_ns=0
    if ((previous_end_ns > 0)); then
      gap_ns=$((start_ns - previous_end_ns))
      ((gap_ns <= MAX_SEGMENT_GAP_NS)) \
        || die "$market gate segments exceed the continuity gap bound at $uri"
    fi
    previous_end_ns=$end_ns
    segment_dir="$tmp_dir/${market}-segment-${index}"
    install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$segment_dir"
    zst_uri="${uri%/*}/$file"
    zst_path="$segment_dir/$file"
    manifest_path="$segment_dir/${file}.manifest.json"
    run_oss "$market" cp "$uri" "$manifest_path" --force --no-progress >/dev/null
    actual_manifest_digest=$(sha256sum "$manifest_path" | awk '{print $1}')
    [[ $actual_manifest_digest == "$manifest_digest" ]] \
      || die "$market manifest changed between discovery and readback: $uri"
    run_oss "$market" cp "$zst_uri" "$zst_path" --force --no-progress >/dev/null
    actual_digest=$(sha256sum "$zst_path" | awk '{print $1}')
    [[ $actual_digest == "$digest" ]] || die "$market OSS round-trip digest mismatch: $zst_uri"
    success_uri="${uri%/*}/${file}._SUCCESS"
    success_path="$segment_dir/${file}._SUCCESS"
    run_oss "$market" cp "$success_uri" "$success_path" --force --no-progress >/dev/null
    printf '%s\n' "$digest" | cmp -s - "$success_path" \
      || die "$market OSS success marker does not match segment SHA-256: $success_uri"
    agg_trade_count=$(zstd -q -d -c "$zst_path" | jq -er -n '
      def valid_agg_trade:
        .schema == "binance.market_tape.v1"
        and (.received_at_ns | type) == "number"
        and .received_at_ns >= 0
        and (.frame.data.e == "aggTrade")
        and (.frame.data.s | type) == "string"
        and (.frame.data.s | length) > 0
        and (.frame.data.a | type) == "number"
        and .frame.data.a >= 0
        and (.frame.data.a | floor) == .frame.data.a
        and (.frame.data.p | type) == "string"
        and (.frame.data.p | length) > 0
        and (.frame.data.q | type) == "string"
        and (.frame.data.q | length) > 0
        and (.frame.data.E | type) == "number"
        and (.frame.data.T | type) == "number"
        and (.frame.data.m | type) == "boolean";
      reduce inputs as $row
        ({count:0,invalid:false};
          if $row.type == "agg_trade" then
            .count += 1 | .invalid = (.invalid or (($row | valid_agg_trade) | not))
          else . end)
      | if .count == 0 or .invalid then error("missing or malformed agg_trade") else .count end') \
      || die "$market segment has no valid aggregate trades: $zst_uri"
    [[ $agg_trade_count == "$manifest_agg_trade_count" ]] \
      || die "$market manifest aggregate-trade count does not match segment: $uri"
    strict_verifier_args+=(
      --verify-segment "$zst_path"
      --segment-content-sha256 "$digest"
      --segment-manifest-sha256 "$manifest_digest"
    )
    bytes=$(stat -c '%s' "$zst_path")
    install -m 0640 "$manifest" "$evidence_dir/${market}-manifest-${index}.json"
    round_trip=$(jq -cn \
      --arg manifest_uri "$uri" \
      --arg data_uri "$zst_uri" \
      --arg success_uri "$success_uri" \
      --arg sha256 "$digest" \
      --arg manifest_sha256 "$manifest_digest" \
      --argjson start_received_at_ns "$start_ns" \
      --argjson end_received_at_ns "$end_ns" \
      --argjson gap_from_previous_ns "$gap_ns" \
      --argjson bytes "$bytes" \
      --argjson agg_trade_count "$agg_trade_count" \
      --slurpfile manifest "$manifest_path" \
      '($manifest[0].lob_continuity) as $lob_continuity
      | {manifest_uri:$manifest_uri,data_uri:$data_uri,success_uri:$success_uri,sha256:$sha256,
        manifest_sha256:$manifest_sha256,gap_from_previous_ns:$gap_from_previous_ns,
        start_received_at_ns:$start_received_at_ns,end_received_at_ns:$end_received_at_ns,bytes:$bytes,
        agg_trade_count:$agg_trade_count,
        lob_capture_session_id:$lob_continuity.capture_session_id,
        lob_reconnect_boundary:$lob_continuity.reconnect_boundary,
        lob_sequence_gaps:$lob_continuity.sequence_gaps,
        lob_source_time_rollbacks:$lob_continuity.source_time_rollbacks,
        lob_declared_symbol_count:$lob_continuity.declared_symbol_count,
        lob_covered_symbol_count:$lob_continuity.covered_symbol_count,
        stream_coverage_verified_count:$manifest[0].stream_coverage_verified_count,
        all_stream_coverage_verified:$manifest[0].all_stream_coverage_verified,
        lob_min_source_latency_ms:([$lob_continuity.symbols[].min_source_latency_ms | select(type == "number")] | min),
        lob_max_source_latency_ms:([$lob_continuity.symbols[].max_source_latency_ms | select(type == "number")] | max),
        lob_min_bid_levels:([$lob_continuity.symbols[].min_bid_levels] | min),
        lob_min_ask_levels:([$lob_continuity.symbols[].min_ask_levels] | min)}')
    round_trips=$(jq -cn --argjson values "$round_trips" --argjson value "$round_trip" \
      '$values + [$value]')
  done < <(sort -n -k1,1 "$candidates")

  "$candidate_binary" "${strict_verifier_args[@]}" >/dev/null \
    || die "$market strict aggregate-trade and LOB continuity readback failed"

  jq -e --arg session_id "${observed_session[$market]}" '
    all(.[].lob_reconnect_boundary; . == false)
    and all(.[].lob_capture_session_id; . == $session_id)' \
    <<<"$round_trips" >/dev/null \
    || die "$market LOB evidence crosses a capture-session or observation boundary"

  printf '%s\n' "$round_trips"
}

duration_seconds=${observed_runtime_seconds[spot]}
if ((observed_runtime_seconds[usdm] < duration_seconds)); then
  duration_seconds=${observed_runtime_seconds[usdm]}
fi

markets_json='{}'
for market in "${markets[@]}"; do
  round_trips=$(verify_oss_round_trips "$market")
  market_json=$(jq -cn \
    --arg market "$market" \
    --arg unit "${unit[$market]}" \
    --arg dataset "${dataset[$market]}" \
    --arg session_id "${observed_session[$market]}" \
    --arg catalog_sha256 "${frozen_catalog_sha256[$market]}" \
    --arg health_sha256 "${health_sha256[$market]}" \
    --argjson symbol_count "${symbol_count[$market]}" \
    --argjson snapshot_ready_count "${snapshot_ready_count[$market]}" \
    --argjson stream_coverage_verified_count "${stream_coverage_verified_count[$market]}" \
    --argjson sequence_gaps "${sequence_gaps[$market]}" \
    --argjson upload_failure_count "${initial_upload_failure_count[$market]}" \
    --argjson health_samples "${health_samples[$market]}" \
    --argjson max_health_silence_seconds "${max_health_silence_seconds[$market]}" \
    --argjson n_restarts 0 \
    --argjson observed_runtime_seconds "${observed_runtime_seconds[$market]}" \
    --argjson cpu_usage_ns "${cpu_usage_ns[$market]}" \
    --argjson cpu_quota_per_sec_us "${cpu_quota_us[$market]}" \
    --argjson memory_peak_bytes "${memory_peak_bytes[$market]}" \
    --argjson memory_max_bytes "${memory_max_bytes[$market]}" \
    --argjson oss_round_trips "$round_trips" \
    '{market:$market,unit:$unit,dataset:$dataset,session_id:$session_id,
      symbols_config:"ALL",catalog_sha256:$catalog_sha256,
      symbol_count:$symbol_count,snapshot_ready_count:$snapshot_ready_count,
      stream_coverage_verified_count:$stream_coverage_verified_count,
      all_stream_coverage_verified:($stream_coverage_verified_count == $symbol_count),
      sequence_gaps:$sequence_gaps,upload_failure_count:$upload_failure_count,
      health_samples:$health_samples,
      max_health_silence_seconds:$max_health_silence_seconds,
      n_restarts:$n_restarts,
      observed_runtime_seconds:$observed_runtime_seconds,
      cpu_usage_ns:$cpu_usage_ns,cpu_quota_per_sec_us:$cpu_quota_per_sec_us,
      memory_peak_bytes:$memory_peak_bytes,memory_max_bytes:$memory_max_bytes,
      health_sha256:$health_sha256,
      strict_trade_summary_readback:true,
      strict_lob_continuity_readback:true,
      lob_reconnect_boundaries:([$oss_round_trips[].lob_reconnect_boundary] | map(select(.)) | length),
      min_lob_source_latency_ms:([$oss_round_trips[].lob_min_source_latency_ms] | min),
      max_lob_source_latency_ms:([$oss_round_trips[].lob_max_source_latency_ms] | max),
      min_lob_bid_levels:([$oss_round_trips[].lob_min_bid_levels] | min),
      min_lob_ask_levels:([$oss_round_trips[].lob_min_ask_levels] | min),
      max_segment_gap_ns:([$oss_round_trips[].gap_from_previous_ns] | max),
      oss_roundtrips:($oss_round_trips | length),
      agg_trade_segments:($oss_round_trips | length),
      agg_trade_count:([$oss_round_trips[].agg_trade_count] | add),
      oss_roundtrip_evidence:$oss_round_trips}')
  markets_json=$(jq -cn \
    --argjson values "$markets_json" \
    --arg market "$market" \
    --argjson value "$market_json" \
    '$values + {($market):$value}')
done

gate_finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
passed=true
production_eligible=true
if [[ $test_only == true ]] || ((duration_seconds < REQUIRED_DURATION_SECONDS)); then
  passed=false
  production_eligible=false
fi

jq -n \
  --arg schema monday.rust_lob_shadow_gate.v3 \
  --arg candidate_sha256 "$candidate_sha" \
  --arg candidate_binary "$candidate_binary" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
  --arg deployment_source_revision "$deployment_source_revision" \
  --arg started_at "$gate_started_at" \
  --arg finished_at "$gate_finished_at" \
  --argjson required_duration_seconds "$REQUIRED_DURATION_SECONDS" \
  --argjson requested_duration_seconds "$gate_seconds" \
  --argjson duration_seconds "$duration_seconds" \
  --argjson test_only "$test_only" \
  --argjson checks_passed true \
  --argjson production_eligible "$production_eligible" \
  --argjson passed "$passed" \
  --argjson markets "$markets_json" \
  '{schema:$schema,candidate_sha256:$candidate_sha256,candidate_binary:$candidate_binary,
    deployment_bundle_sha256:$deployment_bundle_sha256,
    deployment_source_revision:$deployment_source_revision,
    started_at:$started_at,finished_at:$finished_at,
    required_duration_seconds:$required_duration_seconds,
    requested_duration_seconds:$requested_duration_seconds,
    duration_seconds:$duration_seconds,
    test_only:$test_only,checks_passed:$checks_passed,
    production_eligible:$production_eligible,passed:$passed,markets:$markets}' \
  >"$gate_tmp"
[[ ! -e $gate_json && ! -L $gate_json ]] || die 'gate evidence path already exists'
install -m 0640 "$gate_tmp" "$gate_json"
rm -f "$gate_tmp"

if [[ $production_eligible == true ]]; then
  gate_sha=$(sha256sum "$gate_json" | awk '{print $1}')
  printf '%s  gate.json\n' "$gate_sha" >"$marker_tmp"
  chmod 0640 "$marker_tmp"
  [[ ! -e $passed_marker && ! -L $passed_marker ]] \
    || die 'gate pass marker already exists'
  mv "$marker_tmp" "$passed_marker"
  printf 'production shadow gate passed: %s\nmarker: %s\n' "$gate_json" "$passed_marker"
else
  printf 'short test completed; evidence is not eligible for cutover: %s\n' "$gate_json"
fi

gate_finished=true
