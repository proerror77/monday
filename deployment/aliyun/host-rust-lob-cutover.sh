#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf '%s\n' \
    "Usage: ${0##*/} --from <sha|direct> --to <sha> --gate-receipt <path> --gate-sha256 <sha> [--root <path>]" >&2
}
die() { printf 'pair cutover failed: %s\n' "$*" >&2; exit 1; }

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
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true
[[ $TEST_ONLY == true && $ROOT != / ]] || [[ $TEST_ONLY == false ]] \
  || die 'test mode requires an isolated fixture root'

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

controller_root="$ROOT/opt/monday/releases/binance-lob-controller"
production="$ROOT/opt/monday/bin/binance-lob-archiver"
active_link="$controller_root/active"
stable_projection="$controller_root/active/binance-lob-archiver"
lock_root="$ROOT/run/lock"
mkdir -p "$lock_root"

# Every identity read below occurs after the complete operation lock set is
# acquired.  Production keeps the release, drain, Spot and USD-M ordering.
exec 9>"$lock_root/monday-rust-lob-control-plane.lock"
exec 8>"$lock_root/monday-rust-lob-recovery-drain.lock"
exec 7>"$lock_root/monday-rust-lob-spot.lock"
exec 6>"$lock_root/monday-rust-lob-usdm.lock"
if [[ $TEST_ONLY == false ]]; then
  flock -n 9 || die 'another pair transition holds the control-plane lock'
  flock -n 8 || die 'recovery drain is active'
  flock -n 7 || die 'Spot operation is active'
  flock -n 6 || die 'USD-M operation is active'
fi

target_release="$controller_root/$TO"
monday_verify_controller_release "$ROOT" "$TO" || die 'target controller failed verification'
target_manifest="$target_release/release.json"
target_payload=$(monday_manifest_field "$target_manifest" artifact_sha256)
target_runtime=$(monday_manifest_field "$target_manifest" runtime_contract_sha256)
target_binary="$ROOT/opt/monday/releases/binance-lob-archiver/$target_payload/binance-lob-archiver"
monday_file_direct "$target_binary" || die 'target payload binary is missing'
[[ $(monday_sha256_file "$target_binary") == "$target_payload" ]] || die 'target payload digest mismatch'

