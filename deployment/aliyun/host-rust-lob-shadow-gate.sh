#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

# V2 shadow Gate. Candidate controller C1 owns this script and its policy.
readonly REQUIRED_DURATION_SECONDS=240
readonly HEALTH_SETTLE_SECONDS=240
readonly GATE_SEGMENT_SECONDS=120
readonly MAX_HEALTH_SILENCE_SECONDS=120
readonly MAX_SEGMENT_GAP_NS=90000000000
readonly HOST_MEMORY_RESERVE_BYTES=1073741824
readonly PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES=268435456
readonly STRICT_VERIFIER_MEMORY_MAX_BYTES=1610612736
readonly IO_PSI_WINDOW_SECONDS=15
readonly IO_PSI_WINDOW_US=15000000
readonly IO_PSI_FULL_DELTA_LIMIT_US=150000
readonly IO_PSI_CONSECUTIVE_HIT_LIMIT=3
readonly UPLOAD_DRAIN_MEMORY_MAX_BYTES=536870912
readonly SERVICE_USER=hftcollector
readonly SERVICE_HOME=/var/lib/hft-collector
readonly SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
readonly -a SHADOW_ASSETS=(
  binance-lob-archiver-rust@.service
  binance-lob-archiver-rust-upload@.service
  binance-lob-archiver-rust-spot.env
  binance-lob-archiver-rust-usdm.env
)
readonly -a PRODUCTION_ASSETS=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
)

die() { printf 'shadow gate failed: %s\n' "$*"; exit 1; }
usage() {
  cat >&2 <<'EOF'
Usage: host-rust-lob-shadow-gate.sh --from-controller <direct|sha256> \
  --candidate-controller <sha256> [--root <fixture-root>]
EOF
}

ROOT=${MONDAY_ROOT:-/}; FROM_CONTROLLER=; CANDIDATE_CONTROLLER=
while (($#)); do
  case "$1" in
    --from-controller) (($# >= 2)) || { usage; exit 2; }; FROM_CONTROLLER=$2; shift 2 ;;
    --candidate-controller) (($# >= 2)) || { usage; exit 2; }; CANDIDATE_CONTROLLER=$2; shift 2 ;;
    --root) (($# >= 2)) || { usage; exit 2; }; ROOT=$2; shift 2 ;;
    --help|-h) usage >&1; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
ROOT=${ROOT%/}; [[ -n $ROOT ]] || ROOT=/
FROM_CONTROLLER=$(printf '%s' "$FROM_CONTROLLER" | tr '[:upper:]' '[:lower:]')
CANDIDATE_CONTROLLER=$(printf '%s' "$CANDIDATE_CONTROLLER" | tr '[:upper:]' '[:lower:]')
[[ $FROM_CONTROLLER == direct || $FROM_CONTROLLER =~ ^[a-f0-9]{64}$ ]] ||
  die 'before controller must be direct or a 64-character SHA-256'
[[ $CANDIDATE_CONTROLLER =~ ^[a-f0-9]{64}$ ]] ||
  die 'candidate controller must be a 64-character SHA-256'
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true

GATE_DURATION_SECONDS=$REQUIRED_DURATION_SECONDS
HEALTH_SETTLE_DURATION_SECONDS=$HEALTH_SETTLE_SECONDS
resolve_test_duration() {
  local name value current formal
  for name in MONDAY_GATE_TEST_SECONDS MONDAY_TEST_HEALTH_SETTLE_SECONDS; do
    value=${!name:-}
    [[ -n $value ]] || continue
    [[ $TEST_ONLY == true && ${MONDAY_ALLOW_SHORT_GATE_FOR_TESTS:-0} == 1 ]] \
      || die "$name is only allowed for an explicitly authorised fixture Gate"
    [[ $value =~ ^[1-9][0-9]*$ ]] || die "$name must be a positive integer"
    if [[ $name == MONDAY_GATE_TEST_SECONDS ]]; then current=$value; formal=$REQUIRED_DURATION_SECONDS
    else current=$value; formal=$HEALTH_SETTLE_SECONDS; fi
    (( current < formal )) || die "$name must be shorter than the formal Gate contract"
    if [[ $name == MONDAY_GATE_TEST_SECONDS ]]; then GATE_DURATION_SECONDS=$current
    else HEALTH_SETTLE_DURATION_SECONDS=$current; fi
  done
}
resolve_test_duration

OPT_ROOT="$ROOT/opt/monday"; RELEASE_ROOT="$OPT_ROOT/releases/binance-lob-archiver"
CONTROLLER_ROOT="$OPT_ROOT/releases/binance-lob-controller"; BIN_ROOT="$OPT_ROOT/bin"
SYSTEMD_ROOT="$ROOT/etc/systemd/system"; CONFIG_ROOT="$ROOT/etc/monday"
LOCK_FILE="$ROOT/run/lock/monday-rust-lob-control-plane.lock"
OVERRIDE_ROOT="$ROOT/run/monday"
DATA_ROOT="$ROOT/data/monday"; EVIDENCE_ROOT=${MONDAY_GATE_EVIDENCE_ROOT:-$DATA_ROOT/evidence/shadow-gates}
RUN_SPOOL_ROOT="$DATA_ROOT/spool/binance-lob-rust-shadow/runs"; PROC_ROOT="$ROOT/proc"
PSI_SOURCE="$PROC_ROOT/pressure/io"; SHADOW_BINARY="$BIN_ROOT/binance-lob-archiver-shadow"
PRODUCTION_BINARY="$BIN_ROOT/binance-lob-archiver"
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
LIB_SOURCE="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"; POLICY_SOURCE="$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq"
[[ -f $LIB_SOURCE && -f $POLICY_SOURCE ]] || die 'V2 control-plane assets are missing'
# shellcheck disable=SC1090
. "$LIB_SOURCE"

for command in awk bash chmod cmp cp date dirname find grep install jq mkdir mktemp mv readlink rm sed sha256sum sleep sort stat tr wc zstd; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done
if [[ $TEST_ONLY != true ]]; then
  for command in aliyun flock id mountpoint runuser systemctl systemd-run; do
    command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
  done
  mountpoint -q "$ROOT/data" || die 'data filesystem must be a mount point'
  [[ -r "$PROC_ROOT/uptime" && -r $PSI_SOURCE ]] || die 'proc timing/PSI sources are unavailable'
  id "$SERVICE_USER" >/dev/null 2>&1 || die "missing service user: $SERVICE_USER"
fi

# The offline fixture supplies a tiny systemd double.  Production always uses
# the real systemctl binary; the double only models the state fields consumed
# by this action and cannot mutate a host unit.
if [[ $TEST_ONLY == true ]]; then
  declare -A fixture_unit_state=()
  systemctl() {
    local action=${1:-} unit_name=${2:-} property value
    case "$action" in
      start) fixture_unit_state[$unit_name]=active; return 0 ;;
      stop) fixture_unit_state[$unit_name]=inactive; return 0 ;;
      reset-failed|daemon-reload) return 0 ;;
      is-active)
        if [[ $2 == --quiet ]]; then unit_name=$3; else unit_name=$2; fi
        [[ ${fixture_unit_state[$unit_name]:-inactive} == active ]] && { [[ $2 == --quiet ]] || printf 'active\n'; return 0; }
        [[ $2 == --quiet ]] && return 3; printf 'inactive\n'; return 3 ;;
      show)
        unit_name=$2; property=${3#--property=}; property=${property#--property};
        if [[ $property == *=* ]]; then property=${property#*=}; fi
        case "$property" in
          ActiveState) value=${fixture_unit_state[$unit_name]:-inactive} ;;
          SubState) [[ ${fixture_unit_state[$unit_name]:-inactive} == active ]] && value=running || value=dead ;;
          NRestarts) [[ ${MONDAY_GATE_FIXTURE_FAIL_RESTART:-0} == 1 ]] && value=1 || value=0 ;;
          MainPID) value=$$ ;;
          MemoryCurrent) value=1048576 ;;
          MemoryPeak) value=1048576 ;;
          MemoryMax) value=2147483648 ;;
          MemoryHigh) value=1879048192 ;;
          CPUUsageNSec) value=1000000 ;;
          CPUQuotaPerSecUSec) value=800ms ;;
          DropInPaths) value= ;;
          OOMScoreAdjust) value=500 ;;
          *) value= ;;
        esac
        printf '%s\n' "$value"; return 0 ;;
      *) return 0 ;;
    esac
  }
  aliyun() {
    local tool=${1:-} action=${2:-} source target object
    [[ $tool == ossutil ]] || return 2
    shift 2
    case "$action" in
      ls)
        for object in "${spool_dir[$OSS_FIXTURE_MARKET]}"/*.manifest.json; do
          [[ -f $object ]] || continue
          printf 'oss://fixture/%s/%s\n' "$OSS_FIXTURE_MARKET" "${object##*/}"
        done
        ;;
      cp)
        source=${1:-}; target=${2:-}
        object=${source##*/}
        [[ $object != "$source" && -n $target ]] || return 2
        cp -p -- "${spool_dir[$OSS_FIXTURE_MARKET]}/$object" "$target"
        if [[ ${MONDAY_GATE_FIXTURE_TAMPER_OSS:-0} == 1 && $object == *.jsonl.zst ]]; then
          printf '\n' >>"$target"
        fi
        ;;
      *) return 2 ;;
    esac
  }
