#!/usr/bin/env bash
set -euo pipefail

umask 027
export LC_ALL=C

readonly REQUIRED_DURATION_SECONDS=240
readonly HEALTH_SETTLE_SECONDS=240
readonly GATE_SEGMENT_SECONDS=120
readonly MAX_HEALTH_SILENCE_SECONDS=120
readonly MAX_SEGMENT_GAP_NS=90000000000
readonly HOST_MEMORY_RESERVE_BYTES=1073741824
readonly PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES=268435456
readonly STRICT_VERIFIER_MEMORY_MAX_BYTES=1610612736
readonly UPLOAD_DRAIN_MEMORY_MAX_BYTES=536870912
readonly IO_PSI_WINDOW_SECONDS=15
readonly IO_PSI_WINDOW_US=15000000
readonly IO_PSI_FULL_DELTA_LIMIT_US=150000
readonly IO_PSI_CONSECUTIVE_HIT_LIMIT=3
readonly IO_PSI_SOURCE=/proc/pressure/io
readonly ACTIVE_PROCESS_TERM_GRACE_SECONDS=5
readonly ACTIVE_PROCESS_KILL_GRACE_SECONDS=5
readonly SHADOW_BINARY=/opt/monday/bin/binance-lob-archiver-shadow
readonly RELEASE_ROOT=/opt/monday/releases/binance-lob-archiver
readonly CONTROLLER_RELEASE_ROOT=/opt/monday/releases/binance-lob-controller
readonly EVIDENCE_ROOT=/data/monday/evidence/shadow-gates
readonly RUN_SPOOL_ROOT=/data/monday/spool/binance-lob-rust-shadow/runs
readonly OVERRIDE_ROOT=/run/monday
readonly LOCK_FILE=/run/lock/monday-rust-lob-release.lock
readonly SERVICE_USER=hftcollector
readonly SERVICE_HOME=/var/lib/hft-collector
readonly SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
readonly -a RUNTIME_CONTRACT_ASSETS=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-rust@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-rust-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
  binance-lob-archiver-rust-spot.env
  binance-lob-archiver-rust-usdm.env
)

die() {
  printf 'shadow gate failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage: host-rust-lob-shadow-gate.sh [--controller-release-sha256 <controller-sha256>] [--resource-preflight] <candidate-sha256>' \
    '' \
    'Production gates wait up to 240 seconds for health, then observe at least 240 seconds.' \
    'Tests may set MONDAY_GATE_TEST_SECONDS only with' \
    'MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1; test evidence cannot pass cutover.' \
    'Test-only health settling may use MONDAY_TEST_HEALTH_SETTLE_SECONDS only' \
    'with MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1 and a value below 240 seconds;' \
    'otherwise the policy check fails.'
}

run_spool_dir() {
  local candidate=$1 run_id=$2 market=$3
  [[ $candidate =~ ^[a-f0-9]{64}$ && $run_id =~ ^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$ ]]
  [[ $market == spot || $market == usdm ]]
  printf '%s/%s/%s/%s\n' "$RUN_SPOOL_ROOT" "$candidate" "$run_id" "$market"
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
    (( ${#MONDAY_TEST_HEALTH_SETTLE_SECONDS} <= ${#HEALTH_SETTLE_SECONDS} )) \
      || die 'test health settle duration is too large'
    ((MONDAY_TEST_HEALTH_SETTLE_SECONDS < HEALTH_SETTLE_SECONDS)) \
      || die 'test health settle duration must be shorter than the formal settle duration'
    health_settle_seconds=$MONDAY_TEST_HEALTH_SETTLE_SECONDS
  fi
}

[[ ${EUID} -eq 0 ]] || die 'must run as root'
resource_preflight_only=false
controller_release_arg=
case $# in
  1) candidate_arg=$1 ;;
  2)
    [[ $1 == --resource-preflight ]] || { usage >&2; exit 2; }
    resource_preflight_only=true
    candidate_arg=$2
    ;;
  3)
    [[ $1 == --controller-release-sha256 ]] || { usage >&2; exit 2; }
    controller_release_arg=$2
    candidate_arg=$3
    ;;
  4)
    [[ $1 == --controller-release-sha256 && $3 == --resource-preflight ]] \
      || { usage >&2; exit 2; }
    controller_release_arg=$2
    resource_preflight_only=true
    candidate_arg=$4
    ;;
  *) usage >&2; exit 2 ;;
esac

for command in aliyun awk bash chmod chown cmp date dirname find flock grep id install jq mkdir mktemp \
  mountpoint mv readlink rm runuser sed sha256sum sleep sort stat systemctl systemd-run tr wc zstd; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

mountpoint -q /data || die '/data must be a mount point'
[[ -r /proc/uptime ]] || die '/proc/uptime is required for monotonic timing'
[[ -r $IO_PSI_SOURCE ]] || die 'I/O PSI is unavailable'
id "$SERVICE_USER" >/dev/null 2>&1 || die "missing service user: $SERVICE_USER"
controller_release_sha256=
if [[ -n $controller_release_arg ]]; then
  controller_release_sha256=$(printf '%s' "$controller_release_arg" | tr '[:upper:]' '[:lower:]')
  [[ $controller_release_sha256 =~ ^[a-f0-9]{64}$ ]] \
    || die 'controller release SHA-256 must be 64 hexadecimal characters'
fi
pair_mode=false
[[ -n $controller_release_sha256 ]] && pair_mode=true
if [[ $resource_preflight_only != true || $pair_mode == true ]]; then
  install -d -m 0755 "$(dirname "$LOCK_FILE")"
  exec 9>"$LOCK_FILE"
  flock -n 9 || die 'another Rust collector release operation is running'
fi

candidate_sha=$(printf '%s' "$candidate_arg" | tr '[:upper:]' '[:lower:]')
[[ $candidate_sha =~ ^[a-f0-9]{64}$ ]] || die 'candidate SHA-256 must be 64 hexadecimal characters'

controller_release=
controller_deployment=
controller_manifest=
controller_gate_script=
controller_lib=
controller_policy=
controller_deployment_bundle_sha256=
controller_deployment_source_revision=
controller_artifact_sha256=
controller_runtime_contract_sha256=

candidate_release="$RELEASE_ROOT/$candidate_sha"
candidate_binary="$candidate_release/binance-lob-archiver"
candidate_deployment="$candidate_release/deployment"
release_json="$candidate_release/release.json"

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

runtime_contract_sha256_independent() {
  local directory=$1 asset digest
  for asset in "${RUNTIME_CONTRACT_ASSETS[@]}"; do
    [[ -f $directory/$asset && ! -L $directory/$asset ]] || return 1
  done
  {
    for asset in "${RUNTIME_CONTRACT_ASSETS[@]}"; do
      digest=$(command sha256sum "$directory/$asset" | command awk '{print $1}') \
        || return 1
      printf '%s  %s\n' "$digest" "$asset"
    done
  } | command sha256sum | command awk '{print $1}'
}
readonly -f runtime_contract_sha256_independent

validate_controller_manifest() {
  local manifest=$1
  jq -e '
    type == "object"
    and keys == [
      "artifact_sha256",
      "artifact_uri",
      "deployment_bundle_sha256",
      "deployment_bundle_uri",
      "deployment_source_revision",
      "runtime_contract_sha256",
      "schema"
    ]
    and .schema == "monday.rust_lob_controller_release.v1"
    and (.artifact_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.artifact_uri | type == "string" and test("^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$"))
    and (.deployment_bundle_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.deployment_bundle_uri | type == "string" and test("^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$"))
    and (.deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
    and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))' \
    "$manifest" >/dev/null || die 'controller release manifest is invalid'
}