# A production Gate is only authoritative when it has the canonical run
# layout and its exact sibling marker.  Fixture receipts remain non-eligible
# and are accepted only inside the isolated test root.
gate_dir=$(dirname -- "$GATE")
canonical_gate_root="$ROOT/data/monday/evidence/shadow-gates/$TO"
gate_relative=${GATE#"$canonical_gate_root"/}
[[ $gate_relative =~ ^${target_runtime}/runs/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*/gate\.json$ ]] \
  || die 'Gate receipt is outside the canonical V2 run path'
for gate_parent in \
  "$ROOT/data" "$ROOT/data/monday" "$ROOT/data/monday/evidence" \
  "$ROOT/data/monday/evidence/shadow-gates" "$canonical_gate_root" "$gate_dir"; do
  monday_path_direct "$gate_parent" || die "Gate receipt parent is indirect: $gate_parent"
done
if [[ $TEST_ONLY == false ]]; then
  passed_marker="$gate_dir/PASSED.sha256"
  [[ -f $passed_marker && ! -L $passed_marker ]] || die 'Gate PASSED marker is missing'
  marker_sha=$(awk '$2 == "gate.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' "$passed_marker") \
    || die 'Gate PASSED marker is malformed'
  [[ $marker_sha == "$GATE_SHA" ]] || die 'Gate PASSED marker does not match the supplied Gate digest'
  jq -e '.test_only == false and .production_eligible == true' "$GATE" >/dev/null \
    || die 'only a production-eligible Gate may authorize cutover'
else
  jq -e '.test_only == true and .production_eligible == false' "$GATE" >/dev/null \
    || die 'fixture Gate must never authorize production'
fi
monday_validate_v2_gate "$GATE" "$FROM" "$TO" "$GATE_SHA" \
  || die 'Gate receipt does not authorize this exact pair transition'
jq -e --arg payload "$target_payload" --arg runtime "$target_runtime" \
  '.candidate_payload_sha256 == $payload and .candidate_runtime_contract_sha256 == $runtime' \
  "$GATE" >/dev/null || die 'Gate receipt payload/runtime differs from target controller'
[[ $(monday_sha256_file "$target_release/deployment.sha256") == \
  "$(jq -er '.candidate_control_bytes.sha256' "$GATE")" ]] \
  || die 'Gate control bytes do not match the target controller'
expected_control_assets='{}'
while IFS= read -r control_asset; do
  control_sha=$(monday_sha256_file "$target_release/deployment/$control_asset") \
    || die "target control asset is missing: $control_asset"
  expected_control_assets=$(jq -cn --argjson values "$expected_control_assets" \
    --arg asset "$control_asset" --arg sha "$control_sha" \
    '$values + {($asset):$sha}')
done < <(monday_controller_assets)
jq -e --argjson expected "$expected_control_assets" \
  '.candidate_control_bytes.assets == $expected' "$GATE" >/dev/null \
  || die 'Gate control asset digests do not match the target controller'

active=none
if [[ -L $active_link ]]; then active=$(monday_active_controller_sha "$ROOT") || die 'active controller is invalid'; fi
old_active_target=; old_production_target=; before_payload=; before_runtime=; before_release=
if [[ $FROM == direct ]]; then
  [[ $active == none ]] || die 'direct bootstrap requires an absent active controller'
  [[ -e $production || -L $production ]] || die 'direct production payload is missing'
  old_production_target=$(readlink -f -- "$production") || die 'direct production payload is unresolved'
  monday_file_direct "$old_production_target" || die 'direct production payload is not a file'
  [[ $(monday_sha256_file "$old_production_target") == "$target_payload" ]] || die 'bootstrap requires an unchanged payload'
  before_payload=$target_payload; before_runtime=$target_runtime
else
  [[ $active == "$FROM" ]] || die 'active controller is not the requested before controller'
  monday_verify_controller_release "$ROOT" "$FROM" || die 'before controller failed verification'
  old_active_target=$(readlink -- "$active_link")
  before_release="$controller_root/$FROM"
  before_payload=$(monday_manifest_field "$before_release/release.json" artifact_sha256)
  before_runtime=$(monday_manifest_field "$before_release/release.json" runtime_contract_sha256)
  [[ -L $production && $(readlink -- "$production") == "$stable_projection" ]] \
    || die 'production binary is not the stable active projection'
  [[ $(readlink -f -- "$production") == "$ROOT/opt/monday/releases/binance-lob-archiver/$before_payload/binance-lob-archiver" ]] \
    || die 'production payload does not match the before controller'
fi

readonly -a PAIR_ASSETS=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
  binance-lob-archiver-recovery@.service
  binance-lob-archiver-recovery@.timer
  host-rust-lob-recovery-queue.sh
  monday-collector-health.sh
)
declare -A asset_target asset_mode asset_state asset_sha
for asset in "${PAIR_ASSETS[@]}"; do
  if [[ $asset == *.service || $asset == *.timer ]]; then
    asset_target[$asset]="$ROOT/etc/systemd/system/$asset"; asset_mode[$asset]=0644
  elif [[ $asset == *.env ]]; then
    asset_target[$asset]="$ROOT/etc/monday/$asset"; asset_mode[$asset]=0640
  elif [[ $asset == host-rust-lob-recovery-queue.sh ]]; then
    asset_target[$asset]="$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue"; asset_mode[$asset]=0755
  else
    asset_target[$asset]="$ROOT/opt/monday/bin/monday-collector-health.sh"; asset_mode[$asset]=0755
  fi
  if [[ -f ${asset_target[$asset]} && ! -L ${asset_target[$asset]} ]]; then
    asset_state[$asset]=present; asset_sha[$asset]=$(monday_sha256_file "${asset_target[$asset]}")
  elif [[ ! -e ${asset_target[$asset]} && ! -L ${asset_target[$asset]} ]]; then
    asset_state[$asset]=absent; asset_sha[$asset]=
  else
    die "installed pair asset is indirect: ${asset_target[$asset]}"
  fi
done