fi

direct_directory() { local path=$1; [[ -d $path && ! -L $path && $(readlink -f -- "$path") == "$path" ]]; }
direct_directory_or_absent() { local path=$1; [[ ! -e $path && ! -L $path ]] || direct_directory "$path"; }
regular_file() { [[ -f $1 && ! -L $1 ]]; }
secure_file() {
  local path=$1 mode owner; regular_file "$path" || die "required regular file is missing: $path"
  if [[ $TEST_ONLY != true ]]; then owner=$(stat -c %u -- "$path"); mode=$(stat -c %a -- "$path")
    [[ $owner == 0 ]] || die "required file is not root-owned: $path"
    (( (8#$mode & 022) == 0 )) || die "required file is writable by group/world: $path"; fi
}
sha256_file() { monday_sha256_file "$1"; }
meminfo_bytes() {
  local field=$1 source="$PROC_ROOT/meminfo" value
  if [[ ! -f $source && $TEST_ONLY == true ]]; then case "$field" in
    MemTotal) printf '8589934592\n';; MemAvailable) printf '6442450944\n';; SwapTotal) printf '0\n';; esac; return; fi
  value=$(awk -v key="$field:" '$1 == key { count++; value=$2 } END { if (count != 1 || value !~ /^[0-9]+$/) exit 1; print value }' "$source") || return 1
  printf '%s\n' "$((value * 1024))"
}
monotonic_seconds() { if [[ $TEST_ONLY == true && ! -r "$PROC_ROOT/uptime" ]]; then printf '%s\n' "$(date +%s)"; else awk '{print int($1)}' "$PROC_ROOT/uptime"; fi; }
io_total_us() { if [[ $TEST_ONLY == true && ! -f $PSI_SOURCE ]]; then printf '0\n'; else monday_io_full_psi_total_us "$PSI_SOURCE"; fi; }
systemctl_show() { systemctl show "$1" --property="$2" --value 2>/dev/null; }
systemctl_active() { systemctl is-active --quiet "$1"; }
env_value() {
  local file=$1 key=$2 count value; count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || die "$file must contain exactly one ${key}= entry"
  value=$(sed -n "s/^${key}=//p" "$file"); [[ -n $value ]] || die "$file has an empty $key"; printf '%s\n' "$value"
}
run_spool_dir() {
  local candidate=$1 run_id=$2 market=$3; [[ $candidate =~ ^[a-f0-9]{64}$ && $run_id =~ ^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$ ]]
  [[ $market == spot || $market == usdm ]]; printf '%s/%s/%s/%s\n' "$RUN_SPOOL_ROOT" "$candidate" "$run_id" "$market"
}
is_usdm_top100() {
  local value=$1 unique; [[ $value =~ ^[A-Z0-9]+(,[A-Z0-9]+)*$ ]] || return 1
  local -a values; IFS=, read -r -a values <<<"$value"; (( ${#values[@]} == 100 )) || return 1
  unique=$(printf '%s\n' "${values[@]}" | sort -u | wc -l); ((unique == 100))
}

candidate_release="$CONTROLLER_ROOT/$CANDIDATE_CONTROLLER"; candidate_deployment="$candidate_release/deployment"
candidate_manifest="$candidate_release/release.json"
monday_verify_controller_release "$ROOT" "$CANDIDATE_CONTROLLER" || die 'candidate controller release is not an exact immutable V2 release'
[[ $TEST_ONLY == true || $(readlink -f -- "${BASH_SOURCE[0]}") == "$candidate_deployment/host-rust-lob-shadow-gate.sh" ]] || die 'Gate must execute from candidate controller bytes'
candidate_payload=$(monday_manifest_field "$candidate_manifest" artifact_sha256)
candidate_runtime=$(monday_manifest_field "$candidate_manifest" runtime_contract_sha256)
candidate_bundle=$(monday_manifest_field "$candidate_manifest" deployment_bundle_sha256)
candidate_source=$(monday_manifest_field "$candidate_manifest" deployment_source_revision)
candidate_payload_dir="$RELEASE_ROOT/$candidate_payload"; candidate_binary="$candidate_payload_dir/binance-lob-archiver"
secure_file "$candidate_binary"; [[ -x $candidate_binary && $(sha256_file "$candidate_binary") == "$candidate_payload" ]] || die 'candidate payload identity failed'

active_before=direct; before_payload=; before_runtime=; before_bundle=; before_source=; before_deployment=
if [[ $FROM_CONTROLLER != direct ]]; then
  active_before=$(monday_active_controller_sha "$ROOT") || die 'requested before controller is not active'
  [[ $active_before == "$FROM_CONTROLLER" ]] || die 'active pair differs from requested before controller'
  before_release="$CONTROLLER_ROOT/$FROM_CONTROLLER"; before_deployment="$before_release/deployment"
  monday_verify_controller_release "$ROOT" "$FROM_CONTROLLER" || die 'before controller release is invalid'
  before_payload=$(monday_manifest_field "$before_release/release.json" artifact_sha256)
  before_runtime=$(monday_manifest_field "$before_release/release.json" runtime_contract_sha256)
  before_bundle=$(monday_manifest_field "$before_release/release.json" deployment_bundle_sha256)
  before_source=$(monday_manifest_field "$before_release/release.json" deployment_source_revision)
  [[ $(readlink -f -- "$PRODUCTION_BINARY") == "$RELEASE_ROOT/$before_payload/binance-lob-archiver" ]] || die 'production binary is not bound to before pair'
else
  [[ ! -L $CONTROLLER_ROOT/active ]] || die 'direct bootstrap requires no active V2 controller'
  before_deployment=$candidate_deployment
  production_target=$(readlink -f -- "$PRODUCTION_BINARY") || die 'direct bootstrap requires a production binary'
  before_payload=$(sha256_file "$production_target") || die 'direct bootstrap requires a production binary'
  [[ $before_payload == "$candidate_payload" ]] || die 'direct bootstrap requires P0 equal to P1'
  before_runtime=$candidate_runtime; before_bundle=$candidate_bundle; before_source=$candidate_source
  if [[ $TEST_ONLY != true ]]; then [[ -L $PRODUCTION_BINARY && $(readlink -f -- "$PRODUCTION_BINARY") == "$candidate_binary" ]] || die 'direct production identity differs'; fi
fi

production_asset_json='{}'
for asset in "${PRODUCTION_ASSETS[@]}"; do
  if [[ $asset == *.service ]]; then production_target="$SYSTEMD_ROOT/$asset"; else production_target="$CONFIG_ROOT/$asset"; fi
  regular_file "$production_target" || die "installed production asset is missing: $production_target"
  cmp -s "$before_deployment/$asset" "$production_target" || die "installed production asset differs from before controller: $asset"
  production_asset_json=$(jq -cn --argjson values "$production_asset_json" --arg asset "$asset" --arg sha "$(sha256_file "$production_target")" '$values + {($asset):$sha}')
done

declare -A installed_asset asset_kind saved_state saved_sha
declare -A candidate_asset_sha restored_asset_sha
tmp_dir=$(mktemp -d); restore_dir="$tmp_dir/restore"; mkdir -p "$restore_dir/systemd" "$restore_dir/monday"
for asset in "${SHADOW_ASSETS[@]}"; do
  if [[ $asset == *.service ]]; then installed_asset[$asset]="$SYSTEMD_ROOT/$asset"; asset_kind[$asset]=systemd
  else installed_asset[$asset]="$CONFIG_ROOT/$asset"; asset_kind[$asset]=monday; fi
  if regular_file "${installed_asset[$asset]}"; then
    saved_state[$asset]=present; cp -p -- "${installed_asset[$asset]}" "$restore_dir/${asset_kind[$asset]}/$asset"; saved_sha[$asset]=$(sha256_file "${installed_asset[$asset]}")
  elif [[ -L ${installed_asset[$asset]} ]]; then
    die "shadow asset path is a symlink: ${installed_asset[$asset]}"
  else saved_state[$asset]=absent; saved_sha[$asset]=; fi
done
old_shadow_target=; old_shadow_present=false
if [[ -L $SHADOW_BINARY ]]; then old_shadow_target=$(readlink -- "$SHADOW_BINARY"); old_shadow_present=true
elif [[ -e $SHADOW_BINARY ]]; then die 'shadow binary path is not a symlink'; fi

install_shadow_assets() {
  local deployment=$1 asset target
  for asset in "${SHADOW_ASSETS[@]}"; do target="${installed_asset[$asset]}"; [[ ! -L $target ]] || die "shadow asset path became a symlink: $target"; install -d -m 0755 "$(dirname -- "$target")"
    install -m 0640 "$deployment/$asset" "$target"; cmp -s "$deployment/$asset" "$target" || die "staged shadow asset differs: $asset"
    candidate_asset_sha[$asset]=$(sha256_file "$target")
  done
  [[ $TEST_ONLY == true ]] || systemctl daemon-reload
}
restore_shadow_assets() {
  local asset target
  for asset in "${SHADOW_ASSETS[@]}"; do target="${installed_asset[$asset]}"
    [[ ! -L $target ]] || return 1
    if [[ ${saved_state[$asset]} == present ]]; then install -m 0640 "$restore_dir/${asset_kind[$asset]}/$asset" "$target"; [[ $(sha256_file "$target") == "${saved_sha[$asset]}" ]] || return 1
      restored_asset_sha[$asset]=$(sha256_file "$target")
    else rm -f -- "$target"; [[ ! -e $target && ! -L $target ]] || return 1; fi
  done
  [[ $TEST_ONLY == true ]] || systemctl daemon-reload
}
restore_shadow_link() { if [[ $old_shadow_present == true ]]; then monday_atomic_symlink "$old_shadow_target" "$SHADOW_BINARY"; else rm -f -- "$SHADOW_BINARY"; fi; }

mkdir -p "$(dirname -- "$LOCK_FILE")"
if [[ $TEST_ONLY == true ]]; then
  true
else
  exec 9>"$LOCK_FILE"; flock -n 9 || die 'another collector control-plane action is running'
fi
declare -A market_env spool_dir dataset symbols unit override_file
declare -A oss_bucket oss_endpoint oss_region aliyun_profile
markets=(spot usdm)
for market in "${markets[@]}"; do
  market_env[$market]="$candidate_deployment/binance-lob-archiver-rust-${market}.env"
  dataset[$market]=$(env_value "${market_env[$market]}" DATASET); symbols[$market]=$(env_value "${market_env[$market]}" SYMBOLS)
  oss_bucket[$market]=$(env_value "${market_env[$market]}" OSS_BUCKET)
  oss_endpoint[$market]=$(env_value "${market_env[$market]}" OSS_ENDPOINT)
  oss_region[$market]=$(env_value "${market_env[$market]}" OSS_REGION)
  aliyun_profile[$market]=$(env_value "${market_env[$market]}" ALIYUN_PROFILE)
  [[ $(env_value "${market_env[$market]}" MARKET) == "$market" ]] || die "$market env has wrong market"
  if [[ $TEST_ONLY == true ]]; then spool_dir[$market]="$DATA_ROOT/spool/binance-lob-rust-shadow/$market"; else spool_dir[$market]=$(env_value "${market_env[$market]}" SPOOL_DIR); fi
  unit[$market]="binance-lob-archiver-rust@${market}.service"
  override_file[$market]="$OVERRIDE_ROOT/binance-lob-archiver-rust-${market}-soak.env"
done
if [[ $TEST_ONLY != true ]]; then
  [[ ${symbols[spot]} == ALL && ${dataset[spot]} == spot_all_rust_shadow ]] || die 'Spot identity is invalid'
  is_usdm_top100 "${symbols[usdm]}" || die 'USD-M catalog is not frozen'
  [[ ${dataset[usdm]} == usdm_perpetual_top100_lob_rust_shadow ]] || die 'USD-M dataset identity is invalid'
  for market in "${markets[@]}"; do
    [[ ${oss_region[$market]} == ap-northeast-1 ]] || die "$market OSS region is not Tokyo"
    [[ -n ${oss_bucket[$market]} && -n ${oss_endpoint[$market]} && -n ${aliyun_profile[$market]} ]] || die "$market OSS identity is incomplete"
  done
fi

host_memory_total=$(meminfo_bytes MemTotal) || die 'MemTotal is unavailable'; host_memory_available=$(meminfo_bytes MemAvailable) || die 'MemAvailable is unavailable'; host_swap_total=$(meminfo_bytes SwapTotal) || die 'SwapTotal is unavailable'
declare -A production_growth; production_memory_json='{}'
declare -A production_pid production_exe_sha
production_process_json='{}'
if [[ $TEST_ONLY != true ]]; then
  declare -A production_state production_current production_peak production_max
  for market in "${markets[@]}"; do
    production_unit="binance-lob-archiver-production@${market}.service"; production_state[$market]=$(systemctl_show "$production_unit" ActiveState)
    case "${production_state[$market]}" in
      active) [[ $(systemctl_show "$production_unit" SubState) == running ]] || die "$market production is not running"
        production_current[$market]=$(systemctl_show "$production_unit" MemoryCurrent); production_peak[$market]=$(systemctl_show "$production_unit" MemoryPeak); production_max[$market]=$(systemctl_show "$production_unit" MemoryMax)
        production_growth[$market]=$(monday_production_memory_growth_headroom "${production_current[$market]}" "${production_peak[$market]}" "${production_max[$market]}" "$PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES") || die "$market production memory accounting is invalid" ;;
      inactive) production_growth[$market]=0 ;;
      *) die "$market production state is ambiguous" ;;
    esac
    [[ ${production_state[$market]} == active ]] || die "$market production is not active for pair Gate"
    production_pid[$market]=$(systemctl_show "$production_unit" MainPID)
    [[ ${production_pid[$market]} =~ ^[1-9][0-9]*$ ]] || die "$market production MainPID is unavailable"
    production_exe_sha[$market]=$(sha256_file "$(readlink -f -- "$PROC_ROOT/${production_pid[$market]}/exe")") || die "$market production executable is unavailable"
    [[ ${production_exe_sha[$market]} == "$before_payload" ]] || die "$market production executable differs from before pair"
    production_process_json=$(jq -cn --argjson values "$production_process_json" --arg market "$market" --argjson pid "${production_pid[$market]}" --arg exe "${production_exe_sha[$market]}" '$values + {($market):{main_pid:$pid,process_exe_sha256:$exe,active:true}}')
  done
  production_memory_json=$(jq -cn --arg spot "${production_state[spot]}" --arg usdm "${production_state[usdm]}" '{spot:{active_state:$spot},usdm:{active_state:$usdm}}')
