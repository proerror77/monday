#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s --controller <sha> --transition-receipt <path> --receipt-sha256 <sha> [--root <path>]\n' "${0##*/}" >&2
}
die() { printf '%s\n' "$*" >&2; exit 1; }

ROOT=${MONDAY_ROOT:-/}; CONTROLLER=; TRANSITION_RECEIPT=; RECEIPT_SHA=
while (($#)); do
  case $1 in
    --controller) CONTROLLER=${2:-}; shift 2 ;;
    --transition-receipt) TRANSITION_RECEIPT=${2:-}; shift 2 ;;
    --receipt-sha256) RECEIPT_SHA=${2:-}; shift 2 ;;
    --root) ROOT=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
ROOT=${ROOT%/}; [[ -n $ROOT ]] || ROOT=/
[[ $CONTROLLER =~ ^[a-f0-9]{64}$ ]] || die 'controller digest is invalid'
[[ $RECEIPT_SHA =~ ^[a-f0-9]{64}$ ]] || die 'transition receipt digest is invalid'
[[ -n $TRANSITION_RECEIPT ]] || die 'transition receipt is required'
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
monday_control_plane_validate_mode "$ROOT" "$TEST_ONLY" \
  || die 'production uses canonical root or fixture mode lacks an explicit sentinel'

# Readback is serialized with cutover/restore and takes the locks before any
# active, Gate, or process identity is read.
lock_root=$(monday_root_join "$ROOT" run/lock)
mkdir -p "$lock_root"
exec 9>"$lock_root/monday-rust-lob-control-plane.lock"
exec 8>"$lock_root/monday-rust-lob-recovery-drain.lock"
exec 7>"$lock_root/monday-rust-lob-spot.lock"
exec 6>"$lock_root/monday-rust-lob-usdm.lock"
if [[ $TEST_ONLY == false ]]; then
  flock -n 9 || die 'another pair operation holds the control-plane lock'
  flock -n 8 || die 'recovery drain is active'
  flock -n 7 || die 'Spot operation is active'
  flock -n 6 || die 'USD-M operation is active'
fi

FIXTURE_SYSTEMD=false
if [[ $TEST_ONLY == true && ${MONDAY_READBACK_FIXTURE_SYSTEMD:-0} == 1 ]]; then
  FIXTURE_SYSTEMD=true
  declare -A fixture_unit_state=() fixture_unit_file_state=() fixture_unit_load_state=()
  fixture_pid=${MONDAY_READBACK_FIXTURE_PID:-$$}
  fixture_enabled=${MONDAY_READBACK_FIXTURE_UNIT_FILE_STATE:-enabled}
  fixture_timer_active=${MONDAY_READBACK_FIXTURE_TIMER_ACTIVE:-active}
  fixture_timer_enabled=${MONDAY_READBACK_FIXTURE_TIMER_FILE_STATE:-enabled}
  for fixture_market in spot usdm; do
    fixture_unit="binance-lob-archiver-production@${fixture_market}.service"
    fixture_unit_state[$fixture_unit]=active
    fixture_unit_file_state[$fixture_unit]=$fixture_enabled
    fixture_unit_load_state[$fixture_unit]=loaded
  done
  for fixture_market in spot usdm; do
    fixture_unit="binance-lob-archiver-recovery@${fixture_market}.timer"
    fixture_unit_state[$fixture_unit]=$fixture_timer_active
    fixture_unit_file_state[$fixture_unit]=$fixture_timer_enabled
    fixture_unit_load_state[$fixture_unit]=loaded
  done
  systemctl() {
    local action=${1:-} unit=${2:-}
    case "$action" in
      is-active)
        [[ $2 == --quiet ]] && unit=$3
        [[ ${fixture_unit_state[$unit]:-inactive} == active ]] && return 0
        return 3 ;;
      show)
        unit=$2
        case "${3#--property=}" in
          LoadState) printf '%s\n' "${fixture_unit_load_state[$unit]:-loaded}" ;;
          ActiveState) [[ ${fixture_unit_state[$unit]:-inactive} == active ]] && printf 'active\n' || printf 'inactive\n' ;;
          SubState) [[ ${fixture_unit_state[$unit]:-inactive} == active ]] && printf 'running\n' || printf 'dead\n' ;;
          UnitFileState) printf '%s\n' "${fixture_unit_file_state[$unit]:-disabled}" ;;
          MainPID) printf '%s\n' "$fixture_pid" ;;
          NRestarts) printf '%s\n' "${MONDAY_READBACK_FIXTURE_RESTARTS:-0}" ;;
          *) printf '\n' ;;
        esac
        return 0 ;;
      *) return 0 ;;
    esac
  }
