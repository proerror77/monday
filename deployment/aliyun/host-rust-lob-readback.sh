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
lock_root="$ROOT/run/lock"
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
transition_gate=$(jq -er '.gate_receipt' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no exact Gate path'
transition_gate_sha=$(jq -er '.gate_sha256' "$TRANSITION_RECEIPT") \
  || die 'transition receipt has no exact Gate digest'
runtime=$(monday_manifest_field \
  "$ROOT/opt/monday/releases/binance-lob-controller/$CONTROLLER/release.json" \
  runtime_contract_sha256) || die 'active controller runtime contract is invalid'
canonical_gate_root="$ROOT/data/monday/evidence/shadow-gates/$CONTROLLER/$runtime"
gate_relative=${transition_gate#"$canonical_gate_root"/}
[[ $gate_relative =~ ^runs/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*/gate\.json$ ]] \
  || die 'transition Gate path is outside the canonical V2 run path'
gate_dir=$(dirname -- "$transition_gate")
for gate_parent in \
  "$ROOT/data" "$ROOT/data/monday" "$ROOT/data/monday/evidence" \
  "$ROOT/data/monday/evidence/shadow-gates" \
  "$ROOT/data/monday/evidence/shadow-gates/$CONTROLLER" \
  "$canonical_gate_root" "$canonical_gate_root/runs" "$gate_dir"; do
  monday_path_direct "$gate_parent" || die "transition Gate parent is indirect: $gate_parent"
done
monday_validate_v2_transition "$TRANSITION_RECEIPT" "$transition_from" "$CONTROLLER" \
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
  "$ROOT/opt/monday/releases/binance-lob-controller/$CONTROLLER/release.json")
production="$ROOT/opt/monday/bin/binance-lob-archiver"
target="$ROOT/opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver"
stable_projection="$ROOT/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver"
[[ -L $production && $(readlink -- "$production") == "$stable_projection" \
  && $(readlink -f -- "$production") == "$target" ]] \
  || die 'stable production projection does not match the active controller payload'
[[ $(monday_sha256_file "$target") == "$payload" ]] || die 'production binary digest mismatch'

deployment="$ROOT/opt/monday/releases/binance-lob-controller/$CONTROLLER/deployment"
for asset in \
  binance-lob-archiver-production@.service \
  binance-lob-archiver-upload@.service \
  binance-lob-archiver-recovery@.service \
  binance-lob-archiver-recovery@.timer; do
  installed="$ROOT/etc/systemd/system/$asset"
  cmp -s "$deployment/$asset" "$installed" \
    || die "installed systemd asset differs: $asset"
done
  for asset in binance-lob-archiver-production-spot.env binance-lob-archiver-production-usdm.env; do
  cmp -s "$deployment/$asset" "$ROOT/etc/monday/$asset" \
    || die "installed environment differs: $asset"
done
cmp -s "$deployment/host-rust-lob-recovery-queue.sh" \
  "$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue" \
  || die 'installed recovery controller differs from active C'
cmp -s "$deployment/monday-collector-health.sh" \
  "$ROOT/opt/monday/bin/monday-collector-health.sh" \
  || die 'installed health controller differs from active C'

if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  for unit in binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service; do
    systemctl is-active --quiet "$unit" || die "production unit is inactive: $unit"
    restarts=$(systemctl show "$unit" --property=NRestarts --value)
    [[ $restarts == 0 ]] || die "production unit restarted during readback: $unit"
    pid=$(systemctl show "$unit" --property=MainPID --value)
    [[ $pid =~ ^[1-9][0-9]*$ ]] || die "production unit has no main PID: $unit"
    exe=$(readlink -f -- "$ROOT/proc/$pid/exe" 2>/dev/null || true)
    [[ $exe == "$target" ]] || die "production process identity differs: $unit"
  done
fi

status_root=${MONDAY_UPLOAD_STATUS_ROOT:-$ROOT/data/monday/spool/binance-lob}
if [[ -e $status_root || -L $status_root ]]; then
  monday_path_direct "$status_root" || die 'upload status root is indirect'
fi
tmp=$(mktemp -d "${ROOT%/}/tmp/monday-readback.XXXXXX" 2>/dev/null || mktemp -d)
trap 'rm -rf "$tmp"' EXIT
markets='[]'
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
    env_file="$ROOT/etc/monday/binance-lob-archiver-production-$market.env"
    dataset=$(env_value "$env_file" DATASET) || die "${market} production dataset is invalid"
    bucket=$(env_value "$env_file" OSS_BUCKET) || die "${market} production bucket is invalid"
    shard=$(env_value "$env_file" SHARD_ID) || die "${market} production shard is invalid"
    prefix="lake/raw/venue=binance/market=$market/dataset=$dataset/shard=$shard"
    triplet_json=$(monday_verify_upload_triplet_readback "$status" "$market" "$dataset" \
      "$bucket" "$prefix" "$tmp" "$minimum_success_at" copy_oss) \
      || die "${market} independent OSS triplet readback failed"
    markets=$(jq -cn --argjson prior "$markets" --argjson triplet "$triplet_json" \
      '$prior + [$triplet]')
  else
    [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] || die "${market} upload status is missing"
  fi
done

out_root=${MONDAY_READBACK_ROOT:-$ROOT/data/monday/evidence/readbacks}
if [[ ! -e $out_root && ! -L $out_root ]]; then mkdir -p "$out_root"; fi
monday_path_direct "$out_root" || die 'readback evidence root is indirect'
out="$out_root/$CONTROLLER"
[[ ! -e $out && ! -L $out ]] || die 'readback receipt already exists for this controller'
tmp_out="$out.tmp.$$"
jq -cn --arg schema monday.rust_lob_operation_readback.v2 \
  --arg controller "$CONTROLLER" --arg payload "$payload" \
  --arg receipt "$RECEIPT_SHA" --arg transition "$TRANSITION_RECEIPT" \
  --arg gate "$transition_gate" --arg gate_sha "$transition_gate_sha" \
  --argjson markets "$markets" \
  '{schema:$schema,control_plane_version:2,controller_sha256:$controller,
    payload_sha256:$payload,transition_receipt:$transition,
    transition_receipt_sha256:$receipt,gate_receipt:$gate,gate_sha256:$gate_sha,
    production_link_verified:true,process_identity_verified:true,
    process_restarts_verified:true,installed_assets_verified:true,
    oss_triplets:$markets,result:"success"}' >"$tmp_out"
mv -f "$tmp_out" "$out"
out_sha=$(monday_sha256_file "$out")
printf '%s  %s\n' "$out_sha" "$(basename -- "$out")" >"$out.sha256"
chmod 0440 "$out" "$out.sha256"
printf '%s\n' "$out"
