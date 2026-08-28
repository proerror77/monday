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
tmp=$(mktemp -d "${ROOT%/}/tmp/monday-readback.XXXXXX" 2>/dev/null || mktemp -d)
trap 'rm -rf "$tmp"' EXIT
markets='[]'
for market in spot usdm; do
  status="$status_root/$market/upload-status.json"
  if [[ -f $status && ! -L $status ]]; then
    jq -e '.last_success_at | type == "string" and length > 0' "$status" >/dev/null \
      || die "${market} upload status is stale"
    triplet=$(jq -cer '.last_uploaded_triplet | objects' "$status") \
      || die "${market} upload status has no OSS triplet"
    jq -e --arg market "$market" --argjson triplet "$triplet" \
      '($triplet.data_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
       and ($triplet.manifest_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
       and ($triplet.success_sha256 | type == "string" and test("^[a-f0-9]{64}$"))' \
      "$status" >/dev/null || die "${market} OSS triplet digest is invalid"
    if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
      data_uri=$(jq -er '.last_uploaded_triplet.data_uri // .last_uploaded_triplet.object // empty' "$status")
      if [[ -z $data_uri ]]; then
        data_uri=$(jq -er '.last_uploaded_object' "$status")
      fi
      manifest_uri=$(jq -er --arg data "$data_uri" \
        '.manifest_uri // ($data + ".manifest.json")' <<<"$triplet")
      success_uri=$(jq -er --arg data "$data_uri" \
        '.success_uri // ($data + "._SUCCESS")' <<<"$triplet")
      for uri in "$data_uri" "$manifest_uri" "$success_uri"; do
        [[ $uri == oss://* ]] || die "${market} OSS URI is invalid"
      done
      data_file="$tmp/${market}-data"; manifest_file="$tmp/${market}-manifest"; success_file="$tmp/${market}-success"
      aliyun ossutil cp "$data_uri" "$data_file" --profile ecs-role --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --force
      aliyun ossutil cp "$manifest_uri" "$manifest_file" --profile ecs-role --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --force
      aliyun ossutil cp "$success_uri" "$success_file" --profile ecs-role --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --force
      [[ $(monday_sha256_file "$data_file") == "$(jq -er '.data_sha256' <<<"$triplet")" ]] || die "${market} OSS data digest mismatch"
      [[ $(monday_sha256_file "$manifest_file") == "$(jq -er '.manifest_sha256' <<<"$triplet")" ]] || die "${market} OSS manifest digest mismatch"
      success_sha=$(jq -er '.success_sha256' <<<"$triplet")
      data_sha=$(jq -er '.data_sha256' <<<"$triplet")
      if [[ $success_sha == "$data_sha" ]]; then
        [[ $(<"$success_file") == "$data_sha"$'\n' ]] \
          || die "${market} OSS success marker mismatch"
      else
        [[ $(monday_sha256_file "$success_file") == "$success_sha" ]] \
          || die "${market} OSS success digest mismatch"
      fi
      expected_dataset=$([[ $market == spot ]] && printf spot_all || printf usdm_perpetual_top100_lob)
      jq -e --arg market "$market" --arg dataset "$expected_dataset" \
        'type == "object" and .market == $market and .dataset == $dataset
         and (.shard_id | type == "string" and length > 0)
         and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
         and (.sha256 == $data_sha)' \
        --arg data_sha "$(jq -er '.data_sha256' <<<"$triplet")" "$manifest_file" >/dev/null \
        || die "${market} OSS manifest identity mismatch"
    fi
    markets=$(jq -cn --argjson prior "$markets" --arg market "$market" \
      --argjson triplet "$triplet" '$prior + [{market:$market,triplet:$triplet}]')
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
    process_restarts_verified:true,installed_assets_verified:true,
    oss_triplets:$markets,result:"success"}' >"$tmp_out"
mv -f "$tmp_out" "$out"
out_sha=$(monday_sha256_file "$out")
printf '%s  %s\n' "$out_sha" "$(basename -- "$out")" >"$out.sha256"
chmod 0440 "$out" "$out.sha256"
printf '%s\n' "$out"
