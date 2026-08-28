#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf '%s\n' "Usage: ${0##*/} --controller <active-sha> [--root <path>]" >&2
}
die() { printf '%s\n' "$*" >&2; exit 1; }

ROOT=${MONDAY_ROOT:-/}; CONTROLLER=
while (($#)); do
  case $1 in
    --controller) CONTROLLER=${2:-}; shift 2 ;;
    --root) ROOT=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
ROOT=${ROOT%/}; [[ -n $ROOT ]] || ROOT=/
[[ $CONTROLLER =~ ^[a-f0-9]{64}$ ]] || die 'controller digest is invalid'
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true
[[ $TEST_ONLY == false || $ROOT != / ]] || die 'test mode requires an isolated fixture root'
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

controller_root="$ROOT/opt/monday/releases/binance-lob-controller"

# Restore converges the complete pair under the same lock order as cutover.
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

active=$(monday_active_controller_sha "$ROOT") || die 'active controller link is invalid'
[[ $active == "$CONTROLLER" ]] || die 'restore target is not the active controller'
monday_verify_controller_release "$ROOT" "$CONTROLLER" || die 'active controller failed verification'
release="$controller_root/$CONTROLLER"
manifest="$release/release.json"
payload=$(monday_manifest_field "$manifest" artifact_sha256)
binary="$ROOT/opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver"
[[ -f $binary && ! -L $binary && $(monday_sha256_file "$binary") == "$payload" ]] \
  || die 'active payload is missing or has the wrong digest'

install_asset() {
  local source=$1 destination=$2 mode=$3 tmp
  monday_file_direct "$source" || return 1
  [[ ! -L $destination ]] || return 1
  mkdir -p "$(dirname -- "$destination")"
  tmp="$destination.new.$$"
  install -m "$mode" "$source" "$tmp"
  mv -f "$tmp" "$destination"
  cmp -s "$source" "$destination"
}

install_pair_assets() {
  local deployment="$release/deployment" asset
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

install_pair_assets || die 'could not converge installed pair assets'
stable_projection="$controller_root/active/binance-lob-archiver"
[[ -L $stable_projection && $(readlink -f -- "$stable_projection") == "$binary" ]] \
  || die 'active controller projection does not match the active payload'
monday_atomic_symlink "$stable_projection" "$ROOT/opt/monday/bin/binance-lob-archiver" \
  || die 'could not converge stable production projection'
[[ -L $ROOT/opt/monday/bin/binance-lob-archiver \
  && $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$stable_projection" \
  && $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$binary" ]] \
  || die 'stable production projection is not active'

process_json='{}'
if [[ $TEST_ONLY == false ]]; then
  systemctl daemon-reload
  systemctl restart binance-lob-archiver-production@spot.service \
    binance-lob-archiver-production@usdm.service
  systemctl is-active --quiet binance-lob-archiver-production@spot.service \
    || die 'Spot is not active after restore'
  systemctl is-active --quiet binance-lob-archiver-production@usdm.service \
    || die 'USD-M is not active after restore'
  systemctl start binance-lob-archiver-recovery@spot.timer \
    binance-lob-archiver-recovery@usdm.timer
  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    pid=$(systemctl show "$unit" --property=MainPID --value)
    [[ $pid =~ ^[1-9][0-9]*$ ]] || die "$market has no MainPID after restore"
    [[ $(systemctl show "$unit" --property=NRestarts --value) == 0 ]] \
      || die "$market restarted during restore"
    exe=$(readlink -f -- "$ROOT/proc/$pid/exe") || die "$market process executable is unavailable"
    [[ $exe == "$binary" && $(monday_sha256_file "$exe") == "$payload" ]] \
      || die "$market process identity differs from the active payload"
    process_json=$(jq -cn --argjson values "$process_json" --arg market "$market" \
      --argjson pid "$pid" --arg exe_sha "$(monday_sha256_file "$exe")" \
      '$values + {($market):{main_pid:$pid,process_exe_sha256:$exe_sha,restarts:0,active:true}}')
  done
fi

receipt_root=${MONDAY_RESTORE_RECEIPT_ROOT:-$ROOT/data/monday/evidence/restores}
mkdir -p "$receipt_root/$CONTROLLER"
receipt="$receipt_root/$CONTROLLER/restore.json"
[[ ! -e $receipt && ! -L $receipt ]] || die 'restore receipt already exists for this controller'
tmp="$receipt.tmp.$$"; completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
jq -cS -n --arg controller "$CONTROLLER" --arg payload "$payload" \
  --arg completed "$completed_at" --arg projection "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" \
  --argjson test_only "$TEST_ONLY" --argjson eligible "$( [[ $TEST_ONLY == true ]] && printf false || printf true )" \
  --argjson processes "$process_json" \
  '{schema:"monday.rust_lob_pair_restore.v2",control_plane_version:2,
    operation:"restore",test_only:$test_only,production_eligible:$eligible,
    controller_sha256:$controller,payload_sha256:$payload,
    stable_production_projection:$projection,active_pair_converged:true,
    process_identity:$processes,completed_at:$completed,result:"success"}' >"$tmp"
mv -f "$tmp" "$receipt"
receipt_sha=$(monday_sha256_file "$receipt")
printf '%s  restore.json\n' "$receipt_sha" >"$receipt.sha256"
chmod 0440 "$receipt" "$receipt.sha256"
printf 'Pair restore complete\nRestore receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