fi

monday_file_direct "$TRANSITION_RECEIPT" || die 'transition receipt is missing'
[[ $(monday_sha256_file "$TRANSITION_RECEIPT") == "$RECEIPT_SHA" ]] \
  || die 'transition receipt digest mismatch'
monday_verify_controller_release "$ROOT" "$CONTROLLER" \
  || die 'active controller release failed identity verification'
active=$(monday_active_controller_sha "$ROOT") || die 'controller active link is invalid'
[[ $active == "$CONTROLLER" ]] || die 'active controller differs from requested readback'

transition_from=$(jq -er '.from_controller_sha256' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no before controller'
transition_from_source_mode=$(jq -er '.from_source_mode' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no before source mode'
case "$transition_from_source_mode" in
  direct) transition_validator_from=direct ;;
  stable) transition_validator_from=$transition_from ;;
  *) die 'transition receipt has an invalid before source mode' ;;
esac
transition_gate=$(jq -er '.gate_receipt' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no exact Gate path'
transition_gate_sha=$(jq -er '.gate_sha256' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no exact Gate digest'
runtime=$(monday_manifest_field \
  "$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-controller/$CONTROLLER/release.json")" \
  runtime_contract_sha256) || die 'active controller runtime contract is invalid'
canonical_gate_root=$(monday_root_join "$ROOT" "data/monday/evidence/shadow-gates/$CONTROLLER/$runtime")
gate_relative=${transition_gate#"$canonical_gate_root"/}
[[ $gate_relative =~ ^runs/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*/gate\.json$ ]] \
  || die 'transition Gate path is outside the canonical V2 run path'
gate_dir=$(dirname -- "$transition_gate")
for gate_parent in \
  "$(monday_root_join "$ROOT" data)" "$(monday_root_join "$ROOT" data/monday)" "$(monday_root_join "$ROOT" data/monday/evidence)" \
  "$(monday_root_join "$ROOT" data/monday/evidence/shadow-gates)" \
  "$(monday_root_join "$ROOT" "data/monday/evidence/shadow-gates/$CONTROLLER")" \
  "$canonical_gate_root" "$canonical_gate_root/runs" "$gate_dir"; do
  monday_path_direct "$gate_parent" || die "transition Gate parent is indirect: $gate_parent"
done
monday_validate_v2_transition "$TRANSITION_RECEIPT" "$transition_validator_from" "$CONTROLLER" \
  "$transition_gate" "$transition_gate_sha" \
  || die 'transition receipt failed the exact V2 Gate-chain validator'
transition_process=$(jq -ce '.production_process' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no production process identity'
if [[ $TEST_ONLY == false ]]; then
  jq -e '.test_only == false and .production_eligible == true' "$TRANSITION_RECEIPT" >/dev/null \
    || die 'production readback requires an eligible transition'
  marker="$gate_dir/PASSED.sha256"
  [[ -f $marker && ! -L $marker ]] || die 'Gate PASSED marker is missing'
  marker_sha=$(awk '$2 == "gate.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' "$marker") \
    || die 'Gate PASSED marker is malformed'
  [[ $marker_sha == "$transition_gate_sha" ]] || die 'Gate marker does not match the transition Gate'
else
  jq -e '.test_only == true and .production_eligible == false' "$TRANSITION_RECEIPT" >/dev/null \
    || die 'fixture readback must never authorize production'
fi

payload=$(jq -er '.artifact_sha256' \
  "$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-controller/$CONTROLLER/release.json")")
production=$(monday_root_join "$ROOT" opt/monday/bin/binance-lob-archiver)
target=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver")
stable_projection=$(monday_root_join "$ROOT" opt/monday/releases/binance-lob-controller/active/binance-lob-archiver)
[[ -L $production && $(readlink -- "$production") == "$stable_projection" \
  && $(readlink -f -- "$production") == "$target" ]] \
  || die 'stable production projection does not match the active controller payload'
[[ $(monday_sha256_file "$target") == "$payload" ]] || die 'production binary digest mismatch'

deployment=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-controller/$CONTROLLER/deployment")
mapfile -t PAIR_ASSETS < <(monday_runtime_assets)
readonly PAIR_ASSETS
mapfile -t CONTROLLER_PROJECTION_ASSETS < <(monday_controller_projection_assets)
readonly CONTROLLER_PROJECTION_ASSETS
production_projection=$(monday_root_join "$ROOT" opt/monday/releases/binance-lob-controller/active/binance-lob-archiver)
[[ -L $production && $(readlink -- "$production") == "$production_projection" \
  && $(readlink -f -- "$production") == "$target" ]] \
  || die 'stable production projection does not match active C'
for asset in "${PAIR_ASSETS[@]}"; do
  installed=$(monday_runtime_asset_target "$ROOT" "$asset") || die "unknown runtime asset: $asset"
  expected=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-controller/active/deployment/$asset")
  [[ -L $installed && $(readlink -- "$installed") == "$expected" ]] \
    || die "installed pair asset is not the active projection: $asset"
  resolved=$(readlink -f -- "$installed") || die "installed pair projection is dangling: $asset"
  monday_file_direct "$resolved" || die "installed pair projection is not a file: $asset"
  [[ $(monday_sha256_file "$resolved") == "$(monday_sha256_file "$deployment/$asset")" ]] \
    || die "installed pair asset differs from active C: $asset"
done
controller_projections='{}'
for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
  installed=$(monday_controller_projection_target "$ROOT" "$asset") \
    || die "unknown controller projection: $asset"
  expected=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-controller/active/deployment/$asset")
  [[ -L $installed && $(readlink -- "$installed") == "$expected" ]] \
    || die "installed controller projection is not the active projection: $asset"
  resolved=$(readlink -f -- "$installed") \
    || die "installed controller projection is dangling: $asset"
  monday_file_direct "$resolved" \
    || die "installed controller projection is not a file: $asset"
  [[ $(monday_sha256_file "$resolved") == "$(monday_sha256_file "$deployment/$asset")" ]] \
    || die "installed controller projection differs from active C: $asset"
  controller_projections=$(jq -cn --argjson values "$controller_projections" \
    --arg asset "$asset" --arg target "/opt/monday/releases/binance-lob-controller/active/deployment/$asset" \
    --arg sha "$(monday_sha256_file "$resolved")" \
    '$values + {($asset):{target:$target,sha256:$sha}}')
done

runtime_identity='{}'; health_identity='{}'; recovery_scheduler_identity='{}'
capture_runtime_identity() {
  local market unit timer_unit pid restarts enabled exe env_file spool health session updated minimum_ns expected_observed dataset minimum_symbols policy
  local timer_active timer_enabled
  runtime_identity='{}'; health_identity='{}'; recovery_scheduler_identity='{}'
  [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 && $FIXTURE_SYSTEMD == false ]] && return 0
  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    systemctl is-active --quiet "$unit" || die "production unit is inactive: $unit"
    [[ $(systemctl show "$unit" --property=SubState --value) == running ]] \
      || die "production unit is not running: $unit"
    enabled=$(systemctl show "$unit" --property=UnitFileState --value)
    [[ $enabled == enabled ]] || die "production unit is not enabled: $unit"
    restarts=$(systemctl show "$unit" --property=NRestarts --value)
    [[ $restarts == 0 ]] || die "production unit restarted during readback: $unit"
    pid=$(systemctl show "$unit" --property=MainPID --value)
    [[ $pid =~ ^[1-9][0-9]*$ ]] || die "production unit has no main PID: $unit"
    exe=$(readlink -f -- "$(monday_root_join "$ROOT" "proc/$pid/exe")") || die "production process executable is unavailable: $unit"
    [[ $exe == "$target" && $(monday_sha256_file "$exe") == "$payload" ]] \
      || die "production process identity differs: $unit"
    runtime_identity=$(jq -cn --argjson values "$runtime_identity" --arg market "$market" \
      --argjson pid "$pid" --arg sha "$(monday_sha256_file "$exe")" --argjson restarts "$restarts" \
      --arg unit_file_state "$enabled" \
      '$values + {($market):{main_pid:$pid,exe:$sha,n_restarts:$restarts,unit_file_state:$unit_file_state}}')
    env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env") || die "production env is invalid: $market"
    spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file" | head -n1)
    [[ -n $spool && $spool == /* ]] || die "production spool is invalid: $market"
    [[ $ROOT == / ]] || spool="$ROOT$spool"
    health="$spool/health.json"
    [[ -f $health && ! -L $health ]] || die "production health is missing: $market"
    session=$(jq -er '.session_id // empty' "$health") || die "production health session is missing: $market"
    dataset=$(sed -n 's/^DATASET=//p' "$env_file" | head -n1)
    minimum_symbols=1000; [[ $market == usdm ]] && minimum_symbols=100
    policy="$deployment/rust-lob-runtime-health-policy.jq"
    updated=$(jq -er '.updated_at_ns // 0' "$health")
    if ! expected_observed=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$transition_process"); then
      [[ $FIXTURE_SYSTEMD == true ]] || die "transition health observation is missing: $market"
      expected_observed=0
    fi
    [[ $expected_observed =~ ^[0-9]+$ ]] || die "transition health observation is invalid: $market"
    minimum_ns=$expected_observed
    now_ns=$(date +%s%N)
    [[ $updated =~ ^[0-9]+$ && $updated -ge $minimum_ns && $updated -le $now_ns ]] \
      || die "production health is stale or in the future: $market"
    monday_verify_rust_lob_runtime_health "$policy" "$health" "$market" "$dataset" \
      "$minimum_symbols" "$minimum_ns" \
      || die "production health policy failed during readback: $market"
    health_identity=$(jq -cn --argjson values "$health_identity" --arg market "$market" --arg session "$session" \
      --arg status "$(jq -er '.status' "$health")" --argjson observed "$updated" \
      --argjson gaps "$(jq -er '.sequence_gaps' "$health")" \
      --argjson symbols "$(jq -er '.symbol_count' "$health")" \
      --argjson ready "$(jq -er '.snapshot_ready_count' "$health")" \
      '$values + {($market):{session_id:$session,observed_at_ns:$observed,status:$status,sequence_gaps:$gaps,symbol_count:$symbols,snapshot_ready_count:$ready}}')
    timer_unit="binance-lob-archiver-recovery@${market}.timer"
    systemctl is-active --quiet "$timer_unit" || die "recovery timer is inactive: $timer_unit"
    timer_active=$(systemctl show "$timer_unit" --property=ActiveState --value)
    timer_enabled=$(systemctl show "$timer_unit" --property=UnitFileState --value)
    [[ $timer_active == active && $timer_enabled == enabled ]] \
      || die "recovery timer is not active and enabled: $timer_unit"
    recovery_scheduler_identity=$(jq -cn --argjson values "$recovery_scheduler_identity" \
      --arg market "$market" --arg unit "$timer_unit" \
      '$values + {($market):{unit:$unit,active:true,enabled:true}}')
  done
}
capture_runtime_identity
runtime_before=$runtime_identity; health_before=$health_identity; recovery_scheduler_before=$recovery_scheduler_identity
assert_transition_process_identity() {
  local market expected_pid expected_exe expected_restarts expected_enabled expected_session expected_observed
  local current_pid current_exe current_restarts current_enabled current_session current_observed now_ns
  [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 && $FIXTURE_SYSTEMD == false ]] && return 0
  for market in spot usdm; do
    if ! jq -e --arg market "$market" '.[$market] | type == "object"' <<<"$transition_process" >/dev/null 2>&1; then
      [[ $FIXTURE_SYSTEMD == true ]] && continue
      die "transition process identity is missing: $market"
    fi
    expected_pid=$(jq -er --arg market "$market" '.[$market].main_pid' <<<"$transition_process") \
      || die "transition process PID is missing: $market"
    expected_exe=$(jq -er --arg market "$market" '.[$market].process_exe_sha256' <<<"$transition_process") \
      || die "transition process executable identity is missing: $market"
    expected_restarts=$(jq -er --arg market "$market" '.[$market].n_restarts' <<<"$transition_process") \
      || die "transition process restart count is missing: $market"
    expected_enabled=$(jq -er --arg market "$market" '.[$market].unit_file_state // "enabled"' <<<"$transition_process") \
      || die "transition process unit-file state is missing: $market"
    expected_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$transition_process") \
      || die "transition process session is missing: $market"
    expected_observed=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$transition_process") \
      || die "transition process timestamp is missing: $market"
    current_pid=$(jq -er --arg market "$market" '.[$market].main_pid' <<<"$runtime_identity")
    current_exe=$(jq -er --arg market "$market" '.[$market].exe' <<<"$runtime_identity")
    current_restarts=$(jq -er --arg market "$market" '.[$market].n_restarts' <<<"$runtime_identity")
    current_enabled=$(jq -er --arg market "$market" '.[$market].unit_file_state' <<<"$runtime_identity")
    current_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$health_identity")
    current_observed=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$health_identity")
    [[ $current_pid == "$expected_pid" && $current_exe == "$expected_exe" \
      && $current_restarts == "$expected_restarts" && $current_enabled == "$expected_enabled" \
      && $current_session == "$expected_session" ]] \
      || die "production process identity differs from the cutover receipt: $market"
    now_ns=$(date +%s%N)
    [[ $current_observed =~ ^[0-9]+$ && $expected_observed =~ ^[0-9]+$ \
      && $current_observed -ge $expected_observed && $current_observed -le $now_ns ]] \
      || die "production health timestamp is older than cutover or in the future: $market"
  done
}
assert_transition_process_identity
assert_runtime_stable() {
  local observed_active
  observed_active=$(monday_active_controller_sha "$ROOT") || die 'active controller disappeared during OSS readback'
  [[ $observed_active == "$CONTROLLER" ]] || die 'active controller changed during OSS readback'
  [[ -L $production && $(readlink -- "$production") == "$production_projection" \
    && $(readlink -f -- "$production") == "$target" ]] \
    || die 'stable production projection changed during OSS readback'
  for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
    installed=$(monday_controller_projection_target "$ROOT" "$asset") || die "unknown controller projection: $asset"
    expected=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-controller/active/deployment/$asset")
    [[ -L $installed && $(readlink -- "$installed") == "$expected" ]] \
      || die "controller projection changed during OSS readback: $asset"
    resolved=$(readlink -f -- "$installed") || die "controller projection is dangling: $asset"
    monday_file_direct "$resolved" || die "controller projection is not a file: $asset"
    [[ $(monday_sha256_file "$resolved") == "$(monday_sha256_file "$deployment/$asset")" ]] \
      || die "controller projection differs during OSS readback: $asset"
  done
  capture_runtime_identity
  [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 && $FIXTURE_SYSTEMD == false ]] && return 0
  assert_transition_process_identity
  [[ $runtime_identity == "$runtime_before" ]] \
    || die 'process identity changed during OSS readback'
  [[ $recovery_scheduler_identity == "$recovery_scheduler_before" ]] \
    || die 'recovery scheduler identity changed during OSS readback'
  for market in spot usdm; do
    before_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$health_before") \
      || die "initial health session is missing: $market"
    current_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$health_identity") \
      || die "current health session is missing: $market"
    [[ $current_session == "$before_session" ]] || die "health session changed during OSS readback: $market"
    before_updated=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$health_before") \
      || die "initial health timestamp is missing: $market"
    current_updated=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$health_identity") \
      || die "current health timestamp is missing: $market"
    now_ns=$(date +%s%N)
    [[ $before_updated =~ ^[0-9]+$ && $current_updated =~ ^[0-9]+$ \
      && $current_updated -ge $before_updated && $current_updated -le $now_ns ]] \
      || die "health timestamp moved backwards or into the future: $market"
  done
}

assert_direct_directory_chain() {
  local path=$1 current
  [[ $path == /* ]] || return 1
  current=${path%/}; [[ -n $current ]] || current=/
  while :; do
    if [[ -L $current ]]; then
      return 1
    elif [[ -e $current && ! -d $current ]]; then
      return 1
    elif [[ -d $current ]]; then
      [[ $(readlink -f -- "$current") == "$current" ]] || return 1
    fi
    [[ $current == / ]] && break
    current=${current%/*}; [[ -n $current ]] || current=/
  done
}

status_root=$(monday_root_join "$ROOT" data/monday/spool/binance-lob)
if [[ $TEST_ONLY == true && -n ${MONDAY_UPLOAD_STATUS_ROOT:-} ]]; then
  status_root=$MONDAY_UPLOAD_STATUS_ROOT
fi
assert_direct_directory_chain "$status_root" || die 'upload status root or ancestor is indirect'
tmp=$(mktemp -d "$(monday_root_join "$ROOT" tmp)/monday-readback.XXXXXX" 2>/dev/null || mktemp -d)
trap 'rm -rf "$tmp"' EXIT
markets='[]'
status_observations='{}'
env_value() {
  local file=$1 key=$2 count value
  count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || return 1
  value=$(sed -n "s/^${key}=//p" "$file")
  [[ -n $value ]] || return 1
  printf '%s\n' "$value"
}
copy_oss() {
  local uri=$1 target=$2
  [[ $uri == oss://* && -n $target ]] || return 1
  [[ $TEST_ONLY == true ]] && return 1
  [[ -x /usr/local/bin/aliyun ]] || die 'trusted OSS CLI is missing: /usr/local/bin/aliyun'
  env -i HOME=/var/lib/hft-collector LC_ALL=C \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    /usr/local/bin/aliyun ossutil cp "$uri" "$target" --profile ecs-role \
    --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --force \
    --no-progress >/dev/null
}
minimum_success_at=$(jq -er '.completed_at' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no completion timestamp'
minimum_commit_ns=$(jq -er '.completed_at_ns' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no nanosecond completion timestamp'
[[ $minimum_commit_ns =~ ^[0-9]+$ ]] || die 'transition completion nanosecond timestamp is invalid'
[[ $(monday_iso_epoch_ns "$minimum_success_at") == "$minimum_commit_ns" ]] \
  || die 'transition completion timestamp does not match its nanosecond value'
declare -A status_snapshot_path status_snapshot_sha status_dataset status_bucket status_prefix status_failure_count
# Snapshot both upload-status files before any OSS verification.  The final
# paired hash below is the only success point; a lane changing while the other
# lane is read back therefore cannot be silently accepted.
for market in spot usdm; do
  market_status_root="$status_root/$market"
  status="$market_status_root/upload-status.json"
  if [[ -e $status || -L $status ]]; then
    monday_file_direct "$status" || die "${market} upload status is indirect"
    status_snapshot_path[$market]="$tmp/$market.upload-status.snapshot.json"
    cp -p -- "$status" "${status_snapshot_path[$market]}" \
      || die "${market} upload status snapshot failed"
    monday_file_direct "${status_snapshot_path[$market]}" \
      || die "${market} upload status snapshot is indirect"
    status_snapshot_sha[$market]=$(monday_sha256_file "${status_snapshot_path[$market]}") \
      || die "${market} upload status snapshot hash failed"
    status_failure_count[$market]=$(jq -er 'if has("failure_count") then .failure_count else 0 end' \
      "${status_snapshot_path[$market]}") \
      || die "${market} upload status failure_count is invalid"
    [[ $(monday_sha256_file "$status") == "${status_snapshot_sha[$market]}" ]] \
      || die "${market} upload status changed during snapshot"
    env_file=$(monday_root_join "$ROOT" "etc/monday/binance-lob-archiver-production-$market.env")
    status_dataset[$market]=$(env_value "$env_file" DATASET) \
      || die "${market} production dataset is invalid"
    status_bucket[$market]=$(env_value "$env_file" OSS_BUCKET) \
      || die "${market} production bucket is invalid"
    shard=$(env_value "$env_file" SHARD_ID) \
      || die "${market} production shard is invalid"
    status_prefix[$market]="lake/raw/venue=binance/market=$market/dataset=${status_dataset[$market]}/shard=$shard"
  else
    [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] || die "${market} upload status is missing"
  fi
done
for market in spot usdm; do
  market_status_root="$status_root/$market"
  assert_direct_directory_chain "$market_status_root" \
    || die "${market} upload status market root or ancestor is indirect"
  status="$market_status_root/upload-status.json"
  if [[ -e $status || -L $status ]]; then
    monday_file_direct "$status" || die "${market} upload status is indirect"
  fi
  if [[ -f $status && ! -L $status ]]; then
    assert_runtime_stable
    expected_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$health_before") \
      || die "${market} current health session is unavailable"
    triplet_json=$(monday_verify_upload_triplet_readback "${status_snapshot_path[$market]}" "$market" \
      "${status_dataset[$market]}" "${status_bucket[$market]}" "${status_prefix[$market]}" \
      "$tmp" "$minimum_success_at" copy_oss "$expected_session" "$minimum_commit_ns") \
      || die "${market} independent OSS triplet readback failed"
    assert_runtime_stable
    markets=$(jq -cn --argjson prior "$markets" --argjson triplet "$triplet_json" \
      '$prior + [$triplet]')
    status_observations=$(jq -cn --argjson values "$status_observations" --arg market "$market" \
      --arg success "$(jq -er '.last_success_at' "${status_snapshot_path[$market]}")" \
      --argjson failure_count "${status_failure_count[$market]}" \
      --arg session "$expected_session" --arg snapshot_sha "${status_snapshot_sha[$market]}" \
      '$values + {($market):{last_success_at:$success,last_error:null,failure_count:$failure_count,session_id:$session,snapshot_sha256:$snapshot_sha}}')
  else
    [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] || die "${market} upload status is missing"
  fi
done

# OSS reads are independent, and each triplet was bracketed by this same
# active-pair/process check.  Re-read and hash both status files together
# before writing a success receipt so a late lane failure cannot be hidden by
# the other market's successful readback.
for market in spot usdm; do
  status="$status_root/$market/upload-status.json"
  if [[ -n ${status_snapshot_path[$market]:-} ]]; then
    final_status="$tmp/$market.upload-status.final.json"
    monday_file_direct "$status" || die "${market} upload status changed to an indirect file"
    cp -p -- "$status" "$final_status" || die "${market} final upload status read failed"
    monday_file_direct "$final_status" || die "${market} final upload status is indirect"
    [[ $(monday_sha256_file "$final_status") == "${status_snapshot_sha[$market]}" ]] \
      || die "${market} upload status changed during paired OSS readback"
    final_failure_count=$(jq -er 'if has("failure_count") then .failure_count else 0 end' \
      "$final_status") || die "${market} final upload status failure_count is invalid"
    [[ $final_failure_count == "${status_failure_count[$market]}" ]] \
      || die "${market} upload failure_count changed during paired OSS readback"
  fi
done
assert_runtime_stable
runtime_after=$runtime_identity; health_after=$health_identity; recovery_scheduler_after=$recovery_scheduler_identity

out_root=$(monday_root_join "$ROOT" data/monday/evidence/readbacks)
if [[ $TEST_ONLY == true && -n ${MONDAY_READBACK_ROOT:-} ]]; then
  out_root=$MONDAY_READBACK_ROOT
fi
assert_direct_directory_chain "$out_root" || die 'readback evidence root or ancestor is indirect'
if [[ ! -e $out_root && ! -L $out_root ]]; then mkdir -p "$out_root"; fi
monday_path_direct "$out_root" || die 'readback evidence root is indirect'
out="$out_root/$CONTROLLER"
[[ ! -e $out && ! -L $out ]] || die 'readback receipt already exists for this controller'
tmp_out="$out.tmp.$$"
  jq -cn --arg schema monday.rust_lob_operation_readback.v2 \
  --arg controller "$CONTROLLER" --arg payload "$payload" \
  --arg receipt "$RECEIPT_SHA" --arg transition "$TRANSITION_RECEIPT" \
  --arg gate "$transition_gate" --arg gate_sha "$transition_gate_sha" \
  --argjson markets "$markets" --argjson processes "$runtime_after" --argjson health "$health_after" \
  --argjson transition_process "$transition_process" \
  --argjson controller_projections "$controller_projections" \
  --argjson recovery_schedulers "$recovery_scheduler_after" \
  --argjson statuses "$status_observations" \
  '{schema:$schema,control_plane_version:2,controller_sha256:$controller,
    payload_sha256:$payload,transition_receipt:$transition,
    transition_receipt_sha256:$receipt,gate_receipt:$gate,gate_sha256:$gate_sha,
    production_link_verified:true,process_identity_verified:true,
    process_restarts_verified:true,unit_file_state_verified:true,
    health_policy_verified:true,recovery_schedulers_verified:true,installed_assets_verified:true,
    cutover_process_identity:$transition_process,
    process_identity:$processes,health:$health,recovery_schedulers:$recovery_schedulers,
    controller_projections:$controller_projections,
    upload_status:$statuses,
    oss_triplets:$markets,result:"success"}' >"$tmp_out"
mv -f "$tmp_out" "$out"
out_sha=$(monday_sha256_file "$out")
printf '%s  %s\n' "$out_sha" "$(basename -- "$out")" >"$out.sha256"
chmod 0440 "$out" "$out.sha256"
printf '%s\n' "$out"
