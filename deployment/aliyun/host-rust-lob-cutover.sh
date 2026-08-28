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

controller_root=$(monday_root_join "$ROOT" opt/monday/releases/binance-lob-controller)
production=$(monday_root_join "$ROOT" opt/monday/bin/binance-lob-archiver)
active_link="$controller_root/active"
stable_projection="$active_link/binance-lob-archiver"
lock_root=$(monday_root_join "$ROOT" run/lock)
mkdir -p "$lock_root"
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
target_binary=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-archiver/$target_payload/binance-lob-archiver")
monday_file_direct "$target_binary" || die 'target payload binary is missing'
[[ $(monday_sha256_file "$target_binary") == "$target_payload" ]] || die 'target payload digest mismatch'

# Only the immutable V2 receipt emitted by the candidate controller can
# authorize a transition.  Test receipts remain explicitly ineligible.
gate_dir=$(dirname -- "$GATE")
canonical_gate_root=$(monday_root_join "$ROOT" "data/monday/evidence/shadow-gates/$TO")
gate_relative=${GATE#"$canonical_gate_root"/}
[[ $gate_relative =~ ^[a-f0-9]{64}/runs/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*/gate\.json$ ]] \
  || die 'Gate receipt is outside the canonical V2 run path'
for gate_parent in \
  "$(monday_root_join "$ROOT" data)" "$(monday_root_join "$ROOT" data/monday)" \
  "$(monday_root_join "$ROOT" data/monday/evidence)" \
  "$(monday_root_join "$ROOT" data/monday/evidence/shadow-gates)" \
  "$canonical_gate_root" "$gate_dir"; do
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

active=none; old_active_target=; before_payload=; before_runtime=; before_release=
if [[ -L $active_link ]]; then active=$(monday_active_controller_sha "$ROOT") || die 'active controller is invalid'; fi
if [[ $FROM == direct ]]; then
  [[ $active == none ]] || die 'direct bootstrap requires an absent active controller'
  direct_payload_path=$(readlink -f -- "$production" 2>/dev/null || true)
  monday_file_direct "$direct_payload_path" || die 'direct production payload is missing'
  before_payload=$(monday_sha256_file "$direct_payload_path")
  [[ $before_payload == "$target_payload" ]] || die 'bootstrap requires an unchanged payload'
  before_runtime=$target_runtime
else
  [[ $active == "$FROM" ]] || die 'active controller is not the requested before controller'
  before_release="$controller_root/$FROM"
  monday_verify_controller_release "$ROOT" "$FROM" || die 'before controller failed verification'
  before_payload=$(monday_manifest_field "$before_release/release.json" artifact_sha256)
  before_runtime=$(monday_manifest_field "$before_release/release.json" runtime_contract_sha256)
  [[ -L $production && $(readlink -- "$production") == "$stable_projection" ]] \
    || die 'production binary is not the stable active projection'
  [[ $(readlink -f -- "$production") == \
    "$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-archiver/$before_payload/binance-lob-archiver")" ]] \
    || die 'production payload does not match the before controller'
fi

# Independently compute R0 from every live unit/env byte.  The target
# manifest is never used as a substitute for a missing or drifted before set.
live_runtime=$(monday_rust_lob_live_runtime_contract_sha256 "$ROOT") \
  || die 'live runtime contract is missing or indirect'
if [[ $FROM == direct ]]; then
  [[ $live_runtime == "$target_runtime" ]] || die 'bootstrap runtime assets differ from target R1'
else
  [[ $live_runtime == "$before_runtime" ]] || die 'live runtime assets differ from before R0'
fi

mapfile -t PAIR_ASSETS < <(monday_runtime_assets)
readonly PAIR_ASSETS
declare -A asset_target asset_state asset_sha asset_before_target asset_mode
for asset in "${PAIR_ASSETS[@]}"; do
  asset_target[$asset]=$(monday_runtime_asset_target "$ROOT" "$asset") || die "unknown runtime asset: $asset"
  asset_mode[$asset]=0644; [[ $asset == *.env ]] && asset_mode[$asset]=0640
  [[ $asset == *.sh ]] && asset_mode[$asset]=0755
  target=${asset_target[$asset]}
  if [[ -L $target ]]; then
    asset_before_target[$asset]=$(readlink -- "$target") || die "cannot read pair projection: $asset"
    expected_projection="$active_link/deployment/$asset"
    [[ ${asset_before_target[$asset]} == "$expected_projection" ]] \
      || die "pair asset projection is not stable: $asset"
    resolved=$(readlink -f -- "$target") || die "pair projection is dangling: $asset"
    monday_file_direct "$resolved" || die "pair projection target is not a file: $asset"
    asset_state[$asset]=projection; asset_sha[$asset]=$(monday_sha256_file "$resolved")
  elif [[ -f $target ]]; then
    asset_state[$asset]=present; asset_sha[$asset]=$(monday_sha256_file "$target")
  elif [[ ! -e $target ]]; then
    asset_state[$asset]=absent; asset_sha[$asset]=
  else
    die "installed pair asset is indirect: $target"
  fi
done
for asset in "${PAIR_ASSETS[@]}"; do
  source="$target_release/deployment/$asset"
  monday_file_direct "$source" || die "target pair asset is missing: $asset"
  if [[ $FROM == direct ]]; then
    [[ ${asset_state[$asset]} == present ]] || die "bootstrap runtime asset is absent: $asset"
    cmp -s "$source" "${asset_target[$asset]}" || die "bootstrap runtime asset changed: $asset"
  else
    [[ ${asset_state[$asset]} == projection ]] || die "before pair asset is not a stable projection: $asset"
    cmp -s "$before_release/deployment/$asset" "$(readlink -f -- "${asset_target[$asset]}")" \
      || die "before pair asset changed: $asset"
  fi
done

tmp_root=$(mktemp -d "$(monday_root_join "$ROOT" tmp/monday-cutover.XXXXXX)" 2>/dev/null || mktemp -d)
backup_root="$tmp_root/backup"; mkdir -p "$backup_root"
committed=0; projection_prepared=0; production_prepared=0
restore_direct_topology() {
  local asset target state
  for asset in "${PAIR_ASSETS[@]}"; do
    target=${asset_target[$asset]}; state=${asset_state[$asset]}
    case "$state" in
      present) install -m "${asset_mode[$asset]}" "$backup_root/$asset" "$target" 2>/dev/null || true ;;
      absent) rm -f -- "$target" 2>/dev/null || true ;;
      projection) rm -f -- "$target" 2>/dev/null || true; ln -s "${asset_before_target[$asset]}" "$target" 2>/dev/null || true ;;
    esac
  done
}
cleanup() {
  local status=$?; set +e
  if (( status != 0 )); then
    if (( committed == 1 )); then
      if [[ $FROM == direct ]]; then rm -f -- "$active_link"; else
        rm -f -- "$active_link.rollback.$$"; ln -s "$old_active_target" "$active_link.rollback.$$" && rm -f -- "$active_link" && mv -f -- "$active_link.rollback.$$" "$active_link" || true
      fi
    fi
    if (( production_prepared == 1 )); then
      rm -f -- "$production"
      if [[ $FROM == direct ]]; then [[ -n ${old_production_target:-} ]] && ln -s "$old_production_target" "$production" || true
      else ln -s "$stable_projection" "$production" || true; fi
    fi
    if (( projection_prepared == 1 )) && [[ $FROM == direct ]]; then restore_direct_topology; fi
    if [[ $TEST_ONLY == false ]]; then
      systemctl daemon-reload >/dev/null 2>&1 || true
      systemctl start binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service >/dev/null 2>&1 || true
    fi
  fi
  rm -rf -- "$tmp_root"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 143' HUP INT TERM

# Bootstrap is the only migration that converts regular files to static
# projections.  All bytes are saved before the active rename; V2 transitions
# perform no live per-file write at all.
old_production_target=$(readlink -f -- "$production" 2>/dev/null || true)
if [[ $FROM == direct ]]; then
  [[ -n $old_production_target ]] || die 'bootstrap production projection is unresolved'
  for asset in "${PAIR_ASSETS[@]}"; do
    [[ ${asset_state[$asset]} == present ]] || continue
    mkdir -p "$backup_root/$(dirname -- "$asset")"
    cp -p -- "${asset_target[$asset]}" "$backup_root/$asset"
  done
  projection_prepared=1
fi
if [[ $TEST_ONLY == false ]]; then
  systemctl stop binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service
fi
link_projection() {
  local link=$1 target=$2 temporary="$1.new.$$"
  rm -f -- "$temporary"; mkdir -p "$(dirname -- "$link")"
  ln -s "$target" "$temporary"; rm -f -- "$link"; mv -f -- "$temporary" "$link"
  [[ -L $link && $(readlink -- "$link") == "$target" ]]
}
if [[ $FROM == direct ]]; then
  for asset in "${PAIR_ASSETS[@]}"; do
    link_projection "${asset_target[$asset]}" "$active_link/deployment/$asset" \
      || die "could not stage stable pair projection: $asset"
  done
  link_projection "$production" "$stable_projection" || die 'could not stage stable production projection'
  production_prepared=1
else
  [[ -L $production && $(readlink -- "$production") == "$stable_projection" ]] \
    || die 'stable production projection changed before active commit'
fi
if [[ ${MONDAY_CUTOVER_FAIL_AFTER_ASSET_STAGE:-0} == 1 ]]; then
  die 'fault injection after static projection stage before active commit'
fi

# Atomic active rename is the sole pair commit.  No live runtime asset is
# copied after it; daemon-reload/start consumes only controller/active.
if [[ $FROM != direct ]]; then old_active_target=$(readlink -- "$active_link"); fi
monday_atomic_symlink "$target_release" "$active_link" || die 'controller active switch failed'
committed=1
if [[ ${MONDAY_CUTOVER_FAIL_AFTER_ACTIVE:-0} == 1 ]]; then
  die 'fault injection after active pair commit before daemon reload'
fi
if [[ $TEST_ONLY == false ]]; then
  systemctl daemon-reload
  systemctl start binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service
  systemctl is-active --quiet binance-lob-archiver-production@spot.service || die 'Spot did not start after pair commit'
  systemctl is-active --quiet binance-lob-archiver-production@usdm.service || die 'USD-M did not start after pair commit'
fi
[[ -L $production && $(readlink -- "$production") == "$stable_projection" ]] \
  || die 'stable production projection is not active'
[[ $(readlink -f -- "$production") == "$target_binary" ]] || die 'production payload does not resolve to target'
[[ $(monday_active_controller_sha "$ROOT") == "$TO" ]] || die 'active controller is not the target pair'
for asset in "${PAIR_ASSETS[@]}"; do
  [[ -L ${asset_target[$asset]} && $(readlink -- "${asset_target[$asset]}") == "$active_link/deployment/$asset" ]] \
    || die "stable pair projection is not active: $asset"
  resolved=$(readlink -f -- "${asset_target[$asset]}") || die "stable pair projection is dangling: $asset"
  monday_file_direct "$resolved" || die "stable pair projection is not a file: $asset"
  [[ $(monday_sha256_file "$resolved") == "$(monday_sha256_file "$target_release/deployment/$asset")" ]] \
    || die "active pair asset differs from target: $asset"
done

receipt_root=${MONDAY_CUTOVER_RECEIPT_ROOT:-$(monday_root_join "$ROOT" data/monday/evidence/cutovers)}
mkdir -p "$receipt_root/$TO"; receipt="$receipt_root/$TO/transition.json"
[[ ! -e $receipt && ! -L $receipt ]] || die 'transition receipt already exists for target controller'
completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
before_assets='{}'; installed_assets='{}'; installed_projections='{}'
for asset in "${PAIR_ASSETS[@]}"; do
  before_assets=$(jq -cn --argjson values "$before_assets" --arg asset "$asset" \
    --arg state "${asset_state[$asset]}" --arg sha "${asset_sha[$asset]:-}" \
    --arg target "${asset_before_target[$asset]:-}" \
    '$values + {($asset):{state:$state,sha256:(if $sha == "" then null else $sha end),target:(if $state == "projection" then $target else null end)}}')
  installed_assets=$(jq -cn --argjson values "$installed_assets" --arg asset "$asset" \
    --arg sha "$(monday_sha256_file "$(readlink -f -- "${asset_target[$asset]}")")" '$values + {($asset):$sha}')
  installed_projections=$(jq -cn --argjson values "$installed_projections" --arg asset "$asset" \
    --arg target "$(readlink -- "${asset_target[$asset]}")" '$values + {($asset):$target}')
done
gate_evidence=$(jq -cS '{candidate_control_bytes,resource_admission,io_full_psi_windows,shadow_staging,checks,markets}' "$GATE")
transition_tmp="$receipt.tmp.$$"
jq -cS -n --arg from "$FROM" --arg to "$TO" --arg payload "$target_payload" --arg runtime "$target_runtime" \
  --arg before_payload "$before_payload" --arg before_runtime "$before_runtime" --arg gate "$GATE" --arg gate_sha "$GATE_SHA" \
  --arg completed "$completed_at" --arg stable "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" \
  --arg projection "$stable_projection" --argjson evidence "$gate_evidence" --argjson before_assets "$before_assets" \
  --argjson installed_assets "$installed_assets" --argjson installed_projections "$installed_projections" \
  --argjson test_only "$TEST_ONLY" --argjson eligible "$( [[ $TEST_ONLY == true ]] && printf false || printf true )" \
  '{schema:"monday.rust_lob_pair_transition.v2",control_plane_version:2,operation:"cutover",
    test_only:$test_only,production_eligible:$eligible,from_controller_sha256:$from,controller_sha256:$to,
    payload_sha256:$payload,runtime_contract_sha256:$runtime,gate_receipt:$gate,gate_sha256:$gate_sha,
    gate_evidence:$evidence,active_pair_committed:true,completed_at:$completed,
    stable_production_projection:$stable,production_projection:$projection,
    before:{controller:$from,payload_sha256:$before_payload,runtime_contract_sha256:$before_runtime,
      production_projection:$stable,assets:$before_assets},
    installed_assets:$installed_assets,installed_projections:$installed_projections,result:"success"}' >"$transition_tmp"
chmod 0640 "$transition_tmp"; mv -f -- "$transition_tmp" "$receipt"
receipt_sha=$(monday_sha256_file "$receipt"); printf '%s  transition.json\n' "$receipt_sha" >"$receipt.sha256"; chmod 0440 "$receipt.sha256"
trap - EXIT
printf 'Pair cutover complete\nTransition receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