verify_controller_release() {
  local expected_manifest_sha=$1
  local actual_manifest_sha expected_manifest_checksum expected_deployment_checksum asset
  for path in /opt/monday /opt/monday/releases "$CONTROLLER_RELEASE_ROOT" \
    "$controller_release" "$controller_deployment"; do
    direct_directory "$path" || die "controller release path is missing, indirect, or a symlink: $path"
  done
  for path in "$controller_manifest" "$controller_release/release.json.sha256" \
    "$controller_release/deployment.sha256" "$controller_gate_script" \
    "$controller_lib" "$controller_policy"; do
    secure_regular_file "$path"
  done
  [[ -x $controller_gate_script ]] \
    || die 'controller Gate script is not executable'
  actual_manifest_sha=$(sha256sum "$controller_manifest" | awk '{print $1}')
  [[ $actual_manifest_sha == "$expected_manifest_sha" ]] \
    || die 'controller release manifest SHA-256 does not match its digest path'
  expected_manifest_checksum=$(printf '%s  release.json\n' "$actual_manifest_sha")
  printf '%s\n' "$expected_manifest_checksum" \
    | cmp -s - "$controller_release/release.json.sha256" \
    || die 'controller release manifest checksum does not match its digest path'
  validate_controller_manifest "$controller_manifest"
  for asset in "$controller_deployment"/*; do
    secure_regular_file "$asset"
  done
  (cd "$controller_release" && sha256sum --check --strict deployment.sha256 >/dev/null) \
    || die 'controller deployment checksum verification failed'
  expected_deployment_checksum=$(cd "$controller_release" \
    && for asset in deployment/*; do sha256sum "$asset"; done | sort -k2)
  printf '%s\n' "$expected_deployment_checksum" \
    | cmp -s - "$controller_release/deployment.sha256" \
    || die 'controller deployment checksum contents drifted'
  [[ $(readlink -f -- "${BASH_SOURCE[0]}") == "$controller_gate_script" ]] \
    || die 'Gate script was not executed from the controller release digest path'
  controller_artifact_sha256=$(jq -er '.artifact_sha256' "$controller_manifest")
  controller_deployment_bundle_sha256=$(jq -er '.deployment_bundle_sha256' "$controller_manifest")
  controller_deployment_source_revision=$(jq -er '.deployment_source_revision' "$controller_manifest")
  controller_runtime_contract_sha256=$(jq -er '.runtime_contract_sha256' "$controller_manifest")
  [[ $(runtime_contract_sha256_independent "$controller_deployment") \
    == "$controller_runtime_contract_sha256" ]] \
    || die 'controller deployment runtime contract checksum drifted'
}

if [[ $pair_mode == true ]]; then
  controller_release="$CONTROLLER_RELEASE_ROOT/$controller_release_sha256"
  controller_deployment="$controller_release/deployment"
  controller_manifest="$controller_release/release.json"
  controller_gate_script="$controller_deployment/host-rust-lob-shadow-gate.sh"
  controller_lib="$controller_deployment/rust-lob-control-plane-lib.sh"
  controller_policy="$controller_deployment/rust-lob-shadow-gate-policy.jq"
  verify_controller_release "$controller_release_sha256"
else
  printf 'deprecated artifact-routed Gate fallback: pass --controller-release-sha256 for a pair-bound operation\n' >&2
  controller_gate_script="$candidate_deployment/host-rust-lob-shadow-gate.sh"
  controller_lib="$candidate_deployment/rust-lob-control-plane-lib.sh"
  controller_policy="$candidate_deployment/rust-lob-shadow-gate-policy.jq"
fi

for path in /opt/monday /opt/monday/releases "$RELEASE_ROOT" "$candidate_release" \
  "$candidate_deployment"; do
  direct_directory "$path" || die "release path is missing, indirect, or a symlink: $path"
done
secure_regular_file "$release_json"
runtime_contract_sha256=$(jq -er '.runtime_contract_sha256' "$release_json")
[[ $runtime_contract_sha256 =~ ^[a-f0-9]{64}$ ]] \
  || die 'release metadata has an invalid runtime contract SHA-256'
if [[ $pair_mode == true ]]; then
  deployment_bundle_sha256=$(jq -r '.deployment_bundle_sha256 // empty' "$release_json")
  deployment_source_revision=$(jq -r '.deployment_source_revision // empty' "$release_json")
  [[ -z $deployment_bundle_sha256 || $deployment_bundle_sha256 == \
    "$controller_deployment_bundle_sha256" ]] \
    || die 'candidate deployment bundle differs from controller release'
  [[ -z $deployment_source_revision || $deployment_source_revision == \
    "$controller_deployment_source_revision" ]] \
    || die 'candidate deployment source differs from controller release'
  deployment_bundle_sha256=$controller_deployment_bundle_sha256
  deployment_source_revision=$controller_deployment_source_revision
  jq -e --arg artifact "$candidate_sha" \
    --arg runtime_contract "$runtime_contract_sha256" \
    '.artifact_sha256 == $artifact
      and .runtime_contract_sha256 == $runtime_contract' \
    "$release_json" >/dev/null || die 'release metadata does not match the candidate identity'
else
  deployment_bundle_sha256=$(jq -er '.deployment_bundle_sha256' "$release_json")
  deployment_source_revision=$(jq -er '.deployment_source_revision' "$release_json")
  [[ $deployment_bundle_sha256 =~ ^[a-f0-9]{64}$ ]] \
    || die 'release metadata has an invalid deployment bundle SHA-256'
  [[ $deployment_source_revision =~ ^[a-f0-9]{40,64}$ ]] \
    || die 'release metadata has an invalid deployment source revision'
  jq -e --arg artifact "$candidate_sha" --arg bundle "$deployment_bundle_sha256" \
    --arg runtime_contract "$runtime_contract_sha256" \
    '.artifact_sha256 == $artifact and .deployment_bundle_sha256 == $bundle
      and .runtime_contract_sha256 == $runtime_contract' \
    "$release_json" >/dev/null || die 'release metadata does not match the candidate identity'
fi
if [[ $pair_mode == true ]]; then
  [[ $controller_artifact_sha256 == "$candidate_sha" ]] \
    || die 'controller release does not bind the requested candidate artifact'
  [[ $controller_deployment_bundle_sha256 == "$deployment_bundle_sha256" ]] \
    || die 'controller and candidate deployment bundle digests differ'
  [[ $controller_deployment_source_revision == "$deployment_source_revision" ]] \
    || die 'controller and candidate deployment source revisions differ'
  [[ $controller_runtime_contract_sha256 == "$runtime_contract_sha256" ]] \
    || die 'controller and candidate runtime contracts differ'
fi

[[ -f $candidate_binary && -x $candidate_binary ]] || die "candidate is not executable: $candidate_binary"
secure_regular_file "$candidate_binary"
printf '%s  %s\n' "$candidate_sha" "$candidate_binary" \
  | sha256sum --check --strict >/dev/null \
  || die 'candidate binary checksum does not match the requested artifact'
[[ -L $SHADOW_BINARY ]] || die "$SHADOW_BINARY must be a symlink"
[[ $(readlink -f "$SHADOW_BINARY") == "$candidate_binary" ]] \
  || die 'shadow symlink does not point to the requested candidate'
printf '%s  %s\n' "$candidate_sha" "$SHADOW_BINARY" | sha256sum --check --strict >/dev/null \
  || die 'shadow binary checksum does not match the requested artifact'

# shellcheck disable=SC1090,SC1091
. "$controller_lib" >/dev/null
candidate_runtime_contract_from_helper=$(monday_rust_lob_runtime_contract_sha256 \
  "$candidate_deployment") \
  || die 'installed runtime contract helper failed'
candidate_runtime_contract_independent=$(runtime_contract_sha256_independent \
  "$candidate_deployment") \
  || die 'installed runtime contract assets are invalid'
[[ $candidate_runtime_contract_from_helper == "$candidate_runtime_contract_independent" ]] \
  || die 'controller runtime contract helper returned an inconsistent digest'
[[ $candidate_runtime_contract_independent == "$runtime_contract_sha256" ]] \
  || die 'installed runtime contract does not match release metadata'

gate_seconds=${MONDAY_GATE_TEST_SECONDS:-$REQUIRED_DURATION_SECONDS}
[[ $gate_seconds =~ ^[1-9][0-9]*$ ]] || die 'gate duration must be a positive integer'
test_only=false
if ((gate_seconds < REQUIRED_DURATION_SECONDS)); then
  [[ ${MONDAY_ALLOW_SHORT_GATE_FOR_TESTS:-0} == 1 ]] \
    || die 'short gates require MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1'
  test_only=true
fi
resolve_health_settle_seconds

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

is_usdm_top100() {
  local value=$1 unique
  local -a symbols
  [[ $value =~ ^[A-Z0-9]+(,[A-Z0-9]+)*$ ]] || return 1
  IFS=, read -r -a symbols <<<"$value"
  (( ${#symbols[@]} == 100 )) || return 1
  unique=$(printf '%s\n' "${symbols[@]}" | sort -u | wc -l)
  (( unique == 100 ))
}

readonly -a markets=(spot usdm)
declare -A env_file base_spool_dir spool_dir override_file dataset shard_id
declare -A oss_bucket oss_endpoint oss_region aliyun_profile oss_copy_timeout
declare -A configured_symbols min_symbols unit expected_stream_types
for market in "${markets[@]}"; do
  env_file[$market]="/etc/monday/binance-lob-archiver-rust-${market}.env"
  [[ -f ${env_file[$market]} ]] || die "missing ${env_file[$market]}"
  [[ $(env_value "${env_file[$market]}" MARKET) == "$market" ]] \
    || die "${env_file[$market]} has the wrong MARKET"
  configured_symbols[$market]=$(env_value "${env_file[$market]}" SYMBOLS)
  shard_id[$market]=$(env_value "${env_file[$market]}" SHARD_ID)
  [[ ${shard_id[$market]} == all ]] || die "${env_file[$market]} must set SHARD_ID=all"
  base_spool_dir[$market]=$(env_value "${env_file[$market]}" SPOOL_DIR)
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
[[ ${configured_symbols[spot]} == ALL ]] \
  || die "${env_file[spot]} must set SYMBOLS=ALL"
is_usdm_top100 "${configured_symbols[usdm]}" \
  || die "${env_file[usdm]} must set 100 unique explicit symbols"

runtime_assets=("${RUNTIME_CONTRACT_ASSETS[@]}")
for asset in "${runtime_assets[@]}"; do
  secure_regular_file "$candidate_deployment/$asset"
done
shadow_assets=(
  binance-lob-archiver-rust@.service
  binance-lob-archiver-rust-upload@.service
  binance-lob-archiver-rust-spot.env
  binance-lob-archiver-rust-usdm.env
)
for asset in "${shadow_assets[@]}"; do
  case "$asset" in
    *.service) installed_asset="/etc/systemd/system/$asset" ;;
    *.env) installed_asset="/etc/monday/$asset" ;;
  esac
  secure_regular_file "$installed_asset"
  cmp -s "$candidate_deployment/$asset" "$installed_asset" \
    || die "installed runtime asset differs from the gated deployment bundle: $asset"
done
grep -Fxq 'MemoryHigh=384M' \
  "$candidate_deployment/binance-lob-archiver-rust-upload@.service" \
  || die 'shadow upload service MemoryHigh differs from the gated template'
grep -Fxq 'MemoryMax=512M' \
  "$candidate_deployment/binance-lob-archiver-rust-upload@.service" \
  || die 'shadow upload service MemoryMax differs from the gated template'
candidate_production_spot_env="$candidate_deployment/binance-lob-archiver-production-spot.env"
candidate_production_usdm_env="$candidate_deployment/binance-lob-archiver-production-usdm.env"
configured_spot_snapshot_producers=$(env_value "${env_file[spot]}" SNAPSHOT_PRODUCERS)
[[ $configured_spot_snapshot_producers == 16 ]] \
  || die 'Spot shadow SNAPSHOT_PRODUCERS must be 16'
[[ $(env_value "$candidate_production_spot_env" SNAPSHOT_PRODUCERS) \
  == "$configured_spot_snapshot_producers" ]] \
  || die 'Spot shadow and production SNAPSHOT_PRODUCERS differ'
[[ $(env_value "$candidate_production_usdm_env" DATASET) \
  == usdm_perpetual_top100_lob ]] \
  || die 'candidate USD-M production dataset is not the LOB-first identity'
[[ $(env_value "$candidate_production_usdm_env" SYMBOLS) \
  == "${configured_symbols[usdm]}" ]] \
  || die 'USD-M shadow and production symbol lists differ'
configured_usdm_ws_shard_size=$(env_value "${env_file[usdm]}" WS_SHARD_SIZE)
[[ $configured_usdm_ws_shard_size == 25 ]] \
  || die 'USD-M shadow WS_SHARD_SIZE must be 25'
[[ $(env_value "$candidate_production_usdm_env" WS_SHARD_SIZE) \
  == "$configured_usdm_ws_shard_size" ]] \
  || die 'USD-M shadow and production WS_SHARD_SIZE differ'

meminfo_bytes() {
  local field=$1 value
  value=$(awk -v key="$field:" '
      $1 == key { count += 1; value = $2 }
      END { if (count != 1 || value !~ /^[0-9]+$/) exit 1; print value }
    ' /proc/meminfo) || return 1
  ((value <= 9007199254740991)) || return 1
  printf '%s\n' "$((value * 1024))"
}

host_memory_total_bytes=$(meminfo_bytes MemTotal) \
  || die 'MemTotal is unavailable in /proc/meminfo'
host_swap_total_bytes=$(meminfo_bytes SwapTotal) \
  || die 'SwapTotal is unavailable in /proc/meminfo'
maximum_sequential_phase_memory_bytes=0
for market in "${markets[@]}"; do
  shadow_unit=${unit[$market]}
  [[ -z $(systemctl show "$shadow_unit" --property=DropInPaths --value) ]] \
    || die "$market shadow service has an unexpected systemd drop-in"
  [[ $(systemctl show "$shadow_unit" --property=MemoryHigh --value) == 1879048192 ]] \
    || die "$market shadow service MemoryHigh differs from the gated template"
  [[ $(systemctl show "$shadow_unit" --property=OOMScoreAdjust --value) == 500 ]] \
    || die "$market shadow service OOMScoreAdjust differs from the gated template"
  memory_max=$(systemctl show "$shadow_unit" --property=MemoryMax --value)
  [[ $memory_max == 2147483648 ]] \
    || die "$market shadow service MemoryMax differs from the gated template"
  if ((memory_max > maximum_sequential_phase_memory_bytes)); then
    maximum_sequential_phase_memory_bytes=$memory_max
  fi
done
if ((STRICT_VERIFIER_MEMORY_MAX_BYTES > maximum_sequential_phase_memory_bytes)); then
  maximum_sequential_phase_memory_bytes=$STRICT_VERIFIER_MEMORY_MAX_BYTES
fi
if ((UPLOAD_DRAIN_MEMORY_MAX_BYTES > maximum_sequential_phase_memory_bytes)); then
  maximum_sequential_phase_memory_bytes=$UPLOAD_DRAIN_MEMORY_MAX_BYTES
fi
declare -A production_active_state production_memory_current_bytes
declare -A production_memory_peak_bytes production_memory_max_bytes
declare -A production_memory_growth_target_bytes
production_memory_growth_headroom_bytes=0
for market in "${markets[@]}"; do
  production_unit="binance-lob-archiver-production@${market}.service"
  production_active_state[$market]=$(systemctl show "$production_unit" \
    --property=ActiveState --value)
  production_memory_current=$(systemctl show "$production_unit" \
    --property=MemoryCurrent --value)
  case "${production_active_state[$market]}" in
    active)
      [[ $(systemctl show "$production_unit" --property=SubState --value) == running ]] \
        || die "$market production service is active but not running"
      production_memory_max=$(systemctl show "$production_unit" --property=MemoryMax --value)
      production_memory_high=$(systemctl show "$production_unit" --property=MemoryHigh --value)
      production_memory_peak=$(systemctl show "$production_unit" --property=MemoryPeak --value)
      [[ $production_memory_max =~ ^[0-9]+$ \
        && $production_memory_high =~ ^[0-9]+$ \
        && $production_memory_peak =~ ^[0-9]+$ \
        && $production_memory_current =~ ^[0-9]+$ \
        && $production_memory_high -le $production_memory_max \
        && $production_memory_current -le $production_memory_peak \
        && $production_memory_peak -le $production_memory_max \
        && $production_memory_current -le $production_memory_max ]] \
        || die "$market production memory accounting is invalid"
      production_memory_current_bytes[$market]=$production_memory_current
      production_memory_peak_bytes[$market]=$production_memory_peak
      production_memory_max_bytes[$market]=$production_memory_max
      production_growth=$(monday_production_memory_growth_headroom \
        "$production_memory_current" "$production_memory_peak" \
        "$production_memory_max" "$PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES") \
        || die "$market production memory growth headroom is invalid"
      production_memory_growth_target_bytes[$market]=$(( \
        production_memory_current + production_growth))
      ((production_growth \
        <= 9223372036854775807 - production_memory_growth_headroom_bytes)) \
        || die 'production memory growth headroom overflowed'
      production_memory_growth_headroom_bytes=$(( \
        production_memory_growth_headroom_bytes + production_growth))
      ;;
    inactive)
      if [[ $production_memory_current =~ ^[0-9]+$ ]]; then
        production_memory_current_bytes[$market]=$production_memory_current
      else
        production_memory_current_bytes[$market]=
      fi
      production_memory_peak_bytes[$market]=
      production_memory_max_bytes[$market]=
      production_memory_growth_target_bytes[$market]=
      ;;
    *) die "$market production service has ambiguous ActiveState=${production_active_state[$market]}" ;;
  esac
done
production_memory_current_json=$(jq -cn \
  --arg spot_state "${production_active_state[spot]}" \
  --arg usdm_state "${production_active_state[usdm]}" \
  --arg spot "${production_memory_current_bytes[spot]}" \
  --arg usdm "${production_memory_current_bytes[usdm]}" \
  --arg spot_peak "${production_memory_peak_bytes[spot]}" \
  --arg usdm_peak "${production_memory_peak_bytes[usdm]}" \
  --arg spot_max "${production_memory_max_bytes[spot]}" \
  --arg usdm_max "${production_memory_max_bytes[usdm]}" \
  --arg spot_target "${production_memory_growth_target_bytes[spot]}" \
  --arg usdm_target "${production_memory_growth_target_bytes[usdm]}" \
  'def bytes($value): if $value == "" then null else ($value | tonumber) end;
    {spot:{active_state:$spot_state,current_bytes:bytes($spot),peak_bytes:bytes($spot_peak),
      memory_max_bytes:bytes($spot_max),growth_target_bytes:bytes($spot_target)},
    usdm:{active_state:$usdm_state,current_bytes:bytes($usdm),peak_bytes:bytes($usdm_peak),
      memory_max_bytes:bytes($usdm_max),growth_target_bytes:bytes($usdm_target)}}')

resource_admission_samples_json='[]'
latest_resource_admission_sample_json=null
io_psi_windows_json='[]'
io_psi_previous_total=
io_psi_previous_at=
io_psi_previous_monotonic_us=
io_psi_consecutive_hits=0
io_psi_phase=
io_psi_phase_run=0

io_psi_monotonic_us() {
  awk '{printf "%.0f\n", $1 * 1000000}' /proc/uptime
}

record_io_psi_window() {
  local stage=$1 current_total=$2 current_at=$3 current_monotonic_us=$4
  local transition delta ratio hit consecutive window_us
  [[ $current_monotonic_us =~ ^[1-9][0-9]*$ \
    && $io_psi_previous_monotonic_us =~ ^[1-9][0-9]*$ \
    && $current_monotonic_us -gt $io_psi_previous_monotonic_us ]] || return 2
  window_us=$((current_monotonic_us - io_psi_previous_monotonic_us))
  transition=$(monday_io_full_psi_window \
    "$io_psi_previous_total" "$current_total" "$window_us" "$IO_PSI_WINDOW_US" \
    "$IO_PSI_FULL_DELTA_LIMIT_US" "$io_psi_consecutive_hits") || return $?
  read -r delta ratio hit consecutive <<<"$transition"
  io_psi_windows_json=$(jq -cn \
    --argjson windows "$io_psi_windows_json" \
    --arg phase "$io_psi_phase" \
    --arg stage "$stage" \
    --argjson phase_run "$io_psi_phase_run" \
    --arg started_at "$io_psi_previous_at" \
    --arg finished_at "$current_at" \
    --argjson previous_total_us "$io_psi_previous_total" \
    --argjson current_total_us "$current_total" \
    --argjson delta_us "$delta" \
    --argjson window_us "$window_us" \
    --argjson ratio "$ratio" \
    --argjson hit "$hit" \
    --argjson consecutive_hits "$consecutive" \
    '$windows + [{phase:$phase,phase_run:$phase_run,stage:$stage,
      started_at:$started_at,finished_at:$finished_at,
      previous_total_us:$previous_total_us,current_total_us:$current_total_us,
      delta_us:$delta_us,window_us:$window_us,ratio:$ratio,hit:$hit,
      consecutive_hits:$consecutive_hits}]')
  io_psi_previous_total=$current_total
  io_psi_previous_at=$current_at
  io_psi_previous_monotonic_us=$current_monotonic_us
  io_psi_consecutive_hits=$consecutive
  ((consecutive < IO_PSI_CONSECUTIVE_HIT_LIMIT))
}

read_io_psi_window() {
  local stage=$1 current_total current_at current_monotonic_us
  current_total=$(monday_io_full_psi_total_us "$IO_PSI_SOURCE") \
    || return 2
  current_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  current_monotonic_us=$(io_psi_monotonic_us) || return 2
  record_io_psi_window "$stage" "$current_total" "$current_at" "$current_monotonic_us"
}

sample_io_psi_after_window() {
  local stage=$1
  sleep "$IO_PSI_WINDOW_SECONDS"
  read_io_psi_window "$stage"
}

require_io_psi_window() {
  local stage=$1 status
  if sample_io_psi_after_window "$stage"; then
    return 0
  else
    status=$?
  fi
  if [[ $status -eq 1 ]]; then
    die "I/O full PSI exceeded ${IO_PSI_FULL_DELTA_LIMIT_US}us for ${IO_PSI_CONSECUTIVE_HIT_LIMIT} consecutive windows during phase $io_psi_phase"
  fi
  die "I/O PSI full total is missing, invalid, or regressed during phase $io_psi_phase"
}

begin_io_psi_phase() {
  local phase=$1 index
  io_psi_phase=$phase
  io_psi_phase_run=$((io_psi_phase_run + 1))
  io_psi_consecutive_hits=0
  io_psi_previous_total=$(monday_io_full_psi_total_us "$IO_PSI_SOURCE") \
    || die "I/O PSI full total is missing or invalid before phase $phase"
  io_psi_previous_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  io_psi_previous_monotonic_us=$(io_psi_monotonic_us) \
    || die "monotonic clock is unavailable before phase $phase"
  for index in 1 2 3; do
    require_io_psi_window calibration
  done
}

admit_resource_phase() {
  local phase=$1 phase_memory_max_bytes=$2 sampled_at available required status shortfall sample
  sampled_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  available=$(meminfo_bytes MemAvailable) \
    || die 'MemAvailable is unavailable in /proc/meminfo'
  if required=$(monday_shadow_memory_admission \
    "$available" "$HOST_MEMORY_RESERVE_BYTES" "$phase_memory_max_bytes" \
    "$production_memory_growth_headroom_bytes"); then
    :
  else
    status=$?
    [[ $status -eq 1 ]] \
      || die "resource admission inputs are invalid or overflowed for phase $phase"
    shortfall=$((required - available))
    die "insufficient host memory for phase $phase: available=$available reserve=$HOST_MEMORY_RESERVE_BYTES phase_max=$phase_memory_max_bytes required=$required shortfall=$shortfall"
  fi
  sample=$(jq -cn \
    --arg phase "$phase" \
    --arg sampled_at "$sampled_at" \
    --argjson host_memory_available_bytes "$available" \
    --argjson host_memory_reserve_bytes "$HOST_MEMORY_RESERVE_BYTES" \
    --argjson phase_memory_max_bytes "$phase_memory_max_bytes" \
    --argjson production_memory_growth_margin_bytes \
      "$PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES" \
    --argjson production_memory_growth_headroom_bytes \
      "$production_memory_growth_headroom_bytes" \
    --argjson required_bytes "$required" \
    '{phase:$phase,sampled_at:$sampled_at,
      host_memory_available_bytes:$host_memory_available_bytes,
      host_memory_reserve_bytes:$host_memory_reserve_bytes,
      phase_memory_max_bytes:$phase_memory_max_bytes,
      production_memory_growth_margin_bytes:$production_memory_growth_margin_bytes,
      production_memory_growth_headroom_bytes:$production_memory_growth_headroom_bytes,
      required_bytes:$required_bytes}')
  resource_admission_samples_json=$(jq -cn \
    --argjson samples "$resource_admission_samples_json" \
    --argjson sample "$sample" '$samples + [$sample]')
  latest_resource_admission_sample_json=$sample
}

assert_host_memory_reserve() {
  local available
  available=$(meminfo_bytes MemAvailable) \
    || die 'MemAvailable is unavailable in /proc/meminfo'
  ((available >= HOST_MEMORY_RESERVE_BYTES)) \
    || die "host memory reserve was consumed during the active Shadow phase: available=$available reserve=$HOST_MEMORY_RESERVE_BYTES"
}

begin_io_psi_phase resource-preflight
admit_resource_phase resource-preflight "$maximum_sequential_phase_memory_bytes"
resource_preflight_json=$latest_resource_admission_sample_json
resource_preflight_psi_windows_json=$io_psi_windows_json

[[ ${base_spool_dir[spot]} == /data/monday/spool/binance-lob-rust-shadow/spot ]] \
  || die 'Spot shadow spool path is not isolated'
[[ ${base_spool_dir[usdm]} == /data/monday/spool/binance-lob-rust-shadow/usdm ]] \
  || die 'USD-M shadow spool path is not isolated'
[[ ${dataset[spot]} == spot_all_rust_shadow ]] || die 'Spot shadow dataset is not isolated'
[[ ${dataset[usdm]} == usdm_perpetual_top100_lob_rust_shadow ]] \
  || die 'USD-M shadow dataset is not isolated'
min_symbols[spot]=1000
min_symbols[usdm]=100
# A v2 tape candidate declares this exact per-symbol stream-type list in its
# manifest and every session_start row (sorted); forceOrder is USD-M only. A
# v1 candidate keeps the legacy depth@100ms+aggTrade pair and must not carry
# the new families, so both schema generations remain gateable during the
# transition.
expected_stream_types[spot]='["aggTrade","bookTicker","depth@100ms","trade"]'
expected_stream_types[usdm]='["depth@100ms"]'

if [[ $resource_preflight_only == true ]]; then
  preflight_schema=monday.rust_lob_gate_resource_preflight.v1
  [[ $pair_mode == true ]] && preflight_schema=monday.rust_lob_gate_resource_preflight.v2
  jq -cn \
    --arg schema "$preflight_schema" \
    --arg candidate_sha256 "$candidate_sha" \
    --arg runtime_contract_sha256 "$runtime_contract_sha256" \
    --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
    --arg deployment_source_revision "$deployment_source_revision" \
    --arg controller_release_sha256 "$controller_release_sha256" \
    --arg controller_deployment_bundle_sha256 "$controller_deployment_bundle_sha256" \
    --arg controller_deployment_source_revision "$controller_deployment_source_revision" \
    --argjson host_memory_total_bytes "$host_memory_total_bytes" \
    --argjson host_swap_total_bytes "$host_swap_total_bytes" \
    --argjson production_memory_current_bytes "$production_memory_current_json" \
    --argjson maximum_sequential_phase_memory_bytes \
      "$maximum_sequential_phase_memory_bytes" \
    --argjson resource_preflight "$resource_preflight_json" \
    --argjson io_full_psi_windows "$resource_preflight_psi_windows_json" \
    '{schema:$schema,candidate_sha256:$candidate_sha256,
      runtime_contract_sha256:$runtime_contract_sha256,
      deployment_bundle_sha256:$deployment_bundle_sha256,
      deployment_source_revision:$deployment_source_revision,
      host_memory_total_bytes:$host_memory_total_bytes,
      host_swap_total_bytes:$host_swap_total_bytes,
      production_memory_current_bytes:$production_memory_current_bytes,
      maximum_sequential_phase_memory_bytes:$maximum_sequential_phase_memory_bytes,
      resource_preflight:$resource_preflight,
      io_full_psi_windows:$io_full_psi_windows,passed:true}
      + (if $controller_release_sha256 == "" then {} else {
        controller_release_sha256:$controller_release_sha256,
        controller_deployment_bundle_sha256:$controller_deployment_bundle_sha256,
        controller_deployment_source_revision:$controller_deployment_source_revision
      } end)'
  exit 0
fi

binary_evidence_dir="$EVIDENCE_ROOT/$candidate_sha"
runtime_evidence_dir="$binary_evidence_dir/$runtime_contract_sha256"
if [[ $pair_mode == true ]]; then
  runtime_evidence_dir="$binary_evidence_dir/$runtime_contract_sha256/$controller_release_sha256"
fi
runs_dir="$runtime_evidence_dir/runs"
gate_run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run_spool_path="$RUN_SPOOL_ROOT/$candidate_sha/$gate_run_id"
for market in "${markets[@]}"; do
  spool_dir[$market]=$(run_spool_dir "$candidate_sha" "$gate_run_id" "$market")
  override_file[$market]="$OVERRIDE_ROOT/binance-lob-archiver-rust-${market}-soak.env"
done
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
install -d -m 0750 "$runtime_evidence_dir" "$runs_dir"
direct_directory "$runtime_evidence_dir" \
  || die 'runtime contract evidence directory is indirect or a symlink'
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
for path in /data/monday /data/monday/spool /data/monday/spool/binance-lob-rust-shadow; do
  direct_directory "$path" || die "shadow spool parent is indirect or a symlink: $path"
done
for path in "$RUN_SPOOL_ROOT" "$RUN_SPOOL_ROOT/$candidate_sha" "$run_spool_path"; do
  direct_directory_or_absent "$path" || die "run-scoped spool path is indirect: $path"
done
[[ ! -e $run_spool_path && ! -L $run_spool_path ]] \
  || die 'run-scoped spool already exists'
install -d -m 0755 -o root -g root \
  "$RUN_SPOOL_ROOT" "$RUN_SPOOL_ROOT/$candidate_sha"
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" \
  "$run_spool_path" "${spool_dir[spot]}" "${spool_dir[usdm]}"
for path in "$RUN_SPOOL_ROOT" "$RUN_SPOOL_ROOT/$candidate_sha" "$run_spool_path" \
  "${spool_dir[spot]}" "${spool_dir[usdm]}"; do
  direct_directory "$path" || die "run-scoped spool path is indirect: $path"
done
run_created_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_json="$evidence_dir/run.json"
run_json_tmp="$evidence_dir/.run.json.tmp"
run_schema=monday.rust_lob_shadow_gate_run.v1
[[ $pair_mode == true ]] && run_schema=monday.rust_lob_shadow_gate_run.v2
write_run_json() {
  jq -n \
    --arg schema "$run_schema" \
    --arg run_id "$gate_run_id" \
    --arg created_at "$run_created_at" \
    --arg candidate_sha256 "$candidate_sha" \
    --arg runtime_contract_sha256 "$runtime_contract_sha256" \
    --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
    --arg deployment_source_revision "$deployment_source_revision" \
    --arg controller_release_sha256 "$controller_release_sha256" \
    --arg controller_deployment_bundle_sha256 "$controller_deployment_bundle_sha256" \
    --arg controller_deployment_source_revision "$controller_deployment_source_revision" \
    --arg run_spool "$run_spool_path" \
    --argjson segment_seconds "$GATE_SEGMENT_SECONDS" \
    --argjson requested_duration_seconds "$gate_seconds" \
    --argjson health_settle_seconds "$health_settle_seconds" \
    --argjson host_memory_total_bytes "$host_memory_total_bytes" \
    --argjson host_swap_total_bytes "$host_swap_total_bytes" \
    --argjson production_memory_current_bytes "$production_memory_current_json" \
    --argjson maximum_sequential_phase_memory_bytes \
      "$maximum_sequential_phase_memory_bytes" \
    --argjson resource_preflight "$resource_preflight_json" \
    --argjson resource_admission_samples "$resource_admission_samples_json" \
    --argjson io_full_psi_windows "$io_psi_windows_json" \
    --argjson test_only "$test_only" \
    '{schema:$schema,run_id:$run_id,created_at:$created_at,
      candidate_sha256:$candidate_sha256,
      runtime_contract_sha256:$runtime_contract_sha256,
      deployment_bundle_sha256:$deployment_bundle_sha256,
      deployment_source_revision:$deployment_source_revision,
      run_spool:$run_spool,segment_seconds:$segment_seconds,
      requested_duration_seconds:$requested_duration_seconds,
      health_settle_seconds:$health_settle_seconds,
      host_memory_total_bytes:$host_memory_total_bytes,
      host_swap_total_bytes:$host_swap_total_bytes,
      production_memory_current_bytes:$production_memory_current_bytes,
      maximum_sequential_phase_memory_bytes:$maximum_sequential_phase_memory_bytes,
      resource_preflight:$resource_preflight,
      resource_admission_samples:$resource_admission_samples,
      io_full_psi_windows:$io_full_psi_windows,
      test_only:$test_only}
      + (if $controller_release_sha256 == "" then {} else {
        controller_release_sha256:$controller_release_sha256,
        controller_deployment_bundle_sha256:$controller_deployment_bundle_sha256,
        controller_deployment_source_revision:$controller_deployment_source_revision
      } end)' >"$run_json_tmp"
  chmod 0640 "$run_json_tmp"
  mv -Tf "$run_json_tmp" "$run_json"
}
write_run_json

tmp_dir=$(mktemp -d)
chown "$SERVICE_USER:$SERVICE_USER" "$tmp_dir"
chmod 0750 "$tmp_dir"
gate_finished=false
strict_verifier_unit=
strict_verifier_counter=0
upload_drain_unit=
upload_drain_counter=0
io_psi_monitor_pid=
io_psi_monitor_file=
active_process_pid=
active_process_pgid=
active_process_file="$tmp_dir/active-process"
transient_control_group_has_tasks() {
  local control_group=$1 tasks_file task
  [[ -z $control_group ]] && return 1
  [[ $control_group == /* && $control_group != */../* ]] || return 2
  tasks_file="/sys/fs/cgroup${control_group}/cgroup.procs"
  [[ -f $tasks_file && ! -L $tasks_file ]] || return 2
  if IFS= read -r task <"$tasks_file"; then
    [[ $task =~ ^[1-9][0-9]*$ ]] || return 2
    return 0
  fi
  return 1
}
transient_unit_is_drained() {
  local transient_unit=$1 active_state control_group task_status
  active_state=$(systemctl show --property=ActiveState --value "$transient_unit" \
    2>/dev/null) || return 1
  control_group=$(systemctl show --property=ControlGroup --value "$transient_unit" \
    2>/dev/null) || return 1
  [[ $active_state == inactive ]] || return 1
  if transient_control_group_has_tasks "$control_group"; then
    return 1
  else
    task_status=$?
  fi
  [[ $task_status == 1 ]]
}
stop_transient_unit() {
  local transient_unit=$1 stop_failed=false second
  if ! systemctl stop --no-block "$transient_unit" >/dev/null 2>&1; then
    stop_failed=true
  fi
  if [[ $stop_failed == false ]]; then
    for second in {1..5}; do
      transient_unit_is_drained "$transient_unit" && return 0
      sleep 1
    done
  fi
  systemctl kill --kill-who=all --signal=KILL "$transient_unit" \
    >/dev/null 2>&1 || true
  for second in {1..5}; do
    transient_unit_is_drained "$transient_unit" && return 0
    sleep 1
  done
  printf 'shadow gate failed: transient unit did not drain after bounded KILL: %s\n' \
    "$transient_unit" >&2
  return 1
}
run_transient_unit_command() {
  local transient_unit=$1 active_state sub_state exec_code exec_status result
  shift
  "$@" || return $?
  while :; do
    active_state=$(systemctl show --property=ActiveState --value "$transient_unit" \
      2>/dev/null) || return 1
    sub_state=$(systemctl show --property=SubState --value "$transient_unit" \
      2>/dev/null) || return 1
    if [[ $active_state == active && $sub_state == exited ]]; then
      exec_code=$(systemctl show --property=ExecMainCode --value "$transient_unit" \
        2>/dev/null) || return 1
      exec_status=$(systemctl show --property=ExecMainStatus --value "$transient_unit" \
        2>/dev/null) || return 1
      result=$(systemctl show --property=Result --value "$transient_unit" \
        2>/dev/null) || return 1
      [[ $exec_code == 1 && $exec_status == 0 && $result == success ]]
      return
    fi
    [[ $active_state == active || $active_state == activating ]] || return 1
    sleep 1
  done
}
stop_strict_verifier() {
  if [[ -n $strict_verifier_unit ]]; then
    stop_transient_unit "$strict_verifier_unit" || return 1
    strict_verifier_unit=
  fi
}
stop_upload_drain() {
  if [[ -n $upload_drain_unit ]]; then
    stop_transient_unit "$upload_drain_unit" || return 1
    upload_drain_unit=
  fi
}

terminate_active_process_file() {
  local file=$1 pid pgid signal second
  [[ -s $file ]] || return 0
  read -r pid pgid <"$file" || return 0
  [[ $pid =~ ^[1-9][0-9]*$ && $pgid =~ ^[1-9][0-9]*$ ]] || return 0
  kill -0 "$pid" >/dev/null 2>&1 || return 0
  for signal in TERM KILL; do
    kill -"$signal" -- "-$pgid" >/dev/null 2>&1 \
      || kill -"$signal" "$pid" >/dev/null 2>&1 || true
    if [[ $signal == TERM ]]; then
      second=$ACTIVE_PROCESS_TERM_GRACE_SECONDS
    else
      second=$ACTIVE_PROCESS_KILL_GRACE_SECONDS
    fi
    while ((second > 0)) && kill -0 "$pid" >/dev/null 2>&1; do
      sleep 1
      second=$((second - 1))
    done
    kill -0 "$pid" >/dev/null 2>&1 || return 0
  done
  return 1
}

io_psi_runtime_monitor() {
  local phase=$1 phase_run=$2 output=$3 active_file=$4 gate_pid=$5 ready_file=$6
  local previous_total previous_at previous_mono current_total current_at current_mono
  local window_us transition delta ratio hit consecutive=0 stop_requested=false
  trap 'stop_requested=true' TERM
  previous_total=$(monday_io_full_psi_total_us "$IO_PSI_SOURCE") || {
    kill -TERM "$gate_pid" >/dev/null 2>&1 || true
    return 76
  }
  previous_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  previous_mono=$(io_psi_monotonic_us) || {
    kill -TERM "$gate_pid" >/dev/null 2>&1 || true
    return 76
  }
  : >"$ready_file"
  while :; do
    sleep "$IO_PSI_WINDOW_SECONDS" &
    wait $! >/dev/null 2>&1 || true
    current_total=$(monday_io_full_psi_total_us "$IO_PSI_SOURCE") || {
      terminate_active_process_file "$active_file" || true
      kill -TERM "$gate_pid" >/dev/null 2>&1 || true
      return 76
    }
    current_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    current_mono=$(io_psi_monotonic_us) || {
      terminate_active_process_file "$active_file" || true
      kill -TERM "$gate_pid" >/dev/null 2>&1 || true
      return 76
    }
    ((current_mono > previous_mono)) || {
      terminate_active_process_file "$active_file" || true
      kill -TERM "$gate_pid" >/dev/null 2>&1 || true
      return 76
    }
    window_us=$((current_mono - previous_mono))
    transition=$(monday_io_full_psi_window "$previous_total" "$current_total" \
      "$window_us" "$IO_PSI_WINDOW_US" "$IO_PSI_FULL_DELTA_LIMIT_US" \
      "$consecutive") || {
      terminate_active_process_file "$active_file" || true
      kill -TERM "$gate_pid" >/dev/null 2>&1 || true
      return 76
    }
    read -r delta ratio hit consecutive <<<"$transition"
    jq -cn --arg phase "$phase" --argjson phase_run "$phase_run" \
      --arg stage runtime --arg started_at "$previous_at" --arg finished_at "$current_at" \
      --argjson previous_total_us "$previous_total" --argjson current_total_us "$current_total" \
      --argjson delta_us "$delta" --argjson window_us "$window_us" \
      --argjson ratio "$ratio" --argjson hit "$hit" \
      --argjson consecutive_hits "$consecutive" \
      '{phase:$phase,phase_run:$phase_run,stage:$stage,started_at:$started_at,
        finished_at:$finished_at,previous_total_us:$previous_total_us,
        current_total_us:$current_total_us,delta_us:$delta_us,window_us:$window_us,
        ratio:$ratio,hit:$hit,consecutive_hits:$consecutive_hits}' >>"$output"
    if ((consecutive >= IO_PSI_CONSECUTIVE_HIT_LIMIT)); then
      terminate_active_process_file "$active_file" || true
      kill -TERM "$gate_pid" >/dev/null 2>&1 || true
      return 75
    fi
    previous_total=$current_total
    previous_at=$current_at
    previous_mono=$current_mono
    [[ $stop_requested == true ]] && return 0
  done
}

start_io_psi_runtime_monitor() {
  local ready_file index
  [[ -z $io_psi_monitor_pid ]] || die 'I/O PSI runtime monitor is already active'
  io_psi_monitor_file="$tmp_dir/io-psi-${io_psi_phase_run}.ndjson"
  ready_file="$tmp_dir/io-psi-${io_psi_phase_run}.ready"
  : >"$io_psi_monitor_file"
  rm -f -- "$ready_file"
  io_psi_runtime_monitor "$io_psi_phase" "$io_psi_phase_run" \
    "$io_psi_monitor_file" "$active_process_file" "$$" "$ready_file" &
  io_psi_monitor_pid=$!
  for index in {1..50}; do
    [[ -f $ready_file ]] && return 0
    kill -0 "$io_psi_monitor_pid" >/dev/null 2>&1 \
      || die "I/O PSI runtime monitor failed to start for phase $io_psi_phase"
    sleep 0.1
  done
  die "I/O PSI runtime monitor start timed out for phase $io_psi_phase"
}

finish_io_psi_runtime_monitor() {
  local status=0 runtime_windows='[]' second=$((IO_PSI_WINDOW_SECONDS + 2))
  [[ -n $io_psi_monitor_pid ]] || return 0
  kill -TERM "$io_psi_monitor_pid" >/dev/null 2>&1 || true
  while ((second > 0)) && jobs -pr | grep -Fxq "$io_psi_monitor_pid"; do
    sleep 1
    second=$((second - 1))
  done
  if jobs -pr | grep -Fxq "$io_psi_monitor_pid"; then
    kill -KILL "$io_psi_monitor_pid" >/dev/null 2>&1 || true
    status=76
  elif wait "$io_psi_monitor_pid"; then
    :
  else
    status=$?
  fi
  if [[ -s $io_psi_monitor_file ]]; then
    runtime_windows=$(jq -sc '.' "$io_psi_monitor_file") || status=76
    io_psi_windows_json=$(jq -cn --argjson existing "$io_psi_windows_json" \
      --argjson runtime "$runtime_windows" '$existing + $runtime')
  fi
  io_psi_monitor_pid=
  io_psi_monitor_file=
  ((status == 0)) || return "$status"
}

assert_io_psi_runtime_monitor() {
  if [[ -z $io_psi_monitor_pid ]] \
    || ! jobs -pr | grep -Fxq "$io_psi_monitor_pid"; then
    die "I/O PSI runtime monitor stopped during phase $io_psi_phase"
  fi
}

run_active_io_psi_command() {
  local stop_callback=$1 child status monitor_interrupted=false callback_failed=false
  shift
  set -m
  "$@" &
  child=$!
  active_process_pid=$child
  active_process_pgid=$child
  printf '%s %s\n' "$active_process_pid" "$active_process_pgid" \
    >"$active_process_file.tmp"
  mv -Tf "$active_process_file.tmp" "$active_process_file"
  set +m
  while jobs -pr | grep -Fxq "$child"; do
    if [[ -n ${io_psi_monitor_pid:-} ]] \
      && ! jobs -pr | grep -Fxq "$io_psi_monitor_pid"; then
      monitor_interrupted=true
      terminate_active_process_file "$active_process_file" || true
      "$stop_callback" || callback_failed=true
      break
    fi
    sleep 1
  done
  if wait "$child"; then
    status=0
  else
    status=$?
    if [[ $monitor_interrupted != true ]]; then
      "$stop_callback" || callback_failed=true
    fi
  fi
  rm -f -- "$active_process_file"
  active_process_pid=
  active_process_pgid=
  [[ $callback_failed == false ]] || return 77
  [[ $monitor_interrupted == false ]] || return 76
  return "$status"
}

run_io_psi_phase_command() {
  local phase=$1 phase_memory_max_bytes=$2 stop_callback=$3 status=0
  shift 3
  begin_io_psi_phase "$phase"
  admit_resource_phase "$phase" "$phase_memory_max_bytes"
  start_io_psi_runtime_monitor
  run_active_io_psi_command "$stop_callback" "$@" || status=$?
  finish_io_psi_runtime_monitor || status=$?
  return "$status"
}
cleanup() {
  local status=$? cleanup_failed=false
  terminate_active_process_file "$active_process_file" >/dev/null 2>&1 \
    || cleanup_failed=true
  finish_io_psi_runtime_monitor >/dev/null 2>&1 || cleanup_failed=true
  stop_strict_verifier || cleanup_failed=true
  stop_upload_drain || cleanup_failed=true
  write_run_json >/dev/null 2>&1 || true
  if [[ $gate_finished != true ]]; then
    systemctl stop "${unit[spot]}" "${unit[usdm]}" >/dev/null 2>&1 || true
  fi
  for market in "${markets[@]}"; do
    rm -f -- "${override_file[$market]}"
  done
  rm -rf "$tmp_dir"
  rm -f "$run_json_tmp" "$gate_tmp" "$marker_tmp"
  if [[ $cleanup_failed == true ]]; then
    printf 'shadow gate failed: bounded process or transient-unit cleanup was incomplete\n' >&2
    ((status != 0)) || status=1
  fi
  exit "$status"
}
trap 'exit 143' HUP INT TERM
trap cleanup EXIT

assert_candidate() {
  [[ -L $SHADOW_BINARY ]] || die 'shadow candidate symlink disappeared'
  [[ $(readlink -f "$SHADOW_BINARY") == "$candidate_binary" ]] \
    || die 'shadow candidate symlink changed during the gate'
  printf '%s  %s\n' "$candidate_sha" "$SHADOW_BINARY" \
    | sha256sum --check --strict >/dev/null
}

assert_pair_identity() {
  [[ $pair_mode == true ]] || return 0
  verify_controller_release "$controller_release_sha256"
  printf '%s  %s\n' "$candidate_sha" "$candidate_binary" \
    | sha256sum --check --strict >/dev/null \
    || die 'candidate binary changed before Gate finalization'
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
  local market=$1 status
  upload_drain_counter=$((upload_drain_counter + 1))
  upload_drain_unit="monday-rust-upload-drain-$$-${market}-${upload_drain_counter}.service"
  if run_io_psi_phase_command "upload-drain-$market" \
    "$UPLOAD_DRAIN_MEMORY_MAX_BYTES" stop_upload_drain \
    run_transient_unit_command "$upload_drain_unit" systemd-run --quiet \
    --unit="$upload_drain_unit" \
    --property=RemainAfterExit=yes \
    --property=KillMode=control-group \
    --property=OOMScoreAdjust=500 \
    --property=CPUQuota=80% \
    --property=MemoryHigh=384M \
    --property=MemoryMax=512M \
    -- runuser --user "$SERVICE_USER" -- env -i \
      HOME="$SERVICE_HOME" \
      PATH="$SAFE_PATH" \
      RUST_LOG=info \
      SPOOL_DIR="${spool_dir[$market]}" \
      OSS_BUCKET="${oss_bucket[$market]}" \
      OSS_ENDPOINT="${oss_endpoint[$market]}" \
      OSS_REGION="${oss_region[$market]}" \
      ALIYUN_PROFILE="${aliyun_profile[$market]}" \
      OSS_COPY_TIMEOUT_SECONDS="${oss_copy_timeout[$market]}" \
      "$candidate_binary" --upload-only; then
    stop_upload_drain || return 1
  else
    status=$?
    stop_upload_drain || return 1
    return "$status"
  fi
  assert_spool_drained "$market"
}

run_strict_verifier() {
  local verifier_status=0 outer_phase='' outer_monitor=false
  if [[ -n ${io_psi_monitor_pid:-} ]]; then
    outer_phase=$io_psi_phase
    outer_monitor=true
    finish_io_psi_runtime_monitor
  fi
  strict_verifier_counter=$((strict_verifier_counter + 1))
  strict_verifier_unit="monday-rust-strict-verifier-$$-${strict_verifier_counter}.service"
  if run_io_psi_phase_command "strict-verifier-$strict_verifier_counter" \
    "$STRICT_VERIFIER_MEMORY_MAX_BYTES" stop_strict_verifier \
    run_transient_unit_command "$strict_verifier_unit" systemd-run --quiet \
    --unit="$strict_verifier_unit" \
    --property=RemainAfterExit=yes \
    --property=KillMode=control-group \
    --property=OOMScoreAdjust=500 \
    --property=MemoryHigh=1280M \
    --property=MemoryMax=1536M \
    -- "$candidate_binary" "$@" >/dev/null; then
    stop_strict_verifier || verifier_status=1
  else
    verifier_status=$?
    stop_strict_verifier || verifier_status=1
  fi
  if [[ $outer_monitor == true ]]; then
    begin_io_psi_phase "$outer_phase"
    start_io_psi_runtime_monitor
  fi
  return "$verifier_status"
}

run_strict_verifier_pair() {
  run_strict_verifier --require-lob-continuity "$@"
}

verify_adjacent_segments() {
  local previous_path=
  local previous_content_sha256=
  local previous_manifest_sha256=
  local path content_sha256 manifest_sha256 pairs=0
  (( $# > 0 && $# % 3 == 0 )) \
    || die 'strict verifier requires one or more complete segment trust anchors'
  while (($#)); do
    path=$1
    content_sha256=$2
    manifest_sha256=$3
    shift 3
    if [[ -n $previous_path ]]; then
      # Adjacent pairs retain every per-segment check and every stateful
      # cross-segment transition without retaining the full observation in RAM.
      run_strict_verifier_pair \
        --verify-segment "$previous_path" \
        --segment-content-sha256 "$previous_content_sha256" \
        --segment-manifest-sha256 "$previous_manifest_sha256" \
        --verify-segment "$path" \
        --segment-content-sha256 "$content_sha256" \
        --segment-manifest-sha256 "$manifest_sha256" \
        || die 'strict aggregate-trade and LOB continuity readback failed'
      pairs=$((pairs + 1))
    fi
    previous_path=$path
    previous_content_sha256=$content_sha256
    previous_manifest_sha256=$manifest_sha256
  done
  ((pairs > 0)) || die 'strict verifier requires at least two complete segments'
}

verify_aggregate_trade_continuity() {
  local -a verifier_args=(--verify-aggregate-trade-continuity)
  local path content_sha256 manifest_sha256
  (( $# > 0 && $# % 3 == 0 )) \
    || die 'aggregate-trade verifier requires one or more complete segment trust anchors'
  while (($#)); do
    path=$1
    content_sha256=$2
    manifest_sha256=$3
    shift 3
    verifier_args+=(
      --verify-segment "$path"
      --segment-content-sha256 "$content_sha256"
      --segment-manifest-sha256 "$manifest_sha256"
    )
  done
  run_strict_verifier "${verifier_args[@]}" \
    || die 'strict aggregate-trade continuity readback failed'
}

verify_raw_trade_continuity() {
  local -a verifier_args=(--verify-raw-trade-continuity)
  local path content_sha256 manifest_sha256
  (( $# > 0 && $# % 3 == 0 )) \
    || die 'raw-trade verifier requires one or more complete segment trust anchors'
  while (($#)); do
    path=$1
    content_sha256=$2
    manifest_sha256=$3
    shift 3
    verifier_args+=(
      --verify-segment "$path"
      --segment-content-sha256 "$content_sha256"
      --segment-manifest-sha256 "$manifest_sha256"
    )
  done
  run_strict_verifier "${verifier_args[@]}" \
    || die 'strict raw-trade continuity readback failed'
}

systemctl stop "${unit[spot]}" "${unit[usdm]}"
direct_directory_or_absent "$OVERRIDE_ROOT" || die 'runtime override directory is indirect'
install -d -m 0755 "$OVERRIDE_ROOT"
direct_directory "$OVERRIDE_ROOT" || die 'runtime override directory is indirect'
for market in "${markets[@]}"; do
  assert_spool_drained "$market"
  rm -f -- "${override_file[$market]}"
  {
    printf 'SPOOL_DIR=%s\n' "${spool_dir[$market]}"
    printf 'SEGMENT_SECONDS=%s\n' "$GATE_SEGMENT_SECONDS"
  } >"$tmp_dir/$market-gate.env"
  install -m 0640 "$tmp_dir/$market-gate.env" "${override_file[$market]}"
  secure_regular_file "${override_file[$market]}"
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
declare -A market_gate_started_ns market_observation_started_ns
declare -A observed_session frozen_symbol_count frozen_catalog_sha256 configured_catalog_sha256
declare -A initial_upload_failure_count last_health_updated_ns health_samples
declare -A last_health_advance_mono max_health_silence_seconds
declare -A pre_observation_segment current_segment
declare -A observed_runtime_seconds cpu_usage_ns memory_peak_bytes health_sha256
declare -A symbol_count snapshot_ready_count stream_coverage_verified_count sequence_gaps
declare -A full_stream_coverage_verified

health_passes() {
  local market=$1
  local health="${spool_dir[$market]}/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  jq -e \
    --arg market "$market" \
    --arg dataset "${dataset[$market]}" \
    --arg symbols_config "${configured_symbols[$market]}" \
    --argjson minimum_symbols "${min_symbols[$market]}" \
    --argjson gate_started_ns "${market_gate_started_ns[$market]}" \
    '.market == $market
      and .dataset == $dataset
      and .updated_at_ns >= $gate_started_ns
      and .status == "synced"
      and .sequence_gaps == 0
      and (.symbol_count | type) == "number"
      and .symbol_count == (.symbol_count | floor)
      and (if $market == "usdm" then .symbol_count == $minimum_symbols
        else .symbol_count >= $minimum_symbols end)
      and (if $market == "usdm"
        then (.symbols | keys | sort) == ($symbols_config | split(",") | sort)
        else true end)
      and (.snapshot_ready_count | type) == "number"
      and .snapshot_ready_count == (.snapshot_ready_count | floor)
      and .snapshot_ready_count == .symbol_count
      and .bridged_count == .symbol_count
      and .stream_coverage_verified_count == .symbol_count
      and .snapshot_only_symbols == []
      and .all_symbols_bridged == true
      and .all_stream_coverage_verified == true
      and ((.full_stream_coverage_verified == null)
        or (.full_stream_coverage_verified == true))
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

validate_running_sample() {
  local market=$1 memory_now
  assert_io_psi_runtime_monitor
  assert_candidate
  assert_host_memory_reserve
  systemctl is-active --quiet "${unit[$market]}" \
    || die "$market shadow service stopped before observation completed"
  [[ $(systemctl_value "$market" NRestarts) == 0 ]] \
    || die "$market shadow service restarted before observation completed"
  memory_now=$(require_uint "$(systemctl_value "$market" MemoryCurrent)" \
    "$market MemoryCurrent")
  ((memory_now > max_memory_bytes[$market])) && max_memory_bytes[$market]=$memory_now
  ((memory_now <= memory_max_bytes[$market])) \
    || die "$market memory usage exceeded MemoryMax"
  validate_observation_sample "$market"
}

active_segment_start_ns() {
  local directory=$1
  find "$directory" -type f -name 'part-*.jsonl.part' -print \
    | sed -n 's#^.*/part-\([1-9][0-9]*\)\.jsonl\.part$#\1#p' \
    | sort -n \
    | sed -n '$p'
}

