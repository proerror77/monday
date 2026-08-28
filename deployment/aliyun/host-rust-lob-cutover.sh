#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf '%s\n' \
    "Usage: ${0##*/} --from <sha|direct> --to <sha> --gate-receipt <path> --gate-sha256 <sha> [--root <path>]" >&2
}
die() { printf '%s\n' "$*" >&2; exit 1; }

ROOT=${MONDAY_ROOT:-/}; FROM=; TO=; GATE=; GATE_SHA=
while (($#)); do
  case $1 in
    --from) FROM=${2:-}; shift 2 ;;
    --to) TO=${2:-}; shift 2 ;;
    --gate-receipt) GATE=${2:-}; shift 2 ;;
    --gate-sha256) GATE_SHA=${2:-}; shift 2 ;;
    --root) ROOT=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
ROOT=${ROOT%/}; [[ -n $ROOT ]] || ROOT=/
[[ $FROM == direct || $FROM =~ ^[a-f0-9]{64}$ ]] || die 'from controller is invalid'
[[ $TO =~ ^[a-f0-9]{64}$ ]] || die 'target controller is invalid'
[[ $GATE_SHA =~ ^[a-f0-9]{64}$ ]] || die 'Gate receipt digest is invalid'
[[ -n $GATE ]] || die 'Gate receipt is required'
[[ $FROM != "$TO" ]] || die 'cutover requires distinct before and target controllers'
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

controller_root="$ROOT/opt/monday/releases/binance-lob-controller"
target_release="$controller_root/$TO"
monday_verify_controller_release "$ROOT" "$TO" || die 'target controller failed verification'
target_manifest="$target_release/release.json"
target_payload=$(monday_manifest_field "$target_manifest" artifact_sha256)

active=none
if [[ -L $controller_root/active ]]; then
  active=$(monday_active_controller_sha "$ROOT") || die 'active controller is invalid'
fi
production="$ROOT/opt/monday/bin/binance-lob-archiver"
old_production_target=
old_active_target=
if [[ $FROM == direct ]]; then
  [[ $active == none ]] || die 'direct bootstrap requires an absent active controller'
  [[ -e $production || -L $production ]] || die 'direct production payload is missing'
  old_production_target=$(readlink -f -- "$production") || die 'direct production payload is unresolved'
  monday_file_direct "$old_production_target" || die 'direct production payload is not a file'
  [[ $(monday_sha256_file "$old_production_target") == "$target_payload" ]] \
    || die 'bootstrap requires an unchanged payload'
else
  [[ $active == "$FROM" ]] || die 'active controller is not the requested before controller'
  monday_verify_controller_release "$ROOT" "$FROM" || die 'before controller failed verification'
  old_active_target=$(readlink -f -- "$controller_root/active")
  before_manifest="$controller_root/$FROM/release.json"
  before_payload=$(monday_manifest_field "$before_manifest" artifact_sha256)
  old_production_target="$ROOT/opt/monday/releases/binance-lob-archiver/$before_payload/binance-lob-archiver"
  [[ -L $production && $(readlink -f -- "$production") == "$old_production_target" ]] \
    || die 'production payload does not match the before controller'
fi
monday_validate_v2_gate "$GATE" "$FROM" "$TO" "$GATE_SHA" \
  || die 'Gate receipt does not authorize this exact pair transition'
jq -e --arg payload "$target_payload" --arg runtime "$(monday_manifest_field "$target_manifest" runtime_contract_sha256)" \
  '.candidate_payload_sha256 == $payload and .candidate_runtime_contract_sha256 == $runtime' \
  "$GATE" >/dev/null || die 'Gate receipt payload/runtime differs from target controller'

if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  [[ $(readlink -f -- "$0") == "$target_release/deployment/host-rust-lob-cutover.sh" ]] \
    || die 'cutover must execute from the target controller release'
fi

lock_root="$ROOT/run/lock"
mkdir -p "$lock_root"
exec 9>"$lock_root/monday-rust-lob-control-plane.lock"
if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  flock -n 9 || die 'another pair transition holds the control-plane lock'
fi
exec 8>"$lock_root/monday-rust-lob-recovery-drain.lock"
if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  flock -n 8 || die 'recovery drain is active'
fi
exec 7>"$lock_root/monday-rust-lob-spot.lock"
if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  flock -n 7 || die 'Spot operation is active'
fi
exec 6>"$lock_root/monday-rust-lob-usdm.lock"
if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  flock -n 6 || die 'USD-M operation is active'
fi