else production_growth[spot]=0; production_growth[usdm]=0; production_process_json='{}'; fi

resource_samples='[]'; psi_windows='[]'
record_resource() {
  local phase=$1 phase_max=$2 required sample
  required=$(monday_shadow_memory_admission "$host_memory_available" "$HOST_MEMORY_RESERVE_BYTES" "$phase_max" "${production_growth[spot]}" "${production_growth[usdm]}") || die "insufficient memory for $phase"
  sample=$(jq -cn --arg phase "$phase" --argjson available "$host_memory_available" --argjson required "$required" --argjson phase_max "$phase_max" '{phase:$phase,host_memory_available_bytes:$available,required_bytes:$required,phase_memory_max_bytes:$phase_max}')
  resource_samples=$(jq -cn --argjson values "$resource_samples" --argjson value "$sample" '$values + [$value]')
}
calibrate_psi() {
  local phase=$1 previous current transition delta ratio hit consecutive=0 i
  if [[ $TEST_ONLY == true ]]; then psi_windows=$(jq -cn --argjson values "$psi_windows" --arg phase "$phase" '$values + [{phase:$phase,stage:"fixture",hit:false,consecutive_hits:0}]'); return; fi
  previous=$(io_total_us) || die "I/O PSI unavailable before $phase"
  for i in 1 2 3; do sleep "$IO_PSI_WINDOW_SECONDS"; current=$(io_total_us) || die "I/O PSI unavailable during $phase"
    transition=$(monday_io_full_psi_window "$previous" "$current" "$IO_PSI_WINDOW_US" "$IO_PSI_WINDOW_US" "$IO_PSI_FULL_DELTA_LIMIT_US" "$consecutive") || die 'I/O PSI moved backwards'
    read -r delta ratio hit consecutive <<<"$transition"; [[ $hit == false || $consecutive -lt $IO_PSI_CONSECUTIVE_HIT_LIMIT ]] || die 'I/O PSI threshold exceeded'
    psi_windows=$(jq -cn --argjson values "$psi_windows" --arg phase "$phase" --argjson delta "$delta" --argjson ratio "$ratio" --argjson hit "$hit" --argjson consecutive "$consecutive" '$values + [{phase:$phase,stage:"calibration",delta_us:$delta,ratio:$ratio,hit:$hit,consecutive_hits:$consecutive}]'); previous=$current
  done
}

