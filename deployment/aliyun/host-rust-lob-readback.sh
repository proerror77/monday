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
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
monday_verify_controller_release "$ROOT" "$CONTROLLER" \
  || die 'active controller release failed identity verification'
active=$(monday_active_controller_sha "$ROOT") || die 'controller active link is invalid'
[[ $active == "$CONTROLLER" ]] || die 'active controller differs from requested readback'
monday_file_direct "$TRANSITION_RECEIPT" || die 'transition receipt is missing'
[[ $(monday_sha256_file "$TRANSITION_RECEIPT") == "$RECEIPT_SHA" ]] \
  || die 'transition receipt digest mismatch'
jq -e --arg controller "$CONTROLLER" \
  '.controller_sha256 == $controller and .result == "success"' \
  "$TRANSITION_RECEIPT" >/dev/null || die 'transition receipt is not a successful pair transition'

payload=$(jq -er '.artifact_sha256' \
  "$ROOT/opt/monday/releases/binance-lob-controller/$CONTROLLER/release.json")
production="$ROOT/opt/monday/bin/binance-lob-archiver"
target="$ROOT/opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver"
[[ -L $production && $(readlink -f -- "$production") == "$target" ]] \
  || die 'stable production link does not match the active controller payload'
[[ $(monday_sha256_file "$target") == "$payload" ]] || die 'production binary digest mismatch'

if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  for unit in binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service; do
    systemctl is-active --quiet "$unit" || die "production unit is inactive: $unit"
  done
fi

status_root=${MONDAY_UPLOAD_STATUS_ROOT:-$ROOT/data/monday/spool/binance-lob}
tmp=$(mktemp -d "${ROOT%/}/tmp/monday-readback.XXXXXX" 2>/dev/null || mktemp -d)
trap 'rm -rf "$tmp"' EXIT
markets='[]'
for market in spot usdm; do
  status="$status_root/$market/upload-status.json"
  if [[ -f $status && ! -L $status ]]; then
    jq -e --arg market "$market" '.market == $market or .market == null' "$status" >/dev/null \
      || die "${market} upload status has an unexpected market"
    markets=$(jq -cn --argjson prior "$markets" --arg market "$market" --arg status "$status" \
      '$prior + [{market:$market,status:$status}]')
  else
    [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] || die "${market} upload status is missing"
  fi
done

out_root=${MONDAY_READBACK_ROOT:-$ROOT/data/monday/evidence/readbacks}
mkdir -p "$out_root"
out="$out_root/$CONTROLLER"
[[ ! -e $out && ! -L $out ]] || die 'readback receipt already exists for this controller'
tmp_out="$out.tmp.$$"
jq -cn --arg schema monday.rust_lob_operation_readback.v1 \
  --arg controller "$CONTROLLER" --arg payload "$payload" \
  --arg receipt "$RECEIPT_SHA" --argjson markets "$markets" \
  '{schema:$schema,control_plane_version:2,controller_sha256:$controller,
    payload_sha256:$payload,transition_receipt_sha256:$receipt,
    production_link_verified:true,process_identity_verified:true,
    installed_assets_verified:true,oss_triplets:$markets,result:"success"}' >"$tmp_out"
mv -f "$tmp_out" "$out"
printf '%s\n' "$out"