run_market_gate_phase() {
  local market=$1 other health quota_raw settle_deadline alignment_deadline
  local observation_started_mono observation_deadline now_mono remaining interval
  local minimum_health_samples now_monotonic_us cpu_end_ns allowed_cpu_ns memory_now peak_raw

  for other in "${markets[@]}"; do
    if [[ $other != "$market" ]]; then
      systemctl is-active --quiet "${unit[$other]}" \
        && die "$other shadow service is active before the $market phase"
    fi
  done
  assert_candidate
  begin_io_psi_phase "shadow-$market"
  admit_resource_phase "shadow-$market" 2147483648
  start_io_psi_runtime_monitor
  market_gate_started_ns[$market]=$(date +%s%N)
  systemctl start "${unit[$market]}"
  systemctl is-active --quiet "${unit[$market]}" || die "$market shadow service is not active"
  [[ $(systemctl_value "$market" ActiveState) == active ]] \
    || die "$market shadow service did not enter ActiveState=active"
  [[ $(systemctl_value "$market" SubState) == running ]] \
    || die "$market shadow service did not enter SubState=running"
  [[ $(systemctl_value "$market" OOMScoreAdjust) == 500 ]] \
    || die "$market shadow service OOMScoreAdjust differs from the gated template"
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
  [[ -z $(systemctl_value "$market" DropInPaths) ]] \
    || die "$market shadow service has an unexpected systemd drop-in"
  [[ $(systemctl_value "$market" MemoryHigh) == 1879048192 ]] \
    || die "$market shadow service MemoryHigh differs from the gated template"
  memory_max_bytes[$market]=$(require_uint "$(systemctl_value "$market" MemoryMax)" \
    "$market MemoryMax")
  ((memory_max_bytes[$market] == 2147483648)) \
    || die "$market shadow service MemoryMax differs from the gated template"
  max_memory_bytes[$market]=$(require_uint "$(systemctl_value "$market" MemoryCurrent)" \
    "$market MemoryCurrent")

  settle_deadline=$(( $(monotonic_seconds) + health_settle_seconds ))
  while ! health_passes "$market"; do
    assert_io_psi_runtime_monitor
    (( $(monotonic_seconds) < settle_deadline )) \
      || die "$market shadow health did not reach the fail-closed gate before the settle deadline"
    systemctl is-active --quiet "${unit[$market]}" \
      || die "$market shadow service stopped while settling"
    [[ $(systemctl_value "$market" NRestarts) == 0 ]] \
      || die "$market shadow service restarted while settling"
    assert_host_memory_reserve
    sleep "$IO_PSI_WINDOW_SECONDS"
  done

  health="${spool_dir[$market]}/health.json"
  observed_session[$market]=$(jq -er '.session_id' "$health")
  frozen_symbol_count[$market]=$(jq -er '.symbol_count' "$health")
  frozen_catalog_sha256[$market]=$(health_catalog_sha256 "$market")
  if [[ $market == usdm ]]; then
    configured_catalog_sha256[$market]=$(jq -cn \
      --arg symbols "${configured_symbols[$market]}" \
      '$symbols | split(",") | sort' | sha256sum | awk '{print $1}')
  else
    configured_catalog_sha256[$market]=${frozen_catalog_sha256[$market]}
  fi
  initial_upload_failure_count[$market]=$(jq -er '.upload_failure_count' "$health")
  last_health_updated_ns[$market]=$(jq -er '.updated_at_ns' "$health")
  last_health_advance_mono[$market]=$(monotonic_seconds)
  max_health_silence_seconds[$market]=0
  health_samples[$market]=1

  pre_observation_segment[$market]=$(active_segment_start_ns "${spool_dir[$market]}")
  [[ ${pre_observation_segment[$market]} =~ ^[1-9][0-9]{0,18}$ ]] \
    || die "$market has no valid active segment before observation"
  alignment_deadline=$(( $(monotonic_seconds) + GATE_SEGMENT_SECONDS + MAX_HEALTH_SILENCE_SECONDS ))
  while :; do
    assert_io_psi_runtime_monitor
    validate_running_sample "$market"
    current_segment[$market]=$(active_segment_start_ns "${spool_dir[$market]}")
    [[ ${current_segment[$market]} =~ ^[1-9][0-9]{0,18}$ ]] \
      || die "$market lost its active segment before observation"
    ((current_segment[$market] > pre_observation_segment[$market])) && break
    (( $(monotonic_seconds) < alignment_deadline )) \
      || die "$market shadow segments did not rotate after health settled"
    sleep "$IO_PSI_WINDOW_SECONDS"
  done
  market_observation_started_ns[$market]=$(date +%s%N)

  observation_started_mono=$(monotonic_seconds)
  observation_deadline=$((observation_started_mono + gate_seconds))
  while (( $(monotonic_seconds) < observation_deadline )); do
    now_mono=$(monotonic_seconds)
    remaining=$((observation_deadline - now_mono))
    interval=$IO_PSI_WINDOW_SECONDS
    ((remaining < interval)) && interval=$remaining
    if ((interval > 0)); then
      sleep "$interval"
    fi
    validate_running_sample "$market"
  done

  if [[ $test_only != true ]]; then
    minimum_health_samples=$((REQUIRED_DURATION_SECONDS / 30))
    ((health_samples[$market] >= minimum_health_samples)) \
      || die "$market health did not advance often enough during observation"
  fi

  now_monotonic_us=$(awk '{printf "%.0f\n", $1 * 1000000}' /proc/uptime)
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
  full_stream_coverage_verified[$market]=$(jq -c '.full_stream_coverage_verified' "$health")

  systemctl stop "${unit[$market]}"
  finish_io_psi_runtime_monitor
  systemctl is-active --quiet "${unit[$market]}" \
    && die "$market shadow service remained active after stop"
  assert_candidate
  run_candidate_drain "$market"
}