run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
evidence_dir="$EVIDENCE_ROOT/$CANDIDATE_CONTROLLER/$candidate_runtime/runs/$run_id"; gate_json="$evidence_dir/gate.json"; passed_marker="$evidence_dir/PASSED.sha256"; run_spool="$RUN_SPOOL_ROOT/$CANDIDATE_CONTROLLER/$run_id"; run_json="$evidence_dir/run.json"
if [[ $TEST_ONLY == true ]]; then
  for market in "${markets[@]}"; do spool_dir[$market]="$run_spool/$market"; done
fi
install -d -m 0750 "$EVIDENCE_ROOT" "$RUN_SPOOL_ROOT" "$evidence_dir" "$run_spool"; for market in "${markets[@]}"; do install -d -m 0750 "${spool_dir[$market]}"; done
while IFS= read -r prior_receipt; do
  if jq -e '.schema == "monday.rust_lob_shadow_gate.v5" and .passed == true' "$prior_receipt" >/dev/null 2>&1; then
    die 'a passed Gate receipt already exists for this controller identity'
  fi
done < <(find "$EVIDENCE_ROOT/$CANDIDATE_CONTROLLER/$candidate_runtime" -type f -name gate.json -print)
gate_started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ); gate_finished=false; staging_started=false
declare -A phase_pid phase_exe_sha phase_session phase_segments phase_oss phase_runtime
declare -A phase_strict_lob phase_strict_aggregate phase_strict_raw
declare -A market_gate_started_ns market_observation_started_ns frozen_symbol_count frozen_catalog_sha256
declare -A initial_upload_failure_count last_health_updated_ns last_health_advance_mono
declare -A max_health_silence_seconds health_samples
write_run_json() {
  jq -cn --arg run "$run_id" --arg controller "$CANDIDATE_CONTROLLER" --arg payload "$candidate_payload" --arg runtime "$candidate_runtime" --arg spool "$run_spool" --argjson requested "$GATE_DURATION_SECONDS" --argjson settle "$HEALTH_SETTLE_DURATION_SECONDS" --argjson resources "$resource_samples" --argjson psi "$psi_windows" \
    '{schema:"monday.rust_lob_shadow_gate_run.v3",control_plane_version:2,run_id:$run,candidate_controller_sha256:$controller,candidate_payload_sha256:$payload,candidate_runtime_contract_sha256:$runtime,run_spool:$spool,segment_seconds:120,requested_duration_seconds:$requested,health_settle_seconds:$settle,resource_admission:$resources,io_full_psi_windows:$psi}' >"$run_json.tmp"
  chmod 0640 "$run_json.tmp"; mv -f -- "$run_json.tmp" "$run_json"
}
cleanup() {
  local status=$? restore_failed=false; set +e
  for market in "${markets[@]}"; do systemctl stop "${unit[$market]}" >/dev/null 2>&1 || true; done
  for market in "${markets[@]}"; do rm -f -- "${override_file[$market]}"; done
  if [[ $staging_started == true ]]; then restore_shadow_assets || restore_failed=true; fi
  restore_shadow_link || restore_failed=true; [[ $TEST_ONLY == true ]] || systemctl daemon-reload >/dev/null 2>&1 || restore_failed=true
  [[ $gate_finished == true ]] || rm -f -- "$passed_marker" "$evidence_dir/.PASSED.sha256.tmp"; rm -rf -- "$tmp_dir"
  [[ $restore_failed == false ]] || { printf 'shadow staging cleanup was incomplete\n' >&2; status=1; }; exit "$status"
}
trap cleanup EXIT; trap 'exit 143' HUP INT TERM