tmp_root=$(mktemp -d "${ROOT%/}/tmp/monday-cutover.XXXXXX" 2>/dev/null || mktemp -d)
stage_root="$tmp_root/stage"; backup_root="$tmp_root/backup"
mkdir -p "$stage_root/deployment" "$backup_root"
cleanup() { local status=$?; set +e
  if (( status != 0 )); then
    if [[ ${committed:-0} == 1 ]]; then
      if [[ $FROM == direct ]]; then rm -f -- "$active_link"; else monday_atomic_symlink "$old_active_target" "$active_link" >/dev/null 2>&1 || true; fi
    fi
    if [[ ${production_prepared:-0} == 1 ]]; then
      if [[ $FROM == direct ]]; then monday_atomic_symlink "$old_production_target" "$production" >/dev/null 2>&1 || true
      else monday_atomic_symlink "$stable_projection" "$production" >/dev/null 2>&1 || true; fi
    fi
    if [[ ${assets_installed:-0} == 1 ]]; then
      for asset in "${PAIR_ASSETS[@]}"; do
        target=${asset_target[$asset]}
        if [[ ${asset_state[$asset]} == present ]]; then
          install -m "${asset_mode[$asset]}" "$backup_root/$asset" "$target" >/dev/null 2>&1 || true
        else rm -f -- "$target"; fi
      done
    fi
    if [[ $TEST_ONLY == false ]]; then systemctl daemon-reload >/dev/null 2>&1 || true; systemctl start binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service >/dev/null 2>&1 || true; fi
  fi
  rm -rf -- "$tmp_root"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 143' HUP INT TERM

for asset in "${PAIR_ASSETS[@]}"; do
  source="$target_release/deployment/$asset"
  monday_file_direct "$source" || die "target asset is missing: $asset"
  install -m "${asset_mode[$asset]}" "$source" "$stage_root/deployment/$asset"
  cmp -s "$source" "$stage_root/deployment/$asset" || die "target asset staging mismatch: $asset"
  if [[ ${asset_state[$asset]} == present ]]; then
    install -m "${asset_mode[$asset]}" "${asset_target[$asset]}" "$backup_root/$asset"
  fi
done
if [[ ${MONDAY_CUTOVER_FAIL_AFTER_ASSET_STAGE:-0} == 1 ]]; then die 'fault injection after asset staging before active commit'; fi

if [[ $TEST_ONLY == false ]]; then
  systemctl daemon-reload
  systemctl stop binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service
fi

# The stable production projection is never pointed at a release digest.  On
# bootstrap the active link is committed first, then the stable link is made
# to resolve through it; on V2→V2 it is already present and unchanged.
monday_atomic_symlink "$target_release" "$active_link" || die 'controller active switch failed'
committed=1
if [[ $FROM == direct ]]; then
  production_prepared=1
  monday_atomic_symlink "$stable_projection" "$production" || die 'stable production projection failed'
fi
if [[ ${MONDAY_CUTOVER_FAIL_AFTER_ACTIVE:-0} == 1 ]]; then die 'fault injection after active pair commit'; fi

# Mark the live asset phase before its first write.  A failure (including a
# signal or a single failed install) must restore every already-written asset,
# rather than leaving a partially installed pair behind.
assets_installed=1
for asset in "${PAIR_ASSETS[@]}"; do
  target=${asset_target[$asset]}
  install -m "${asset_mode[$asset]}" "$stage_root/deployment/$asset" "$target"
  cmp -s "$stage_root/deployment/$asset" "$target" || die "installed target asset mismatch: $asset"
done
if [[ $TEST_ONLY == false ]]; then
  systemctl daemon-reload
  systemctl start binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service
  systemctl is-active --quiet binance-lob-archiver-production@spot.service || die 'Spot did not start after pair commit'
  systemctl is-active --quiet binance-lob-archiver-production@usdm.service || die 'USD-M did not start after pair commit'
fi
[[ -L $production && $(readlink -- "$production") == "$stable_projection" ]] || die 'stable production projection is not active'
[[ $(readlink -f -- "$production") == "$target_binary" ]] || die 'production payload does not resolve to target'
[[ $(monday_active_controller_sha "$ROOT") == "$TO" ]] || die 'active controller is not the target pair'

receipt_root=${MONDAY_CUTOVER_RECEIPT_ROOT:-$ROOT/data/monday/evidence/cutovers}
mkdir -p "$receipt_root/$TO"
receipt="$receipt_root/$TO/transition.json"
[[ ! -e $receipt && ! -L $receipt ]] || die 'transition receipt already exists for target controller'
completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
before_assets='{}'; installed_assets='{}'
for asset in "${PAIR_ASSETS[@]}"; do
  before_assets=$(jq -cn --argjson values "$before_assets" --arg asset "$asset" --arg state "${asset_state[$asset]}" --arg sha "${asset_sha[$asset]:-}" '$values + {($asset):{state:$state,sha256:(if $sha == "" then null else $sha end)}}')
  installed_assets=$(jq -cn --argjson values "$installed_assets" --arg asset "$asset" --arg sha "$(monday_sha256_file "${asset_target[$asset]}")" '$values + {($asset):$sha}')
done
gate_evidence=$(jq -cS '{candidate_control_bytes,resource_admission,io_full_psi_windows,shadow_staging,checks,markets}' "$GATE")
transition_tmp="$receipt.tmp.$$"
jq -cS -n --arg from "$FROM" --arg to "$TO" --arg payload "$target_payload" --arg runtime "$target_runtime" \
  --arg before_payload "$before_payload" --arg before_runtime "$before_runtime" \
  --arg gate "$GATE" --arg gate_sha "$GATE_SHA" --arg completed "$completed_at" \
  --arg stable "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" \
  --arg projection "$stable_projection" --argjson evidence "$gate_evidence" \
  --argjson before_assets "$before_assets" --argjson installed_assets "$installed_assets" \
  --argjson test_only "$TEST_ONLY" --argjson eligible "$( [[ $TEST_ONLY == true ]] && printf false || printf true )" \
  '{schema:"monday.rust_lob_pair_transition.v2",control_plane_version:2,operation:"cutover",
    test_only:$test_only,production_eligible:$eligible,from_controller_sha256:$from,controller_sha256:$to,
    payload_sha256:$payload,runtime_contract_sha256:$runtime,gate_receipt:$gate,gate_sha256:$gate_sha,
    gate_evidence:$evidence,active_pair_committed:true,completed_at:$completed,
    stable_production_projection:$stable,production_projection:$projection,
    before:{controller:$from,payload_sha256:$before_payload,runtime_contract_sha256:$before_runtime,
      production_projection:$stable,assets:$before_assets},
    installed_assets:$installed_assets,result:"success"}' >"$transition_tmp"
chmod 0640 "$transition_tmp"; mv -f -- "$transition_tmp" "$receipt"
receipt_sha=$(monday_sha256_file "$receipt")
printf '%s  transition.json\n' "$receipt_sha" >"$receipt.sha256"
chmod 0440 "$receipt.sha256"
trap - EXIT
printf 'Pair cutover complete\nTransition receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