install_asset() {
  local source=$1 destination=$2 mode=$3 tmp
  monday_file_direct "$source" || return 1
  mkdir -p "$(dirname -- "$destination")"
  tmp="$destination.new.$$"
  install -m "$mode" "$source" "$tmp"
  mv -f "$tmp" "$destination"
  cmp -s "$source" "$destination"
}

install_pair_assets() {
  local deployment="$1/deployment" asset destination mode
  for asset in binance-lob-archiver-production@.service binance-lob-archiver-upload@.service; do
    install_asset "$deployment/$asset" "$ROOT/etc/systemd/system/$asset" 0644 || return 1
  done
  for asset in binance-lob-archiver-production-spot.env binance-lob-archiver-production-usdm.env; do
    install_asset "$deployment/$asset" "$ROOT/etc/monday/$asset" 0640 || return 1
  done
  install_asset "$deployment/binance-lob-archiver-recovery@.service" \
    "$ROOT/etc/systemd/system/binance-lob-archiver-recovery@.service" 0644 || return 1
  install_asset "$deployment/binance-lob-archiver-recovery@.timer" \
    "$ROOT/etc/systemd/system/binance-lob-archiver-recovery@.timer" 0644 || return 1
  install_asset "$deployment/host-rust-lob-recovery-queue.sh" \
    "$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue" 0755 || return 1
  install_asset "$deployment/monday-collector-health.sh" \
    "$ROOT/opt/monday/bin/monday-collector-health.sh" 0755 || return 1
}

restore_assets() {
  [[ $FROM != direct ]] || return 0
  install_pair_assets "$controller_root/$FROM"
}

rollback() {
  local status=$1
  (( status == 0 )) && return 0
  if (( mutated )); then
    if [[ $FROM == direct ]]; then
      monday_atomic_symlink "$old_production_target" "$production" || true
      rm -f -- "$controller_root/active"
    else
      monday_atomic_symlink "$old_active_target" "$controller_root/active" || true
      monday_atomic_symlink "$old_production_target" "$production" || true
      restore_assets || true
    fi
    if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
      systemctl daemon-reload >/dev/null 2>&1 || true
      systemctl start binance-lob-archiver-production@spot.service \
        binance-lob-archiver-production@usdm.service >/dev/null 2>&1 || true
    fi
  fi
  exit "$status"
}
mutated=0
trap 'rollback $?' EXIT

install_pair_assets "$target_release" || die 'target pair assets failed installation'
if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  systemctl daemon-reload
  systemctl stop binance-lob-archiver-production@spot.service \
    binance-lob-archiver-production@usdm.service
fi

target_binary="$ROOT/opt/monday/releases/binance-lob-archiver/$target_payload/binance-lob-archiver"
[[ -f $target_binary && ! -L $target_binary ]] || die 'target payload binary is missing'
monday_atomic_symlink "$target_binary" "$production" || die 'production payload switch failed'
mutated=1
monday_atomic_symlink "$target_release" "$controller_root/active" \
  || die 'controller active switch failed'
if [[ ${MONDAY_CUTOVER_FAIL_AFTER_ACTIVE:-0} == 1 ]]; then
  die 'fault injection after active pair commit'
fi

if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  systemctl start binance-lob-archiver-production@spot.service \
    binance-lob-archiver-production@usdm.service
  systemctl is-active --quiet binance-lob-archiver-production@spot.service \
    || die 'Spot did not start after pair commit'
  systemctl is-active --quiet binance-lob-archiver-production@usdm.service \
    || die 'USD-M did not start after pair commit'
fi

receipt_root=${MONDAY_CUTOVER_RECEIPT_ROOT:-$ROOT/data/monday/evidence/cutovers}
mkdir -p "$receipt_root/$TO"
receipt="$receipt_root/$TO/transition.json"
[[ ! -e $receipt && ! -L $receipt ]] || die 'transition receipt already exists for target controller'
receipt_tmp="$receipt.tmp.$$"
jq -cS -n --arg from "$FROM" --arg to "$TO" --arg payload "$target_payload" \
  --arg gate "$GATE" --arg gate_sha "$GATE_SHA" \
  '{schema:"monday.rust_lob_pair_transition.v2",control_plane_version:2,
    operation:"cutover",from_controller_sha256:$from,controller_sha256:$to,
    payload_sha256:$payload,gate_receipt:$gate,gate_sha256:$gate_sha,
    active_pair_committed:true,result:"success"}' >"$receipt_tmp"
mv -f "$receipt_tmp" "$receipt"
receipt_sha=$(monday_sha256_file "$receipt")
printf '%s  transition.json\n' "$receipt_sha" >"$receipt.sha256"
chmod 0440 "$receipt" "$receipt.sha256"
trap - EXIT
printf 'Pair cutover complete\nTransition receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