record_resource preflight "$STRICT_VERIFIER_MEMORY_MAX_BYTES"; calibrate_psi preflight; write_run_json
if [[ $FROM_CONTROLLER != direct ]]; then
  for asset in "${SHADOW_ASSETS[@]}"; do [[ ${saved_state[$asset]} == present ]] || die "before shadow asset is absent: $asset"; cmp -s "$before_deployment/$asset" "${installed_asset[$asset]}" || die "installed shadow asset differs from before controller: $asset"; done
else
  for asset in "${SHADOW_ASSETS[@]}"; do if [[ ${saved_state[$asset]} == present ]]; then cmp -s "$candidate_deployment/$asset" "${installed_asset[$asset]}" || die "direct bootstrap installed shadow asset differs: $asset"; fi; done
fi
for market in "${markets[@]}"; do systemctl stop "${unit[$market]}" >/dev/null 2>&1 || true; done
staging_started=true
install_shadow_assets "$candidate_deployment"; mkdir -p "$(dirname -- "$SHADOW_BINARY")"; monday_atomic_symlink "$candidate_binary" "$SHADOW_BINARY" || die 'candidate shadow link staging failed'

fixture_seed_market() {
  local market=$1 dir="${spool_dir[$1]}" i file data_sha now; [[ $TEST_ONLY == true ]] || return 0
  now=$(monotonic_seconds); mkdir -p "$dir"
  jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" --arg session "fixture-$run_id-$market" --argjson updated "$((now * 1000000000))" '{market:$market,dataset:$dataset,updated_at_ns:$updated,status:"synced",sequence_gaps:0,symbol_count:1,symbols:{FIXTURE:{}},snapshot_ready_count:1,bridged_count:1,stream_coverage_verified_count:1,snapshot_only_symbols:[],all_symbols_bridged:true,all_stream_coverage_verified:true,full_stream_coverage_verified:true,queue_saturated:false,disk_warning:false,upload_warning:false,upload_failure_count:0,session_id:$session}' >"$dir/health.json"
  for i in 1 2; do
    file="part-$((now+i)).jsonl"; printf '{"schema":"binance.market_tape.v2","type":"session_start"}\n' >"$dir/$file"; zstd -q -f "$dir/$file" -o "$dir/$file.zst"; rm -f -- "$dir/$file"; file="$file.zst"; data_sha=$(sha256_file "$dir/$file")
    jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" --arg file "$file" --arg sha "$data_sha" --arg session "fixture-$run_id-$market" --argjson start "$((now+i*1000))" --argjson end "$((now+i*1000+900))" '{schema:"binance.market_tape.v2",market:$market,dataset:$dataset,shard_id:"all",start_received_at_ns:$start,end_received_at_ns:$end,file:$file,sha256:$sha,symbols:["FIXTURE"],stream_types:["depth@100ms"],event_types:{agg_trade:0,raw_trade:0,book_ticker:0,force_order:0},has_replay_safe_checkpoint:true,lob_continuity:{sequence_gaps:0,reconnect_boundary:false,capture_session_id:$session}}' >"$dir/$file.manifest.json"
    printf '%s\n' "$data_sha" >"$dir/$file._SUCCESS"
  done
}
run_strict_verifier() {
  if [[ $TEST_ONLY == true ]]; then
    local expect_path=false argument
    for argument in "$@"; do
      if [[ $expect_path == true ]]; then
        [[ -f $argument && ! -L $argument ]] || return 1
        expect_path=false
      elif [[ $argument == --verify-segment ]]; then
        expect_path=true
      fi
    done
    [[ $expect_path == false ]]
    return
  fi
  systemd-run --quiet --wait --collect --unit="monday-rust-strict-verifier-$$.service" \
    --property=MemoryMax=1536M --property=MemoryHigh=1280M \
    --property=OOMScoreAdjust=500 --uid="$SERVICE_USER" -- "$candidate_binary" "$@"
}
run_strict_verifier_pair() { run_strict_verifier --require-lob-continuity "$@"; }
verify_adjacent_segments() {
  local -a args=(); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do
    path=$1; digest=$2; manifest_digest=$3; shift 3
    args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest")
  done
  run_strict_verifier_pair "${args[@]}"
}
verify_aggregate_trade_continuity() {
  local -a args=(--verify-aggregate-trade-continuity); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do path=$1; digest=$2; manifest_digest=$3; shift 3; args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest"); done
  run_strict_verifier "${args[@]}"
}
verify_raw_trade_continuity() {
  local -a args=(--verify-raw-trade-continuity); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do path=$1; digest=$2; manifest_digest=$3; shift 3; args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest"); done
  run_strict_verifier "${args[@]}"
}
systemctl_value() { systemctl_show "${unit[$1]}" "$2"; }
health_ok() {
  local market=$1 health="${spool_dir[$1]}/health.json"; [[ -f $health ]] || return 1
  if [[ $TEST_ONLY == true ]]; then jq -e --arg market "$market" '.market == $market and .status == "synced" and .sequence_gaps == 0' "$health" >/dev/null; return; fi
  local minimum_symbols=1000
  [[ $market == usdm ]] && minimum_symbols=100
  jq -e --arg market "$market" --arg dataset "${dataset[$market]}" \
    --arg symbols "${symbols[$market]}" --argjson minimum "$minimum_symbols" \
    --argjson started "${market_gate_started_ns[$market]}" \
    '.market == $market
      and .dataset == $dataset
      and (.updated_at_ns | type) == "number"
      and .updated_at_ns >= $started
      and .status == "synced"
      and .sequence_gaps == 0
      and (.symbol_count | type) == "number"
      and .symbol_count >= $minimum
      and .snapshot_ready_count == .symbol_count
      and .bridged_count == .symbol_count
      and .stream_coverage_verified_count == .symbol_count
      and .snapshot_only_symbols == []
      and .all_symbols_bridged == true
      and .all_stream_coverage_verified == true
      and (.full_stream_coverage_verified == null or .full_stream_coverage_verified == true)
      and .queue_saturated == false
      and .disk_warning == false
      and .upload_warning == false
      and .upload_failure_count == 0
      and (.session_id | type) == "string"
      and (.session_id | length) > 0
      and (if $market == "usdm" then (.symbols | keys | sort) == ($symbols | split(",") | sort) else true end)' \
    "$health" >/dev/null
}

health_catalog_sha256() {
  local market=$1
  jq -c '.symbols | keys | sort' "${spool_dir[$market]}/health.json" \
    | sha256sum | awk '{print $1}'
}

