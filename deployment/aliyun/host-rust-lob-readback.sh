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
[[ $TEST_ONLY == false || $ROOT != / ]] || die 'test mode requires an isolated fixture root'
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

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

runtime_identity='{}'; health_identity='{}'
capture_runtime_identity() {
  local market unit pid restarts exe env_file spool health session updated minimum_ns
  runtime_identity='{}'; health_identity='{}'
  [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && return 0
  minimum_ns=$(( $(monday_iso_epoch "$(jq -er '.completed_at' "$TRANSITION_RECEIPT")") * 1000000000 ))
  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    systemctl is-active --quiet "$unit" || die "production unit is inactive: $unit"
    restarts=$(systemctl show "$unit" --property=NRestarts --value)
    [[ $restarts == 0 ]] || die "production unit restarted during readback: $unit"
    pid=$(systemctl show "$unit" --property=MainPID --value)
    [[ $pid =~ ^[1-9][0-9]*$ ]] || die "production unit has no main PID: $unit"
    exe=$(readlink -f -- "$(monday_root_join "$ROOT" "proc/$pid/exe")") || die "production process executable is unavailable: $unit"
    [[ $exe == "$target" && $(monday_sha256_file "$exe") == "$payload" ]] \
      || die "production process identity differs: $unit"
    runtime_identity=$(jq -cn --argjson values "$runtime_identity" --arg market "$market" \
      --argjson pid "$pid" --arg sha "$(monday_sha256_file "$exe")" --argjson restarts "$restarts" \
      '$values + {($market):{main_pid:$pid,exe:$sha,n_restarts:$restarts}}')
    env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env") || die "production env is invalid: $market"
    spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file" | head -n1)
    [[ -n $spool && $spool == /* ]] || die "production spool is invalid: $market"
    [[ $ROOT == / ]] || spool="$ROOT$spool"
    health="$spool/health.json"
    [[ -f $health && ! -L $health ]] || die "production health is missing: $market"
    session=$(jq -er '.session_id // empty' "$health") || die "production health session is missing: $market"
    updated=$(jq -er '.updated_at_ns // 0' "$health")
    [[ $updated =~ ^[0-9]+$ && $updated -ge $minimum_ns ]] || die "production health is stale: $market"
    health_identity=$(jq -cn --argjson values "$health_identity" --arg market "$market" --arg session "$session" \
      --argjson observed "$updated" '$values + {($market):{session_id:$session,observed_at_ns:$observed}}')
  done
}
capture_runtime_identity
runtime_before=$runtime_identity; health_before=$health_identity
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
  [[ $runtime_identity == "$runtime_before" && $health_identity == "$health_before" ]] \
    || die 'process or health identity changed during OSS readback'
}

status_root=${MONDAY_UPLOAD_STATUS_ROOT:-$(monday_root_join "$ROOT" data/monday/spool/binance-lob)}
if [[ -e $status_root || -L $status_root ]]; then
  monday_path_direct "$status_root" || die 'upload status root is indirect'
fi
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
  aliyun ossutil cp "$uri" "$target" --profile ecs-role \
    --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --force \
    --no-progress >/dev/null
}
minimum_success_at=$(jq -er '.completed_at' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no completion timestamp'
for market in spot usdm; do
  status="$status_root/$market/upload-status.json"
  if [[ -f $status && ! -L $status ]]; then
    jq -e '.last_error == null' "$status" >/dev/null \
      || die "${market} upload status has a last_error"
    env_file=$(monday_root_join "$ROOT" "etc/monday/binance-lob-archiver-production-$market.env")
    dataset=$(env_value "$env_file" DATASET) || die "${market} production dataset is invalid"
    bucket=$(env_value "$env_file" OSS_BUCKET) || die "${market} production bucket is invalid"
    shard=$(env_value "$env_file" SHARD_ID) || die "${market} production shard is invalid"
    prefix="lake/raw/venue=binance/market=$market/dataset=$dataset/shard=$shard"
    assert_runtime_stable
    triplet_json=$(monday_verify_upload_triplet_readback "$status" "$market" "$dataset" \
      "$bucket" "$prefix" "$tmp" "$minimum_success_at" copy_oss) \
      || die "${market} independent OSS triplet readback failed"
    assert_runtime_stable
    markets=$(jq -cn --argjson prior "$markets" --argjson triplet "$triplet_json" \
      '$prior + [$triplet]')
    status_observations=$(jq -cn --argjson values "$status_observations" --arg market "$market" \
      --arg success "$(jq -er '.last_success_at' "$status")" \
      '$values + {($market):{last_success_at:$success,last_error:null}}')
  else
    [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] || die "${market} upload status is missing"
  fi
done

# OSS reads are independent, and each triplet was bracketed by this same
# active-pair/process check.  Keep one final sample in the receipt as well.
assert_runtime_stable
runtime_after=$runtime_identity; health_after=$health_identity

out_root=${MONDAY_READBACK_ROOT:-$(monday_root_join "$ROOT" data/monday/evidence/readbacks)}
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
  --argjson controller_projections "$controller_projections" \
  --argjson statuses "$status_observations" \
  '{schema:$schema,control_plane_version:2,controller_sha256:$controller,
    payload_sha256:$payload,transition_receipt:$transition,
    transition_receipt_sha256:$receipt,gate_receipt:$gate,gate_sha256:$gate_sha,
    production_link_verified:true,process_identity_verified:true,
    process_restarts_verified:true,installed_assets_verified:true,
    process_identity:$processes,health:$health,controller_projections:$controller_projections,
    upload_status:$statuses,
    oss_triplets:$markets,result:"success"}' >"$tmp_out"
mv -f "$tmp_out" "$out"
out_sha=$(monday_sha256_file "$out")
printf '%s  %s\n' "$out_sha" "$(basename -- "$out")" >"$out.sha256"
chmod 0440 "$out" "$out.sha256"
printf '%s\n' "$out"