for market in "${markets[@]}"; do
  run_market_gate_phase "$market"
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
  max_age_seconds=$((gate_seconds + health_settle_seconds + 3600))
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
  local unsafe_candidates="$tmp_dir/${market}-manifest-unsafe.tsv"
  local index=0
  local uri manifest start_ns end_ns file digest zst_uri zst_path success_uri success_path
  local manifest_replay_safe
  local segment_dir manifest_path manifest_digest actual_manifest_digest
  local actual_digest bytes agg_trade_count manifest_agg_trade_count gap_ns digest_output
  local tape_schema='' candidate_schema stream_type_count
  local family_counts family_counts_path family_counts_filter_path
  local raw_trade_count book_ticker_count force_order_count
  local manifest_symbol_count manifest_raw_trade_count manifest_book_ticker_count
  local manifest_force_order_count
  local previous_end_ns=0
  local round_trips='[]'
  local -a strict_verifier_segments=()

  begin_io_psi_phase "oss-roundtrip-$market"
  start_io_psi_runtime_monitor

  run_active_io_psi_command : manifest_uris "$market" "$listing" >"$uris"
  : >"$candidates"
  : >"$unsafe_candidates"
  while IFS= read -r uri; do
    [[ -n $uri ]] || continue
    manifest="$tmp_dir/${market}-scan-$index.json"
    index=$((index + 1))
    run_active_io_psi_command : run_oss "$market" cp "$uri" "$manifest" \
      --force --no-progress >/dev/null
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
    ((end_ns <= market_observation_started_ns[$market])) && continue
    jq -e --arg session_id "${observed_session[$market]}" \
      --arg market "$market" \
      --argjson expected_stream_types "${expected_stream_types[$market]}" \
      '(.schema == "binance.market_tape.v1" or .schema == "binance.market_tape.v2")
        and (if $market == "usdm" then
          .schema == "binance.market_tape.v2"
          and (.stream_types | sort) == $expected_stream_types
          and ((.event_types.book_ticker // 0) == 0)
          and ((.event_types.agg_trade // 0) == 0)
          and ((.event_types.raw_trade // 0) == 0)
          and ((.event_types.force_order // 0) == 0)
          and (has("trade_summary_contract") | not)
          and (has("trade_summaries") | not)
        elif .schema == "binance.market_tape.v1" then
          (has("stream_types") | not)
          and (.event_types | has("raw_trade") | not)
          and (.event_types | has("book_ticker") | not)
          and (.event_types | has("force_order") | not)
        else
          (.stream_types | type) == "array"
          and (.stream_types | sort) == $expected_stream_types
          and (.event_types.raw_trade | type) == "number"
          and .event_types.raw_trade == (.event_types.raw_trade | floor)
          and .event_types.raw_trade > 0
          and (.event_types.book_ticker | type) == "number"
          and .event_types.book_ticker == (.event_types.book_ticker | floor)
          and .event_types.book_ticker > 0
          and (if $market == "usdm" then
            ((.event_types.force_order // 0) | type) == "number"
            and (.event_types.force_order // 0) == ((.event_types.force_order // 0) | floor)
            and (.event_types.force_order // 0) >= 0
          else
            (.event_types | has("force_order") | not)
          end)
        end)
        and (if $market == "usdm" then true
          else .trade_summary_contract == "binance.aggregate_trade_summary.v1"
            and (.trade_summaries | type) == "object"
            and (.trade_summaries | length) > 0
        end)
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
        and (if $market == "usdm" then true
          else (.event_types.agg_trade | type) == "number"
            and .event_types.agg_trade == (.event_types.agg_trade | floor)
            and .event_types.agg_trade > 0
        end)' \
      "$manifest" >/dev/null \
      || die "$market has an incomplete market-tape manifest after observation start: $uri"
    candidate_schema=$(jq -er '.schema' "$manifest")
    if [[ -z $tape_schema ]]; then
      tape_schema=$candidate_schema
    else
      [[ $candidate_schema == "$tape_schema" ]] \
        || die "$market mixes market-tape schema versions at $uri"
    fi
    jq -e '.has_replay_safe_checkpoint | type == "boolean"' "$manifest" >/dev/null \
      || die "$market manifest has no replay-safety decision: $uri"
    manifest_replay_safe=$(jq -r '.has_replay_safe_checkpoint' "$manifest")
    if [[ $manifest_replay_safe != true ]]; then
      printf '%s\t%s\t%s\n' "$start_ns" "$end_ns" "$uri" >>"$unsafe_candidates"
      continue
    fi
    file=$(jq -er '.file' "$manifest")
    digest=$(jq -er '.sha256' "$manifest")
    manifest_digest=$(sha256sum "$manifest" | awk '{print $1}')
    manifest_agg_trade_count=$(jq -er '.event_types.agg_trade // 0' "$manifest")
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$start_ns" "$end_ns" "$uri" "$file" "$digest" "$manifest_digest" "$manifest" \
      "$manifest_agg_trade_count" \
      >>"$candidates"
  done <"$uris"

  monday_validate_replay_safe_manifest_order "$market" "$candidates" "$unsafe_candidates" \
    || die "$market replay-safe manifest ordering check failed"

  candidate_count=$(wc -l <"$candidates" | tr -d ' ')
  ((candidate_count >= 2)) \
    || die "$market has fewer than two replay-safe complete OSS manifests after observation start"

  if [[ $tape_schema == binance.market_tape.v2 ]]; then
    stream_type_count=$(jq -er 'length' <<<"${expected_stream_types[$market]}")
  else
    stream_type_count=2
  fi

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
    run_active_io_psi_command : run_oss "$market" cp "$uri" "$manifest_path" \
      --force --no-progress >/dev/null
    actual_manifest_digest=$(sha256sum "$manifest_path" | awk '{print $1}')
    [[ $actual_manifest_digest == "$manifest_digest" ]] \
      || die "$market manifest changed between discovery and readback: $uri"
    run_active_io_psi_command : run_oss "$market" cp "$zst_uri" "$zst_path" \
      --force --no-progress >/dev/null
    digest_output="$segment_dir/data.sha256"
    run_active_io_psi_command : sha256sum "$zst_path" >"$digest_output"
    actual_digest=$(awk '{print $1}' "$digest_output")
    [[ $actual_digest == "$digest" ]] || die "$market OSS round-trip digest mismatch: $zst_uri"
    success_uri="${uri%/*}/${file}._SUCCESS"
    success_path="$segment_dir/${file}._SUCCESS"
    run_active_io_psi_command : run_oss "$market" cp "$success_uri" "$success_path" \
      --force --no-progress >/dev/null
    printf '%s\n' "$digest" | cmp -s - "$success_path" \
      || die "$market OSS success marker does not match segment SHA-256: $success_uri"
    manifest_symbol_count=$(jq -er '.symbols | length' "$manifest")
    family_counts_path="$segment_dir/family-counts.json"
    family_counts_filter_path="$segment_dir/family-counts.jq"
    # shellcheck disable=SC2016
    printf '%s\n' '
      def valid_agg_trade:
        (.received_at_ns | type) == "number"
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
      def valid_raw_trade:
        (.received_at_ns | type) == "number"
        and .received_at_ns >= 0
        and (.frame.data.e == "trade")
        and (.frame.data.s | type) == "string"
        and (.frame.data.s | length) > 0
        and (.frame.data.t | type) == "number"
        and .frame.data.t >= 0
        and (.frame.data.t | floor) == .frame.data.t
        and (.frame.data.p | type) == "string"
        and (.frame.data.p | length) > 0
        and (.frame.data.q | type) == "string"
        and (.frame.data.q | length) > 0
        and (.frame.data.E | type) == "number"
        and (.frame.data.T | type) == "number"
        and .frame.data.T <= .frame.data.E
        and (.frame.data.m | type) == "boolean";
      def valid_book_ticker:
        (.received_at_ns | type) == "number"
        and .received_at_ns >= 0
        and (if $market == "spot" then
          ((.frame.data | has("e")) | not)
          and ((.frame.data | has("E")) | not)
          and ((.frame.data | has("T")) | not)
        elif $market == "usdm" then
          (.frame.data.e == "bookTicker")
          and (.frame.data.E | type) == "number"
          and ((.frame.data.T == null)
            or ((.frame.data.T | type) == "number" and .frame.data.T <= .frame.data.E))
        else false end)
        and (.frame.data.s | type) == "string"
        and (.frame.data.s | length) > 0
        and (.frame.data.u | type) == "number"
        and .frame.data.u >= 0
        and (.frame.data.u | floor) == .frame.data.u
        and (.frame.data.b | type) == "string"
        and (.frame.data.b | length) > 0
        and (.frame.data.B | type) == "string"
        and (.frame.data.B | length) > 0
        and (.frame.data.a | type) == "string"
        and (.frame.data.a | length) > 0
        and (.frame.data.A | type) == "string"
        and (.frame.data.A | length) > 0;
      def valid_force_order:
        (.received_at_ns | type) == "number"
        and .received_at_ns >= 0
        and (.frame.data.e == "forceOrder")
        and (.frame.data.o | type) == "object"
        and (.frame.data.o.s | type) == "string"
        and (.frame.data.o.s | length) > 0
        and (.frame.data.o.S == "BUY" or .frame.data.o.S == "SELL")
        and (.frame.data.o.p | type) == "string"
        and (.frame.data.o.p | length) > 0
        and (.frame.data.o.q | type) == "string"
        and (.frame.data.o.q | length) > 0
        and (.frame.data.E | type) == "number"
        and (.frame.data.o.T | type) == "number"
        and .frame.data.o.T <= .frame.data.E;
      def valid_session_start:
        .websocket_streams == ($symbol_count * $stream_type_count)
        and (if $schema == "binance.market_tape.v2" then
          (.stream_types | sort) == $expected_stream_types
        else
          (has("stream_types") | not)
        end);
      reduce inputs as $row
        ({agg_trade:0,raw_trade:0,book_ticker:0,force_order:0,invalid:false};
          if $row.schema != $schema then
            .invalid = true
          elif $row.type == "agg_trade" then
            .agg_trade += 1 | .invalid = (.invalid or (($row | valid_agg_trade) | not))
          elif $row.type == "raw_trade" then
            .raw_trade += 1 | .invalid = (.invalid or (($row | valid_raw_trade) | not))
          elif $row.type == "book_ticker" then
            .book_ticker += 1 | .invalid = (.invalid or (($row | valid_book_ticker) | not))
          elif $row.type == "force_order" then
            .force_order += 1 | .invalid = (.invalid or (($row | valid_force_order) | not))
          elif $row.type == "session_start" then
            .invalid = (.invalid or (($row | valid_session_start) | not))
          else . end)
      | if .invalid then error("malformed market-tape row")
        elif $market == "usdm"
          and (.book_ticker > 0 or .agg_trade > 0 or .raw_trade > 0 or .force_order > 0)
          then error("USD-M LOB stream family contract")
        elif $market == "spot" and .agg_trade == 0 then error("missing agg_trade")
        elif $market == "spot" and $schema == "binance.market_tape.v2"
          and (.raw_trade == 0 or .book_ticker == 0)
          then error("missing v2 stream family")
        elif $market == "spot" and $schema == "binance.market_tape.v1"
          and (.raw_trade > 0 or .book_ticker > 0 or .force_order > 0)
          then error("v1 tape carries v2 stream families")
        else {agg_trade,raw_trade,book_ticker,force_order} end' \
      >"$family_counts_filter_path"
    # The external bash is the registered process-group leader; with job control
    # disabled inside it, both zstd and jq inherit that same group.
    # shellcheck disable=SC2016
    if ! run_active_io_psi_command : bash -o pipefail -c '
      zstd -q -d -c "$1" | jq -ec -n \
        --arg schema "$2" --arg market "$3" \
        --argjson symbol_count "$4" --argjson stream_type_count "$5" \
        --argjson expected_stream_types "$6" -f "$7"
    ' _ "$zst_path" "$tape_schema" "$market" "$manifest_symbol_count" \
      "$stream_type_count" "${expected_stream_types[$market]}" \
      "$family_counts_filter_path" >"$family_counts_path"; then
      die "$market segment has missing or malformed stream-family events: $zst_uri"
    fi
    family_counts=$(<"$family_counts_path")
    agg_trade_count=$(jq -er '.agg_trade' <<<"$family_counts")
    raw_trade_count=$(jq -er '.raw_trade' <<<"$family_counts")
    book_ticker_count=$(jq -er '.book_ticker' <<<"$family_counts")
    force_order_count=$(jq -er '.force_order' <<<"$family_counts")
    manifest_raw_trade_count=$(jq -er '.event_types.raw_trade // 0' "$manifest")
    manifest_book_ticker_count=$(jq -er '.event_types.book_ticker // 0' "$manifest")
    manifest_force_order_count=$(jq -er '.event_types.force_order // 0' "$manifest")
    [[ $agg_trade_count == "$manifest_agg_trade_count" ]] \
      || die "$market manifest aggregate-trade count does not match segment: $uri"
    [[ $raw_trade_count == "$manifest_raw_trade_count" ]] \
      || die "$market manifest raw-trade count does not match segment: $uri"
    [[ $book_ticker_count == "$manifest_book_ticker_count" ]] \
      || die "$market manifest book-ticker count does not match segment: $uri"
    [[ $force_order_count == "$manifest_force_order_count" ]] \
      || die "$market manifest force-order count does not match segment: $uri"
    strict_verifier_segments+=("$zst_path" "$digest" "$manifest_digest")
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
    if [[ $tape_schema == binance.market_tape.v2 ]]; then
      round_trip=$(jq -cn \
        --argjson value "$round_trip" \
        --argjson raw_trade_count "$raw_trade_count" \
        --argjson book_ticker_count "$book_ticker_count" \
        --argjson force_order_count "$force_order_count" \
        --arg market "$market" \
        '$value + {raw_trade_count:$raw_trade_count,book_ticker_count:$book_ticker_count}
          + (if $market == "usdm" then {force_order_count:$force_order_count} else {} end)')
    fi
    round_trips=$(jq -cn --argjson values "$round_trips" --argjson value "$round_trip" \
      '$values + [$value]')
  done < <(sort -n -k1,1 "$candidates")

  verify_adjacent_segments "${strict_verifier_segments[@]}"
  if [[ $market == spot ]]; then
    verify_aggregate_trade_continuity "${strict_verifier_segments[@]}"
    if [[ $tape_schema == binance.market_tape.v2 ]]; then
      verify_raw_trade_continuity "${strict_verifier_segments[@]}"
    fi
  fi

  jq -e --arg session_id "${observed_session[$market]}" '
    all(.[].lob_reconnect_boundary; . == false)
    and all(.[].lob_capture_session_id; . == $session_id)' \
    <<<"$round_trips" >/dev/null \
    || die "$market LOB evidence crosses a capture-session or observation boundary"

  finish_io_psi_runtime_monitor
  jq -cn --arg tape_schema "$tape_schema" --argjson round_trips "$round_trips" \
    '{tape_schema:$tape_schema,round_trips:$round_trips}'
}

duration_seconds=${observed_runtime_seconds[spot]}
if ((observed_runtime_seconds[usdm] < duration_seconds)); then
  duration_seconds=${observed_runtime_seconds[usdm]}
fi
observation_started_ns=${market_observation_started_ns[spot]}

markets_json='{}'
for market in "${markets[@]}"; do
  round_trips_path="$tmp_dir/${market}-round-trips.json"
  verify_oss_round_trips "$market" >"$round_trips_path"
  tape_schema=$(jq -er '.tape_schema' "$round_trips_path")
  round_trips=$(jq -c '.round_trips' "$round_trips_path")
  market_json=$(jq -cn \
    --arg market "$market" \
    --arg unit "${unit[$market]}" \
    --arg dataset "${dataset[$market]}" \
    --arg session_id "${observed_session[$market]}" \
    --arg tape_schema "$tape_schema" \
    --arg symbols_config "${configured_symbols[$market]}" \
    --arg catalog_sha256 "${frozen_catalog_sha256[$market]}" \
    --arg configured_catalog_sha256 "${configured_catalog_sha256[$market]}" \
    --arg health_sha256 "${health_sha256[$market]}" \
    --argjson observation_started_ns "${market_observation_started_ns[$market]}" \
    --argjson symbol_count "${symbol_count[$market]}" \
    --argjson snapshot_ready_count "${snapshot_ready_count[$market]}" \
    --argjson stream_coverage_verified_count "${stream_coverage_verified_count[$market]}" \
    --argjson sequence_gaps "${sequence_gaps[$market]}" \
    --argjson full_stream_coverage_verified "${full_stream_coverage_verified[$market]}" \
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
      observation_started_ns:$observation_started_ns,
      tape_schema:$tape_schema,
      symbols_config:$symbols_config,catalog_sha256:$catalog_sha256,
      configured_catalog_sha256:$configured_catalog_sha256,
      symbol_count:$symbol_count,snapshot_ready_count:$snapshot_ready_count,
      stream_coverage_verified_count:$stream_coverage_verified_count,
      all_stream_coverage_verified:($stream_coverage_verified_count == $symbol_count),
      full_stream_coverage_verified:$full_stream_coverage_verified,
      sequence_gaps:$sequence_gaps,upload_failure_count:$upload_failure_count,
      health_samples:$health_samples,
      max_health_silence_seconds:$max_health_silence_seconds,
      n_restarts:$n_restarts,
      observed_runtime_seconds:$observed_runtime_seconds,
      cpu_usage_ns:$cpu_usage_ns,cpu_quota_per_sec_us:$cpu_quota_per_sec_us,
      memory_peak_bytes:$memory_peak_bytes,memory_max_bytes:$memory_max_bytes,
      health_sha256:$health_sha256,
      strict_trade_summary_readback:($market == "spot"),
      strict_lob_continuity_readback:true,
      lob_reconnect_boundaries:([$oss_round_trips[].lob_reconnect_boundary] | map(select(.)) | length),
      min_lob_source_latency_ms:([$oss_round_trips[].lob_min_source_latency_ms] | min),
      max_lob_source_latency_ms:([$oss_round_trips[].lob_max_source_latency_ms] | max),
      min_lob_bid_levels:([$oss_round_trips[].lob_min_bid_levels] | min),
      min_lob_ask_levels:([$oss_round_trips[].lob_min_ask_levels] | min),
      max_segment_gap_ns:([$oss_round_trips[].gap_from_previous_ns] | max),
      oss_roundtrips:($oss_round_trips | length),
      agg_trade_segments:(if $market == "spot" then ($oss_round_trips | length) else 0 end),
      agg_trade_count:([$oss_round_trips[].agg_trade_count] | add),
      oss_roundtrip_evidence:$oss_round_trips}')
  if [[ $tape_schema == binance.market_tape.v2 ]]; then
    market_json=$(jq -cn \
      --argjson value "$market_json" \
      --arg market "$market" \
      --argjson stream_types "${expected_stream_types[$market]}" \
      '$value + {
        stream_types:$stream_types,
        raw_trade_segments:([$value.oss_roundtrip_evidence[].raw_trade_count
          | select(. > 0)] | length),
        raw_trade_count:([$value.oss_roundtrip_evidence[].raw_trade_count] | add),
        book_ticker_count:([$value.oss_roundtrip_evidence[].book_ticker_count] | add),
        strict_raw_trade_continuity_readback:($market == "spot")}
        + (if $market == "usdm" then
          {force_order_count:([$value.oss_roundtrip_evidence[].force_order_count] | add)}
        else {} end)')
  fi
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
write_run_json

assert_pair_identity
gate_schema=monday.rust_lob_shadow_gate.v4
[[ $pair_mode == true ]] && gate_schema=monday.rust_lob_shadow_gate.v5
jq -n \
  --arg schema "$gate_schema" \
  --arg candidate_sha256 "$candidate_sha" \
  --arg candidate_binary "$candidate_binary" \
  --arg runtime_contract_sha256 "$runtime_contract_sha256" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
  --arg deployment_source_revision "$deployment_source_revision" \
  --arg controller_release_sha256 "$controller_release_sha256" \
  --arg controller_deployment_bundle_sha256 "$controller_deployment_bundle_sha256" \
  --arg controller_deployment_source_revision "$controller_deployment_source_revision" \
  --arg run_id "$gate_run_id" \
  --arg run_spool "$run_spool_path" \
  --arg started_at "$gate_started_at" \
  --arg finished_at "$gate_finished_at" \
  --argjson observation_started_ns "$observation_started_ns" \
  --argjson required_duration_seconds "$REQUIRED_DURATION_SECONDS" \
  --argjson requested_duration_seconds "$gate_seconds" \
  --argjson health_settle_seconds "$health_settle_seconds" \
  --argjson host_memory_total_bytes "$host_memory_total_bytes" \
  --argjson host_swap_total_bytes "$host_swap_total_bytes" \
  --argjson production_memory_current_bytes "$production_memory_current_json" \
  --argjson maximum_sequential_phase_memory_bytes \
    "$maximum_sequential_phase_memory_bytes" \
  --argjson resource_preflight "$resource_preflight_json" \
  --argjson resource_admission_samples "$resource_admission_samples_json" \
  --argjson io_full_psi_windows "$io_psi_windows_json" \
  --argjson segment_seconds "$GATE_SEGMENT_SECONDS" \
  --argjson duration_seconds "$duration_seconds" \
  --argjson test_only "$test_only" \
  --argjson checks_passed true \
  --argjson production_eligible "$production_eligible" \
  --argjson passed "$passed" \
  --argjson markets "$markets_json" \
  '{schema:$schema,candidate_sha256:$candidate_sha256,candidate_binary:$candidate_binary,
    runtime_contract_sha256:$runtime_contract_sha256,
    deployment_bundle_sha256:$deployment_bundle_sha256,
    deployment_source_revision:$deployment_source_revision,
    run_id:$run_id,run_spool:$run_spool,
    started_at:$started_at,finished_at:$finished_at,
    observation_started_ns:$observation_started_ns,
    required_duration_seconds:$required_duration_seconds,
    requested_duration_seconds:$requested_duration_seconds,
    health_settle_seconds:$health_settle_seconds,
    host_memory_total_bytes:$host_memory_total_bytes,
    host_swap_total_bytes:$host_swap_total_bytes,
    production_memory_current_bytes:$production_memory_current_bytes,
    maximum_sequential_phase_memory_bytes:$maximum_sequential_phase_memory_bytes,
    resource_preflight:$resource_preflight,
    resource_admission_samples:$resource_admission_samples,
    io_full_psi_windows:$io_full_psi_windows,
    segment_seconds:$segment_seconds,
    duration_seconds:$duration_seconds,
    test_only:$test_only,checks_passed:$checks_passed,
    production_eligible:$production_eligible,passed:$passed,markets:$markets}
    + (if $controller_release_sha256 == "" then {} else {
      controller_release_sha256:$controller_release_sha256,
      controller_deployment_bundle_sha256:$controller_deployment_bundle_sha256,
      controller_deployment_source_revision:$controller_deployment_source_revision
    } end)' \
  >"$gate_tmp"
[[ ! -e $gate_json && ! -L $gate_json ]] || die 'gate evidence path already exists'
install -m 0640 "$gate_tmp" "$gate_json"
rm -f "$gate_tmp"

if [[ $production_eligible == true ]]; then
  jq -e \
    --arg candidate_sha256 "$candidate_sha" \
    --arg runtime_contract_sha256 "$runtime_contract_sha256" \
    --arg deployment_bundle_sha256 "$deployment_bundle_sha256" \
    --arg deployment_source_revision "$deployment_source_revision" \
    --arg controller_release_sha256 "$controller_release_sha256" \
    -f "$controller_policy" "$gate_json" >/dev/null \
    || die 'Gate evidence failed the controller-bound policy'
  assert_pair_identity
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