validate_observation_sample() {
  local market=$1 health="${spool_dir[$1]}/health.json" session symbols_now catalog upload_failures updated_ns current_mono
  health_ok "$market" || die "$market health failed during observation"
  session=$(jq -er '.session_id' "$health")
  [[ $session == "${phase_session[$market]}" ]] || die "$market collector session changed during observation"
  symbols_now=$(jq -er '.symbol_count' "$health")
  [[ $symbols_now == "${frozen_symbol_count[$market]}" ]] || die "$market symbol catalog changed during observation"
  catalog=$(health_catalog_sha256 "$market")
  [[ $catalog == "${frozen_catalog_sha256[$market]}" ]] || die "$market catalog membership changed during observation"
  upload_failures=$(jq -er '.upload_failure_count' "$health")
  [[ $upload_failures == "${initial_upload_failure_count[$market]}" ]] || die "$market upload failures changed during observation"
  updated_ns=$(jq -er '.updated_at_ns' "$health")
  current_mono=$(monotonic_seconds)
  if [[ $TEST_ONLY == true ]]; then
    last_health_updated_ns[$market]=$updated_ns
    last_health_advance_mono[$market]=$current_mono
    return 0
  fi
  local next_updated next_mono next_gap increment
  read -r next_updated next_mono next_gap increment < <(
    monday_observe_health_freshness \
      "${last_health_updated_ns[$market]}" \
      "${last_health_advance_mono[$market]}" \
      "${max_health_silence_seconds[$market]}" \
      "$updated_ns" "$current_mono" "$MAX_HEALTH_SILENCE_SECONDS"
  ) || die "$market health timestamp regressed or stopped advancing"
  last_health_updated_ns[$market]=$next_updated
  last_health_advance_mono[$market]=$next_mono
  max_health_silence_seconds[$market]=$next_gap
  health_samples[$market]=$((health_samples[$market] + increment))
}
verify_segments() {
  local market=$1 dir="${spool_dir[$1]}" path file digest manifest_digest count=0 previous_end=0 start end
  local -a segment_records=()
  while IFS= read -r path; do
    file=${path##*/}; digest=$(sha256_file "$path"); [[ $(sed -n '1p' "$path._SUCCESS") == "$digest" ]] || die "$market _SUCCESS digest mismatch"
    manifest_digest=$(sha256_file "$path.manifest.json")
    jq -e --arg market "$market" --arg digest "$digest" --arg session "${phase_session[$market]}" \
      '.schema == "binance.market_tape.v2" and .market == $market and .sha256 == $digest
       and .has_replay_safe_checkpoint == true and .lob_continuity.sequence_gaps == 0
       and .lob_continuity.reconnect_boundary == false
       and .lob_continuity.capture_session_id == $session' \
      "$path.manifest.json" >/dev/null || die "$market manifest failed strict checks"
    start=$(jq -er '.start_received_at_ns' "$path.manifest.json"); end=$(jq -er '.end_received_at_ns' "$path.manifest.json"); ((previous_end == 0 || start >= previous_end)) || die "$market segments overlap"; ((previous_end == 0 || start-previous_end <= MAX_SEGMENT_GAP_NS)) || die "$market segment gap is too large"; previous_end=$end; count=$((count+1))
    segment_records+=("$path" "$digest" "$manifest_digest")
  done < <(find "$dir" -maxdepth 1 -type f -name '*.jsonl.zst' | sort)
  ((count >= 2)) || die "$market has fewer than two complete segments"
  verify_adjacent_segments "${segment_records[@]}" || die "$market strict LOB continuity verifier failed"
  if [[ $market == spot ]]; then
    verify_aggregate_trade_continuity "${segment_records[@]}" || die "$market strict aggregate-trade continuity verifier failed"
    verify_raw_trade_continuity "${segment_records[@]}" || die "$market strict raw-trade continuity verifier failed"
    phase_strict_aggregate[$market]=true
    phase_strict_raw[$market]=true
  else
    phase_strict_aggregate[$market]=false
    phase_strict_raw[$market]=false
  fi
  phase_strict_lob[$market]=true
  phase_segments["$market"]=$count
}
run_market_gate_phase() {
  local market=$1 settle observation pid started_ns
  record_resource "shadow-$market" 2147483648; calibrate_psi "shadow-$market"; fixture_seed_market "$market"; mkdir -p "$OVERRIDE_ROOT"; printf 'SPOOL_DIR=%s\nSEGMENT_SECONDS=%s\n' "${spool_dir[$market]}" "$GATE_SEGMENT_SECONDS" >"${override_file[$market]}"; chmod 0640 "${override_file[$market]}"; systemctl reset-failed "${unit[$market]}" >/dev/null 2>&1 || true
  started_ns=$(date +%s%N); market_gate_started_ns[$market]=$started_ns
  systemctl start "${unit[$market]}"; systemctl_active "${unit[$market]}" || die "$market shadow did not start"; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market shadow restarted"; pid=$(systemctl_value "$market" MainPID); [[ $pid =~ ^[1-9][0-9]*$ ]] || die "$market MainPID unavailable"; phase_pid["$market"]=$pid; phase_exe_sha["$market"]=$candidate_payload
  if [[ $TEST_ONLY != true ]]; then
    exe_path=$(readlink -f -- "$PROC_ROOT/$pid/exe") || die "$market process executable is unavailable"
    [[ $(sha256_file "$exe_path") == "$candidate_payload" ]] || die "$market process executable identity differs from P1"
  fi
  settle=$(( $(monotonic_seconds) + HEALTH_SETTLE_DURATION_SECONDS )); while ! health_ok "$market"; do (( $(monotonic_seconds) < settle )) || die "$market health did not settle"; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market restarted while settling"; [[ $(systemctl_value "$market" MainPID) == "$pid" ]] || die "$market MainPID changed while settling"; sleep 1; done
  phase_session["$market"]=$(jq -er '.session_id' "${spool_dir[$market]}/health.json"); frozen_symbol_count[$market]=$(jq -er '.symbol_count' "${spool_dir[$market]}/health.json"); frozen_catalog_sha256[$market]=$(health_catalog_sha256 "$market"); initial_upload_failure_count[$market]=$(jq -er '.upload_failure_count' "${spool_dir[$market]}/health.json"); last_health_updated_ns[$market]=$(jq -er '.updated_at_ns' "${spool_dir[$market]}/health.json"); last_health_advance_mono[$market]=$(monotonic_seconds); max_health_silence_seconds[$market]=0; health_samples[$market]=1; market_observation_started_ns[$market]=$(date +%s%N)
  observation=$(( $(monotonic_seconds) + GATE_DURATION_SECONDS )); while (( $(monotonic_seconds) < observation )); do validate_observation_sample "$market"; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market restarted"; [[ $(systemctl_value "$market" MainPID) == "$pid" ]] || die "$market MainPID changed"; [[ $TEST_ONLY == true ]] && break; sleep 15; done
  phase_runtime["$market"]=$GATE_DURATION_SECONDS; systemctl stop "${unit[$market]}"; systemctl_active "${unit[$market]}" && die "$market shadow remained active"; verify_segments "$market"
}
run_candidate_drain() { local market=$1; [[ $TEST_ONLY == true ]] && return 0; systemd-run --quiet --wait --collect --unit="monday-rust-upload-drain-$$-$market.service" --property=MemoryMax="$((UPLOAD_DRAIN_MEMORY_MAX_BYTES / 1048576))M" --property=MemoryHigh=384M --uid="$SERVICE_USER" -- "$candidate_binary" --upload-only; }
assert_spool_drained() {
  local market=$1 remaining
  remaining=$(find "${spool_dir[$market]}" \( -type f -o -type l \) \( \
    -name '*.jsonl.part' -o -name '*.zst.tmp' -o -name '*.part.corrupt' -o \
    -name '*.uploaded-cleanup.json' -o -name '*.uploaded-cleanup.json.tmp' \
  \) -print -quit)
  [[ -z $remaining ]] || die "$market spool contains an unsealed or pending artifact: $remaining"
}
run_oss() {
  local market=$1; shift
  if [[ $TEST_ONLY == true ]]; then
    OSS_FIXTURE_MARKET=$market aliyun ossutil "$@" --profile "${aliyun_profile[$market]}" --endpoint fixture --region ap-northeast-1
  else
    runuser --user "$SERVICE_USER" -- env -i HOME="$SERVICE_HOME" PATH="$SAFE_PATH" \
      ALIYUN_PROFILE="${aliyun_profile[$market]}" aliyun ossutil "$@" \
      --profile "${aliyun_profile[$market]}" --endpoint "${oss_endpoint[$market]}" \
      --region "${oss_region[$market]}"
  fi
}
verify_oss_roundtrips() {
  local market=$1 readback="$tmp_dir/oss-$1" listing="$tmp_dir/oss-$1.list" uri manifest final_manifest data success file digest manifest_digest line token replay_safe
  local candidates_file unsafe_file
  local start end previous_end=0 count=0; local -a roundtrip_records=(); mkdir -p "$readback"
  candidates_file="$readback/replay-safe.tsv"; unsafe_file="$readback/replay-unsafe.tsv"
  : >"$candidates_file"; : >"$unsafe_file"
  local prefix
  if [[ $TEST_ONLY == true ]]; then
    prefix="oss://fixture/lake/raw/venue=binance/market=$market/dataset=${dataset[$market]}/shard=all/"
  else
    prefix="oss://${oss_bucket[$market]}/lake/raw/venue=binance/market=$market/dataset=${dataset[$market]}/shard=$(env_value "${market_env[$market]}" SHARD_ID)/"
  fi
  run_oss "$market" ls "$prefix" --recursive --short-format >"$listing"
  : >"$readback/manifest-uris"
  while IFS= read -r line; do
    line=${line%$'\r'}
    if [[ $line =~ (oss://[^[:space:]]+\.manifest\.json) ]]; then
      printf '%s\n' "${BASH_REMATCH[1]}" >>"$readback/manifest-uris"
      continue
    fi
    token=${line##*[$' \t']}; token=${token#/}
    [[ $token == *.manifest.json ]] && printf 'oss://%s/%s\n' "${oss_bucket[$market]}" "$token" >>"$readback/manifest-uris"
  done <"$listing"
  sort -u -o "$readback/manifest-uris" "$readback/manifest-uris"
  while IFS= read -r uri; do
    [[ -n $uri ]] || continue
    manifest="$readback/discovered-$count.json"; run_oss "$market" cp "$uri" "$manifest" --force --no-progress >/dev/null
    jq -e --arg market "$market" \
      '.market == $market
       and (.start_received_at_ns | type == "number")
       and (.end_received_at_ns | type == "number")
       and .end_received_at_ns >= .start_received_at_ns
       and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))' \
      "$manifest" >/dev/null || die "$market OSS manifest failed strict verification"
    start=$(jq -er '.start_received_at_ns' "$manifest"); end=$(jq -er '.end_received_at_ns' "$manifest")
    if [[ $TEST_ONLY != true ]] && ((end <= market_observation_started_ns[$market])); then
      continue
    fi
    jq -e --arg session "${phase_session[$market]}" \
      '.schema == "binance.market_tape.v2"
       and (.has_replay_safe_checkpoint | type == "boolean") and .lob_continuity.sequence_gaps == 0
       and .lob_continuity.reconnect_boundary == false
       and .lob_continuity.capture_session_id == $session
       and (.file | type == "string" and test("^[A-Za-z0-9._-]+\\.jsonl\\.zst$"))' \
      "$manifest" >/dev/null || die "$market OSS manifest failed strict verification"
    replay_safe=$(jq -er '.has_replay_safe_checkpoint' "$manifest")
    if [[ $replay_safe != true ]]; then
      printf '%s\t%s\t%s\n' "$start" "$end" "$uri" >>"$unsafe_file"
      continue
    fi
    printf '%s\t%s\t%s\n' "$start" "$end" "$uri" >>"$candidates_file"
    ((previous_end == 0 || start >= previous_end)) || die "$market OSS segments overlap"; ((previous_end == 0 || start - previous_end <= MAX_SEGMENT_GAP_NS)) || die "$market OSS continuity gap exceeded"; previous_end=$end
    file=$(jq -er '.file' "$manifest"); digest=$(jq -er '.sha256' "$manifest"); manifest_digest=$(sha256_file "$manifest")
    data="$readback/$file"; success="$data._SUCCESS"; final_manifest="$data.manifest.json"
    run_oss "$market" cp "$uri" "$final_manifest" --force --no-progress >/dev/null
    [[ $(sha256_file "$final_manifest") == "$manifest_digest" ]] || die "$market OSS manifest changed between reads"
    run_oss "$market" cp "${uri%/*}/$file" "$data" --force --no-progress >/dev/null; run_oss "$market" cp "${uri%/*}/$file._SUCCESS" "$success" --force --no-progress >/dev/null
    [[ $(sha256_file "$data") == "$digest" ]] || die "$market OSS data digest mismatch"; [[ $(sed -n '1p' "$success") == "$digest" ]] || die "$market OSS success marker mismatch"
    roundtrip_records+=("$data" "$digest" "$manifest_digest")
    count=$((count + 1))
  done <"$readback/manifest-uris"
  ((count >= 2)) || die "$market OSS readback has fewer than two triplets"
  monday_validate_replay_safe_manifest_order "$market" "$candidates_file" "$unsafe_file" \
    || die "$market replay-safe manifest ordering failed"
  verify_adjacent_segments "${roundtrip_records[@]}" || die "$market OSS strict LOB continuity verifier failed"
  if [[ $market == spot ]]; then
    verify_aggregate_trade_continuity "${roundtrip_records[@]}" || die "$market OSS strict aggregate-trade continuity verifier failed"
    verify_raw_trade_continuity "${roundtrip_records[@]}" || die "$market OSS strict raw-trade continuity verifier failed"
  fi
  phase_oss[$market]=$count
}

for market in "${markets[@]}"; do run_market_gate_phase "$market"; run_candidate_drain "$market"; assert_spool_drained "$market"; done
for market in "${markets[@]}"; do verify_oss_roundtrips "$market"; done
for market in "${markets[@]}"; do systemctl stop "${unit[$market]}" >/dev/null 2>&1 || true; done
restore_shadow_assets || die 'before shadow assets could not be restored'; restore_shadow_link || die 'before shadow link could not be restored'
if [[ $FROM_CONTROLLER != direct ]]; then [[ $(monday_active_controller_sha "$ROOT") == "$FROM_CONTROLLER" ]] || die 'active controller changed during Gate'; [[ $(readlink -f -- "$PRODUCTION_BINARY") == "$RELEASE_ROOT/$before_payload/binance-lob-archiver" ]] || die 'production identity changed during Gate'
else [[ $TEST_ONLY == true || $(readlink -f -- "$PRODUCTION_BINARY") == "$candidate_binary" ]] || die 'direct production identity changed during Gate'; fi
if [[ $old_shadow_present == true ]]; then [[ $(readlink -- "$SHADOW_BINARY") == "$old_shadow_target" ]] || die 'shadow link was not restored'; else [[ ! -e $SHADOW_BINARY && ! -L $SHADOW_BINARY ]] || die 'shadow link was not removed'; fi

checks=$(jq -cn '{before_pair_unchanged:true,shadow_staging_verified:true,shadow_assets_restored:true,resource_preflight:true,oss_triplets:true,strict_segment_verifier:true,final_identity:true}')
before_assets_json='{}'; staged_assets_json='{}'; restored_assets_json='{}'
for asset in "${SHADOW_ASSETS[@]}"; do
  before_assets_json=$(jq -cn --argjson values "$before_assets_json" --arg asset "$asset" \
    --arg state "${saved_state[$asset]}" --arg sha "${saved_sha[$asset]:-}" \
    '$values + {($asset):{state:$state,sha256:(if $sha == "" then null else $sha end)}}')
  staged_assets_json=$(jq -cn --argjson values "$staged_assets_json" --arg asset "$asset" \
    --arg sha "${candidate_asset_sha[$asset]:-}" \
    '$values + {($asset):$sha}')
  restored_assets_json=$(jq -cn --argjson values "$restored_assets_json" --arg asset "$asset" \
    --arg state "${saved_state[$asset]}" --arg sha "${restored_asset_sha[$asset]:-}" \
    '$values + {($asset):{state:$state,sha256:(if $sha == "" then null else $sha end)}}')
done
markets_json='{}'
for market in "${markets[@]}"; do
  health_sha=$(sha256_file "${spool_dir[$market]}/health.json")
  market_json=$(jq -cn --arg market "$market" --arg unit "${unit[$market]}" --arg dataset "${dataset[$market]}" --arg session "${phase_session[$market]}" --argjson pid "${phase_pid[$market]}" --arg exe "${phase_exe_sha[$market]}" --argjson runtime "${phase_runtime[$market]}" --argjson segments "${phase_segments[$market]}" --argjson oss "${phase_oss[$market]}" --arg health "$health_sha" --argjson strict_lob "${phase_strict_lob[$market]:-false}" --argjson strict_aggregate "${phase_strict_aggregate[$market]:-false}" --argjson strict_raw "${phase_strict_raw[$market]:-false}" '{market:$market,unit:$unit,dataset:$dataset,session_id:$session,main_pid:$pid,process_exe_sha256:$exe,n_restarts:0,observed_runtime_seconds:$runtime,segment_count:$segments,oss_triplet_count:$oss,health_sha256:$health,process_identity_verified:true,installed_shadow_assets_verified:true,strict_lob_continuity_readback:$strict_lob,strict_aggregate_trade_continuity_readback:$strict_aggregate,strict_raw_trade_continuity_readback:$strict_raw}')
  markets_json=$(jq -cn --argjson values "$markets_json" --arg market "$market" --argjson value "$market_json" '$values + {($market):$value}')
done
gate_finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ); production_eligible=true; [[ $TEST_ONLY == true ]] && production_eligible=false
jq -cn --arg schema monday.rust_lob_shadow_gate.v5 --arg from "$FROM_CONTROLLER" --arg after "$CANDIDATE_CONTROLLER" --arg candidate "$CANDIDATE_CONTROLLER" --arg payload "$candidate_payload" --arg runtime "$candidate_runtime" --arg bundle "$candidate_bundle" --arg source "$candidate_source" --arg before_payload "$before_payload" --arg before_runtime "$before_runtime" --arg before_bundle "$before_bundle" --arg before_source "$before_source" --arg run "$run_id" --arg spool "$run_spool" --arg started "$gate_started_at" --arg finished "$gate_finished_at" --argjson host_total "$host_memory_total" --argjson host_swap "$host_swap_total" --argjson production_memory "$production_memory_json" --argjson production_process "$production_process_json" --argjson production_assets "$production_asset_json" --argjson resources "$resource_samples" --argjson psi "$psi_windows" --argjson checks "$checks" --argjson markets "$markets_json" --argjson eligible "$production_eligible" --argjson test_only "$TEST_ONLY" --argjson before_assets "$before_assets_json" --argjson staged_assets "$staged_assets_json" --argjson restored_assets "$restored_assets_json" --arg shadow_binary "$SHADOW_BINARY" --arg candidate_binary "$candidate_binary" --arg old_shadow_target "$old_shadow_target" --argjson old_shadow_present "$old_shadow_present" \
  '{schema:$schema,control_plane_version:2,passed:true,production_eligible:$eligible,test_only:$test_only,transition:{before:$from,after:$after,topology:(if $from == "direct" then "direct-bootstrap" else "stable" end)},candidate_controller_sha256:$candidate,candidate_payload_sha256:$payload,candidate_runtime_contract_sha256:$runtime,candidate_deployment_bundle_sha256:$bundle,candidate_deployment_source_revision:$source,before:{controller:$from,payload_sha256:$before_payload,runtime_contract_sha256:$before_runtime,deployment_bundle_sha256:$before_bundle,deployment_source_revision:$before_source},run_id:$run,run_spool:$spool,started_at:$started,finished_at:$finished,required_duration_seconds:240,health_settle_seconds:240,segment_seconds:120,host_memory_total_bytes:$host_total,host_swap_total_bytes:$host_swap,production_memory:$production_memory,production_process:$production_process,production_assets:$production_assets,resource_admission:$resources,io_full_psi_windows:$psi,shadow_staging:{candidate_assets:$staged_assets,restored_assets:$restored_assets,before_assets:$before_assets,binary:{path:$shadow_binary,candidate_target:$candidate_binary,restored_target:(if $old_shadow_present then $old_shadow_target else null end),restored_present:$old_shadow_present}},checks:$checks,markets:$markets}' >"$gate_json.tmp"
chmod 0640 "$gate_json.tmp"; [[ ! -e $gate_json ]] || die 'gate receipt already exists'; mv -f -- "$gate_json.tmp" "$gate_json"; jq -e -f "$POLICY_SOURCE" "$gate_json" >/dev/null || die 'V2 Gate policy rejected the receipt'
if [[ $production_eligible == true ]]; then gate_sha=$(sha256_file "$gate_json"); printf '%s  gate.json\n' "$gate_sha" >"$passed_marker.tmp"; chmod 0640 "$passed_marker.tmp"; mv -f -- "$passed_marker.tmp" "$passed_marker"; fi
gate_finished=true; printf 'V2 Gate receipt: %s\nSHA-256: %s\n' "$gate_json" "$(sha256_file "$gate_json")"; [[ $production_eligible == true ]] && printf 'production shadow gate passed\n' || printf 'fixture Gate completed; not eligible for cutover\n'
