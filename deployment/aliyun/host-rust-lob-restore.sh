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
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

controller_root="$ROOT/opt/monday/releases/binance-lob-controller"
active=$(monday_active_controller_sha "$ROOT") || die 'active controller link is invalid'
[[ $active == "$CONTROLLER" ]] || die 'restore target is not the active controller'
monday_verify_controller_release "$ROOT" "$CONTROLLER" || die 'active controller failed verification'
release="$controller_root/$CONTROLLER"
manifest="$release/release.json"
payload=$(monday_manifest_field "$manifest" artifact_sha256)
binary="$ROOT/opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver"
[[ -f $binary && ! -L $binary && $(monday_sha256_file "$binary") == "$payload" ]] \
  || die 'active payload is missing or has the wrong digest'

lock_root="$ROOT/run/lock"
mkdir -p "$lock_root"
exec 9>"$lock_root/monday-rust-lob-control-plane.lock"
if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  flock -n 9 || die 'another pair operation holds the control-plane lock'
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
monday_atomic_symlink "$binary" "$ROOT/opt/monday/bin/binance-lob-archiver" \
  || die 'could not converge stable production payload'
if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  systemctl daemon-reload
  systemctl restart binance-lob-archiver-production@spot.service \
    binance-lob-archiver-production@usdm.service
  systemctl is-active --quiet binance-lob-archiver-production@spot.service \
    || die 'Spot is not active after restore'
  systemctl is-active --quiet binance-lob-archiver-production@usdm.service \
    || die 'USD-M is not active after restore'
  systemctl start binance-lob-archiver-recovery@spot.timer \
    binance-lob-archiver-recovery@usdm.timer
fi

receipt_root=${MONDAY_RESTORE_RECEIPT_ROOT:-$ROOT/data/monday/evidence/restores}
mkdir -p "$receipt_root/$CONTROLLER"
receipt="$receipt_root/$CONTROLLER/restore.json"
[[ ! -e $receipt && ! -L $receipt ]] || die 'restore receipt already exists for this controller'
tmp="$receipt.tmp.$$"
jq -cS -n --arg controller "$CONTROLLER" --arg payload "$payload" \
  '{schema:"monday.rust_lob_pair_restore.v2",control_plane_version:2,
    controller_sha256:$controller,payload_sha256:$payload,
    active_pair_converged:true,result:"success"}' >"$tmp"
mv -f "$tmp" "$receipt"
receipt_sha=$(monday_sha256_file "$receipt")
printf '%s  restore.json\n' "$receipt_sha" >"$receipt.sha256"
chmod 0440 "$receipt" "$receipt.sha256"
printf 'Pair restore complete\nRestore receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
