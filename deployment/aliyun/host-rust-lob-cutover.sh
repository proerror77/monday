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
readonly PRODUCTION_SLICE='system-binance\x2dlob\x2darchiver\x2dproduction.slice'
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true
PRODUCTION_HEALTH_WAIT_SECONDS=${MONDAY_CUTOVER_HEALTH_TIMEOUT_SECONDS:-240}
PRODUCTION_HEALTH_POLL_SECONDS=${MONDAY_CUTOVER_HEALTH_POLL_SECONDS:-1}
[[ $PRODUCTION_HEALTH_WAIT_SECONDS =~ ^[1-9][0-9]*$ && $PRODUCTION_HEALTH_WAIT_SECONDS -le 900 ]] \
  || die 'MONDAY_CUTOVER_HEALTH_TIMEOUT_SECONDS must be 1..900 seconds'
[[ $PRODUCTION_HEALTH_POLL_SECONDS =~ ^[1-9][0-9]*$ && $PRODUCTION_HEALTH_POLL_SECONDS -le 10 ]] \
  || die 'MONDAY_CUTOVER_HEALTH_POLL_SECONDS must be 1..10 seconds'

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
monday_control_plane_validate_mode "$ROOT" "$TEST_ONLY" \
  || die 'production uses canonical root or fixture mode lacks an explicit sentinel'

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

FIXTURE_SYSTEMD=false
if [[ $TEST_ONLY == true && ${MONDAY_CUTOVER_FIXTURE_SYSTEMD:-0} == 1 ]]; then
  FIXTURE_SYSTEMD=true
  declare -A fixture_unit_state=() fixture_unit_file_state=() fixture_unit_load_state=()
  fixture_calls=$(monday_root_join "$ROOT" run/cutover-fixture.calls)
  fixture_process_root=$(monday_root_join "$ROOT" run/cutover-fixture.processes)
  mkdir -p "$(dirname -- "$fixture_calls")"
  mkdir -p "$fixture_process_root"
  fixture_usdm_starts=0
  if [[ ${MONDAY_CUTOVER_FIXTURE_LEGACY_ACTIVE:-0} == 1 ]]; then
    while IFS= read -r fixture_legacy_unit; do
      fixture_unit_state[$fixture_legacy_unit]=active
      fixture_unit_file_state[$fixture_legacy_unit]=enabled
      fixture_unit_load_state[$fixture_legacy_unit]=loaded
    done < <(monday_rust_lob_legacy_writer_units)
  fi
  systemctl() {
    local action=${1:-} unit=${2:-} argument fixture_pid
    case "$action" in
      start)
        for argument in "$@"; do
          [[ $argument == -* || $argument == start ]] && continue
          if [[ $argument == *'@usdm.service' && ${MONDAY_CUTOVER_FIXTURE_FAIL_USDM:-0} == 1 ]]; then
            printf 'start %s\n' "$argument" >>"$fixture_calls"
            return 1
          fi
          if [[ $argument == *'@usdm.service' && ${MONDAY_CUTOVER_FIXTURE_FAIL_USDM_ONCE:-0} == 1 ]]; then
            fixture_usdm_starts=$((fixture_usdm_starts + 1))
            if (( fixture_usdm_starts == 1 )); then
              printf 'start %s\n' "$argument" >>"$fixture_calls"
              return 1
            fi
          fi
          fixture_unit_state[$argument]=active
          [[ -n ${fixture_unit_file_state[$argument]:-} ]] || fixture_unit_file_state[$argument]=enabled
          fixture_unit_load_state[$argument]=loaded
          if [[ $argument == *.service ]]; then
            printf '%s\n' "$(monday_active_controller_sha "$ROOT")" >"$fixture_process_root/${argument//@/_}"
          fi
          printf 'start %s\n' "$argument" >>"$fixture_calls"
        done
        return 0 ;;
      stop|disable|mask|unmask|enable)
        shift
        for argument in "$@"; do
          [[ $argument == -* ]] && continue
          if [[ $action == stop || $action == mask || $action == disable ]]; then
            fixture_unit_state[$argument]=inactive
            rm -f -- "$fixture_process_root/${argument//@/_}"
          fi
          case "$action" in
            mask) fixture_unit_file_state[$argument]=masked; fixture_unit_load_state[$argument]=masked ;;
            unmask) fixture_unit_file_state[$argument]=disabled; fixture_unit_load_state[$argument]=loaded ;;
            disable) fixture_unit_file_state[$argument]=disabled ;;
            enable) fixture_unit_file_state[$argument]=enabled ;;
          esac
          printf '%s %s\n' "$action" "$argument" >>"$fixture_calls"
        done
        return 0 ;;
      daemon-reload) printf 'daemon-reload\n' >>"$fixture_calls"; return 0 ;;
      is-active)
        [[ $2 == --quiet ]] && unit=$3
        [[ ${fixture_unit_state[$unit]:-inactive} == active ]] && return 0
        return 3 ;;
      show)
        unit=$2
        property=${3#--property=}; property=${property#--property=}
        if [[ $property == 'MemoryHigh,MemoryMax,ControlGroup' && $unit == "$PRODUCTION_SLICE" ]]; then
          printf 'verify-config %s\n' "$unit" >>"$fixture_calls"
          if [[ ${MONDAY_CUTOVER_FIXTURE_BAD_CONFIG:-0} == 1 ]]; then
            printf 'ControlGroup=/system.slice/wrong-production.slice\nMemoryMax=3758096385\nMemoryHigh=3221225472\n'
          else
            printf 'ControlGroup=/system.slice/system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice\nMemoryMax=3758096384\nMemoryHigh=3221225472\n'
          fi
          return 0
        fi
        if [[ $property == 'Slice,ControlGroup,MemoryMax' && $unit == binance-lob-archiver-production@* ]]; then
          printf 'verify-membership %s\n' "$unit" >>"$fixture_calls"
          market=${unit#*@}; market=${market%.service}
          if [[ ${MONDAY_CUTOVER_FIXTURE_BAD_MEMBERSHIP:-0} == 1 && $market == spot ]]; then
            printf 'MemoryMax=2684354561\nControlGroup=/system.slice/wrong.slice/%s\nSlice=wrong.slice\n' "$unit"
          else
            printf 'MemoryMax=2684354560\nControlGroup=/system.slice/system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice/%s\nSlice=system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice\n' "$unit"
          fi
          return 0
        fi
        case "$property" in
          LoadState) printf '%s\n' "${fixture_unit_load_state[$unit]:-loaded}" ;;
          ActiveState) [[ ${fixture_unit_state[$unit]:-inactive} == active ]] && printf 'active\n' || printf 'inactive\n' ;;
          SubState) [[ ${fixture_unit_state[$unit]:-inactive} == active ]] && printf 'running\n' || printf 'dead\n' ;;
          UnitFileState) printf '%s\n' "${fixture_unit_file_state[$unit]:-disabled}" ;;
          MainPID)
            fixture_pid=${MONDAY_CUTOVER_FIXTURE_PID:-$$}
            if [[ $unit == *'@spot.service' && -f "$(monday_root_join "$ROOT" run/cutover-fixture-spot-flip)" ]]; then
              fixture_pid=${MONDAY_CUTOVER_FIXTURE_SPOT_FLIP_PID:-$fixture_pid}
            fi
            printf '%s\n' "$fixture_pid" ;;
          NRestarts) printf '%s\n' "${MONDAY_CUTOVER_FIXTURE_RESTARTS:-0}" ;;
          *) printf '\n' ;;
        esac
        return 0 ;;
      is-enabled)
        [[ $2 == --quiet ]] && unit=$3
        printf '%s\n' "${fixture_unit_file_state[$unit]:-disabled}"
        [[ ${fixture_unit_file_state[$unit]:-disabled} == enabled ]] && return 0
        return 1 ;;
      *) return 0 ;;
    esac
  }
fi

target_release="$controller_root/$TO"
monday_verify_controller_release "$ROOT" "$TO" || die 'target controller failed verification'
target_manifest="$target_release/release.json"
target_payload=$(monday_manifest_field "$target_manifest" artifact_sha256)
target_runtime=$(monday_manifest_field "$target_manifest" runtime_contract_sha256)
target_binary=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-archiver/$target_payload/binance-lob-archiver")
monday_file_direct "$target_binary" || die 'target payload binary is missing'
[[ $(monday_sha256_file "$target_binary") == "$target_payload" ]] || die 'target payload digest mismatch'
target_production_runtime=$(monday_verify_production_runtime_assets \
  "$ROOT" "$target_release/deployment" "$target_payload") \
  || die 'target production runtime contract failed static verification'

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
gate_production_runtime=$(jq -ce '.production_runtime' "$GATE") \
  || die 'Gate receipt has no production runtime contract'
jq -e --argjson expected "$target_production_runtime" \
  '$expected == .production_runtime' "$GATE" >/dev/null \
  || die 'Gate production runtime contract differs from target controller'
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

active=none; old_active_target=; before_controller=; before_payload=; before_runtime=; before_release=
if [[ -L $active_link ]]; then active=$(monday_active_controller_sha "$ROOT") || die 'active controller is invalid'; fi
if [[ $FROM == direct ]]; then
  [[ $active != none ]] || die 'direct bootstrap requires an existing legacy active controller'
  old_active_target=$(readlink -- "$active_link") || die 'legacy active controller target is unreadable'
  legacy_target=$(readlink -f -- "$active_link") || die 'legacy active controller is dangling'
  legacy_controller=${legacy_target##*/}
  [[ $legacy_target == "$controller_root/$legacy_controller" ]] || die 'legacy active controller is not digest-addressed'
  monday_verify_legacy_controller_release "$ROOT" "$legacy_controller" "$production" \
    || die 'direct bootstrap requires an immutable v1 active controller'
  before_controller=$legacy_controller
  before_release=$legacy_target
  direct_payload_path=$(readlink -f -- "$production" 2>/dev/null || true)
  monday_file_direct "$direct_payload_path" || die 'direct production payload is missing'
  before_payload=$(monday_sha256_file "$direct_payload_path")
  [[ $before_payload == "$target_payload" ]] || die 'bootstrap requires an unchanged payload'
  before_runtime=$(jq -er '.runtime_contract_sha256' "$legacy_target/release.json") \
    || die 'legacy controller runtime is invalid'
else
  [[ $active == "$FROM" ]] || die 'active controller is not the requested before controller'
  before_controller=$FROM
  before_release="$controller_root/$FROM"
  monday_verify_controller_release "$ROOT" "$FROM" || die 'before controller failed verification'
  monday_verify_controller_projections "$ROOT" "$FROM" || die 'before controller projections are not stable'
  before_payload=$(monday_manifest_field "$before_release/release.json" artifact_sha256)
  before_runtime=$(monday_manifest_field "$before_release/release.json" runtime_contract_sha256)
  [[ -L $production && $(readlink -- "$production") == "$stable_projection" ]] \
    || die 'production binary is not the stable active projection'
  [[ $(readlink -f -- "$production") == \
    "$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-archiver/$before_payload/binance-lob-archiver")" ]] \
    || die 'production payload does not match the before controller'
fi

# The exact Gate receipt must name the controller that is actually installed
# before this operation.  For direct bootstrap that identity is resolved only
# from the immutable legacy active link; a different legacy C0 with the same
# P/R is still a different transition authority.
gate_from_controller=$(jq -er '.from_controller_sha256' "$GATE") \
  || die 'Gate receipt has no exact before controller'
if [[ $FROM == direct ]]; then
  [[ $gate_from_controller == "$legacy_controller" ]] \
    || die 'Gate receipt before controller differs from the resolved legacy active controller'
else
  [[ $gate_from_controller == "$FROM" ]] \
    || die 'Gate receipt before controller differs from the active controller'
fi

# Independently compute R0 from every live unit/env byte.  The target
# manifest is never used as a substitute for a missing or drifted before set.
if [[ $FROM == direct ]]; then
  # Direct bootstrap is the typed R0(v1, eight assets) -> R2(v2, nine assets)
  # migration; always hash the legacy eight-asset view for the before check.
  live_runtime=$(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT") \
    || die 'legacy live runtime contract is missing or indirect'
elif live_runtime=$(monday_rust_lob_live_runtime_contract_sha256 "$ROOT" 2>/dev/null); then
  :
else
  die 'live runtime contract is missing or indirect'
fi
if [[ $FROM == direct ]]; then
  [[ $live_runtime == "$before_runtime" ]] || die 'bootstrap runtime assets differ from legacy R0'
else
  [[ $live_runtime == "$before_runtime" ]] || die 'live runtime assets differ from before R0'
fi

mapfile -t PAIR_ASSETS < <(monday_runtime_assets)
readonly PAIR_ASSETS
mapfile -t CONTROLLER_PROJECTION_ASSETS < <(monday_controller_projection_assets)
readonly CONTROLLER_PROJECTION_ASSETS
declare -A asset_target asset_state asset_sha asset_before_target asset_mode
declare -A controller_projection_target controller_projection_state controller_projection_before_target controller_projection_mode
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
for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
  controller_projection_target[$asset]=$(monday_controller_projection_target "$ROOT" "$asset") \
    || die "unknown controller projection: $asset"
  controller_projection_mode[$asset]=0755
  target=${controller_projection_target[$asset]}
  if [[ -L $target ]]; then
    controller_projection_before_target[$asset]=$(readlink -- "$target") \
      || die "cannot read controller projection: $asset"
    expected_projection="$active_link/deployment/$asset"
    [[ ${controller_projection_before_target[$asset]} == "$expected_projection" ]] \
      || die "controller projection is not stable: $asset"
    resolved=$(readlink -f -- "$target") || die "controller projection is dangling: $asset"
    monday_file_direct "$resolved" || die "controller projection target is not a file: $asset"
    controller_projection_state[$asset]=projection
  elif [[ -f $target ]]; then
    controller_projection_state[$asset]=present
  elif [[ ! -e $target ]]; then
    controller_projection_state[$asset]=absent
  else
    die "controller projection is indirect: $target"
  fi
done
if [[ $FROM == direct ]]; then
  # Legacy C0 control bytes are read-only rollback evidence.  Bootstrap backs
  # up the fixed entrypoints but never sources or executes those bytes.
  for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
    [[ ${controller_projection_state[$asset]} == present ]] \
      || die "direct bootstrap controller projection is absent: $asset"
    legacy_asset="$before_release/deployment/$asset"
    if [[ -f $legacy_asset && ! -L $legacy_asset ]]; then
      cmp -s "$legacy_asset" "${controller_projection_target[$asset]}" \
        || die "legacy controller projection differs: $asset"
    fi
  done
else
  for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
    [[ ${controller_projection_state[$asset]} == projection ]] \
      || die "before controller projection is not stable: $asset"
    resolved=$(readlink -f -- "${controller_projection_target[$asset]}") \
      || die "before controller projection is dangling: $asset"
    cmp -s "$before_release/deployment/$asset" "$resolved" \
      || die "before controller projection changed: $asset"
  done
fi
for asset in "${PAIR_ASSETS[@]}"; do
  source="$target_release/deployment/$asset"
  monday_file_direct "$source" || die "target pair asset is missing: $asset"
  if [[ $FROM == direct ]]; then
    if [[ $asset == 'system-binance\x2dlob\x2darchiver\x2dproduction.slice' \
      && ${asset_state[$asset]} == absent ]]; then
      : # typed 8 -> 9 migration: the signed target installs this new asset
    else
      [[ ${asset_state[$asset]} == present ]] || die "bootstrap runtime asset is absent: $asset"
      cmp -s "$source" "${asset_target[$asset]}" || die "bootstrap runtime asset changed: $asset"
    fi
  else
    [[ ${asset_state[$asset]} == projection ]] || die "before pair asset is not a stable projection: $asset"
    cmp -s "$before_release/deployment/$asset" "$(readlink -f -- "${asset_target[$asset]}")" \
      || die "before pair asset changed: $asset"
  fi
done

tmp_root=$(mktemp -d "$(monday_root_join "$ROOT" tmp/monday-cutover.XXXXXX)" 2>/dev/null || mktemp -d)
backup_root="$tmp_root/backup"; controller_backup_root="$backup_root/controller"; mkdir -p "$backup_root" "$controller_backup_root"
writer_snapshot="$tmp_root/writer-state.tsv"
monday_rust_lob_writer_state_snapshot >"$writer_snapshot" \
  || die 'could not snapshot canonical writer states'
committed=0; projection_prepared=0; production_prepared=0; writer_containment_started=0; writer_containment_failed=0
restore_direct_topology() {
  local asset target state
  for asset in "${PAIR_ASSETS[@]}"; do
    target=${asset_target[$asset]}; state=${asset_state[$asset]}
    case "$state" in
      present)
        install -m "${asset_mode[$asset]}" "$backup_root/$asset" "$target" 2>/dev/null || return 1
        ;;
      absent)
        rm -f -- "$target" 2>/dev/null || return 1
        ;;
      projection)
        rm -f -- "$target" 2>/dev/null || return 1
        ln -s "${asset_before_target[$asset]}" "$target" 2>/dev/null || return 1
        ;;
    esac
  done
  for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
    target=${controller_projection_target[$asset]}; state=${controller_projection_state[$asset]}
    case "$state" in
      present)
        install -m "${controller_projection_mode[$asset]}" "$controller_backup_root/$asset" "$target" 2>/dev/null || return 1
        ;;
      absent)
        rm -f -- "$target" 2>/dev/null || return 1
        ;;
      projection)
        rm -f -- "$target" 2>/dev/null || return 1
        ln -s "${controller_projection_before_target[$asset]}" "$target" 2>/dev/null || return 1
        ;;
    esac
  done
}
cleanup() {
  local status=$? rollback_failed=false; set +e
  if (( status != 0 )); then
    if (( committed == 1 )); then
      if ! rm -f -- "$active_link.rollback.$$"; then
        rollback_failed=true
      elif ! ln -s "$old_active_target" "$active_link.rollback.$$"; then
        rollback_failed=true
      elif ! rm -f -- "$active_link"; then
        rollback_failed=true
      elif ! mv -f -- "$active_link.rollback.$$" "$active_link"; then
        rollback_failed=true
      fi
    fi
    if (( production_prepared == 1 )); then
      rm -f -- "$production" || rollback_failed=true
      if [[ $FROM == direct ]]; then
        if [[ -n ${old_production_target:-} ]]; then
          ln -s "$old_production_target" "$production" || rollback_failed=true
        else
          rollback_failed=true
        fi
      else
        ln -s "$stable_projection" "$production" || rollback_failed=true
      fi
    fi
    if (( projection_prepared == 1 )) && [[ $FROM == direct ]]; then
      restore_direct_topology || rollback_failed=true
    fi
    if (( writer_containment_started == 1 )) && [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
      if (( writer_containment_failed == 1 )); then
        rollback_failed=true
      else
        systemctl daemon-reload >/dev/null 2>&1 || rollback_failed=true
        if [[ $rollback_failed == false ]]; then
          if [[ $FROM == direct ]]; then
            monday_rust_lob_restore_writer_snapshot "$writer_snapshot" legacy \
              || rollback_failed=true
          else
            # A V2 transition may restore only the pre-existing V2 units; old
            # canonical writers must remain permanently contained.
            monday_rust_lob_restore_writer_snapshot "$writer_snapshot" v2 \
              || rollback_failed=true
            monday_rust_lob_contain_legacy_writers || rollback_failed=true
            monday_rust_lob_verify_legacy_contained || rollback_failed=true
          fi
          # Recovery timers are part of the V2 runtime contract.  Re-enable
          # them only after the previous writer state has been restored; a
          # failed rollback keeps the complete scheduler set contained below.
          monday_rust_lob_enable_recovery_schedulers || rollback_failed=true
        fi
        if [[ $rollback_failed == false ]]; then
          monday_rust_lob_verify_writer_state_snapshot() {
            local unit load active enabled
            while IFS=$'\t' read -r unit load active enabled; do
              [[ -n $unit ]] || continue
              [[ $FROM == direct ]] || {
                monday_rust_lob_legacy_writer_units | grep -Fqx "$unit" && continue
              }
              monday_rust_lob_verify_writer_state "$unit" "$load" "$active" "$enabled" || return 1
            done <"$writer_snapshot"
          }
          monday_rust_lob_verify_writer_state_snapshot || rollback_failed=true
        fi
      fi
      if [[ $rollback_failed == true ]]; then
        monday_rust_lob_contain_writers >/dev/null 2>&1 || true
        monday_rust_lob_verify_contained >/dev/null 2>&1 || true
        monday_rust_lob_contain_recovery_schedulers >/dev/null 2>&1 || true
        monday_rust_lob_verify_recovery_schedulers_contained >/dev/null 2>&1 || true
        printf 'pair rollback failed; all canonical writers remain contained\n' >&2
      fi
    fi
  fi
  rm -rf -- "$tmp_root"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 143' HUP INT TERM

# Bootstrap is the only migration that converts regular files to static
# projections.  All bytes are saved before the active rename; V2 transitions
# perform no live per-file write at all.  The active rename is deliberately
# first so an interrupted bootstrap has one authoritative recovery source.
old_production_target=$(readlink -f -- "$production" 2>/dev/null || true)
if [[ $FROM == direct ]]; then
  [[ -n $old_production_target ]] || die 'bootstrap production projection is unresolved'
  for asset in "${PAIR_ASSETS[@]}"; do
    if [[ ${asset_state[$asset]} == absent && $asset == 'system-binance\x2dlob\x2darchiver\x2dproduction.slice' ]]; then
      # The sole typed R0 -> R2 delta is the newly signed aggregate slice.
      continue
    fi
    [[ ${asset_state[$asset]} == present ]] || die "bootstrap runtime asset is not a direct file: $asset"
    mkdir -p "$backup_root/$(dirname -- "$asset")"
    cp -p -- "${asset_target[$asset]}" "$backup_root/$asset"
  done
  for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
    [[ ${controller_projection_state[$asset]} == present ]] \
      || die "bootstrap controller projection is not a direct file: $asset"
    mkdir -p "$controller_backup_root/$(dirname -- "$asset")"
    cp -p -- "${controller_projection_target[$asset]}" "$controller_backup_root/$asset"
  done
  projection_prepared=1
fi
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  writer_containment_started=1
  if ! monday_rust_lob_contain_writers; then
    writer_containment_failed=1
    die 'could not contain all canonical writers before pair transition'
  fi
  monday_rust_lob_verify_contained \
    || die 'canonical writers are not stopped, disabled, and runtime-masked'
  if ! monday_rust_lob_contain_recovery_schedulers; then
    writer_containment_failed=1
    die 'could not contain recovery schedulers before pair transition'
  fi
  monday_rust_lob_verify_recovery_schedulers_contained \
    || die 'recovery schedulers are not stopped, disabled, and runtime-masked'
fi
if [[ $FROM == direct ]]; then
  # The containment boundary must not widen the bootstrap identity window. Re-read
  # the unchanged direct payload and all live runtime bytes immediately before
  # active=C1 is committed; the target manifest is never used as R0 evidence.
  direct_payload_after_stop=$(readlink -f -- "$production" 2>/dev/null || true)
  monday_file_direct "$direct_payload_after_stop" \
    || die 'bootstrap direct payload disappeared after stopping lanes'
  [[ $(monday_sha256_file "$direct_payload_after_stop") == "$before_payload" ]] \
    || die 'bootstrap direct payload changed after stopping lanes'
  live_runtime_after_stop=$(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT") \
    || die 'bootstrap legacy runtime contract disappeared after stopping lanes'
  [[ $live_runtime_after_stop == "$before_runtime" ]] \
    || die 'bootstrap runtime contract changed after stopping lanes'
fi
link_projection() {
  local link=$1 target=$2 temporary="$1.new.$$"
  rm -f -- "$temporary"; mkdir -p "$(dirname -- "$link")"
  ln -s "$target" "$temporary"; rm -f -- "$link"; mv -f -- "$temporary" "$link"
  [[ -L $link && $(readlink -- "$link") == "$target" ]]
}
if [[ $FROM == direct ]]; then
  # No live projection is changed before active=C1 is committed.
  :
else
  [[ -L $production && $(readlink -- "$production") == "$stable_projection" ]] \
    || die 'stable production projection changed before active commit'
fi
if [[ ${MONDAY_CUTOVER_FAIL_AFTER_ASSET_STAGE:-0} == 1 ]]; then
  die 'fault injection after static projection stage before active commit'
fi

# Atomic active rename is the sole pair commit.  No live runtime asset is
# copied before it; bootstrap projections are repaired only after active=C1
# and are therefore recoverable from the active controller after a crash.
if [[ $FROM != direct ]]; then old_active_target=$(readlink -- "$active_link"); fi
monday_atomic_symlink "$target_release" "$active_link" || die 'controller active switch failed'
committed=1
if [[ ${MONDAY_CUTOVER_HARD_CRASH_AFTER_ACTIVE:-0} == 1 ]]; then
  kill -KILL "$$"
fi
if [[ ${MONDAY_CUTOVER_FAIL_AFTER_ACTIVE:-0} == 1 ]]; then
  die 'fault injection after active pair commit before daemon reload'
fi
link_controller_projections() {
  local asset target
  for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
    target=${controller_projection_target[$asset]}
    link_projection "$target" "$active_link/deployment/$asset" \
      || die "could not establish controller projection: $asset"
  done
}
if [[ $FROM == direct ]]; then
  production_prepared=1
  link_controller_projections
  for asset in "${PAIR_ASSETS[@]}"; do
    link_projection "${asset_target[$asset]}" "$active_link/deployment/$asset" \
      || die "could not establish stable pair projection: $asset"
  done
  link_projection "$production" "$stable_projection" \
    || die 'could not establish stable production projection'
else
  [[ -L $production && $(readlink -- "$production") == "$stable_projection" ]] \
    || die 'stable production projection is not active'
fi
# Re-read the exact static production contract after the active pair and all
# projections are prepared, but before any production process is started.
# This closes the identity window between Gate authorization and systemd.
post_commit_production_runtime=$(monday_verify_production_runtime_assets \
  "$ROOT" "$target_release/deployment" "$target_payload") \
  || die 'target production runtime contract disappeared after pair commit'
jq -e --argjson expected "$target_production_runtime" \
  '$expected == .' <<<"$post_commit_production_runtime" >/dev/null \
  || die 'target production runtime contract changed after pair commit'
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  if [[ $TEST_ONLY == false ]]; then
    # A direct bootstrap Gate may have restored a legacy runtime-only lease.
    # Drop that transient override so the newly committed signed slice asset
    # is the permanent source of the production envelope.
    systemctl revert --runtime "$PRODUCTION_SLICE" \
      || die 'could not clear the bootstrap production slice lease'
  fi
  systemctl daemon-reload || die 'daemon-reload failed after pair commit'
  monday_rust_lob_verify_systemd_production_slice_configured "$ROOT" \
    || die 'permanent production slice verification failed before start'
  cutover_started_ns=$(date +%s%N)
  systemctl unmask binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service \
    || die 'could not unmask V2 production lanes'
  systemctl enable binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service \
    || die 'could not enable V2 production lanes before start'
  systemctl start binance-lob-archiver-production@spot.service \
    || die 'Spot did not start after pair commit'
  systemctl start binance-lob-archiver-production@usdm.service \
    || die 'USD-M did not start after pair commit'
  systemctl is-active --quiet binance-lob-archiver-production@spot.service || die 'Spot did not start after pair commit'
  systemctl is-active --quiet binance-lob-archiver-production@usdm.service || die 'USD-M did not start after pair commit'
  monday_rust_lob_verify_systemd_production_membership "$ROOT" \
    || die 'production child membership is not exact after start'
else
  cutover_started_ns=0
fi
production_process='{}'
declare -A expected_pid expected_restarts expected_exe_sha
verify_production_process() {
  local market unit active sub pid restarts enabled exe exe_sha env_file env_file_resolved spool health session updated now_ns minimum_symbols policy
  local deadline health_session ready recorded_pid recorded_restarts recorded_exe recorded_session recorded_observed
  # Ordinary fixture cutovers never start a process.  The opt-in fixture
  # systemd path below exercises the same identity/freshness loop without
  # weakening production (and keeps its timeout bounded by the caller).
  [[ $TEST_ONLY == true && ${MONDAY_CUTOVER_FIXTURE_VERIFY_PROCESS:-0} != 1 ]] && return 0
  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env") \
      || die "$market production environment path is invalid"
    env_file_resolved=$(readlink -f -- "$env_file") \
      || die "$market production environment projection is dangling"
    monday_file_direct "$env_file_resolved" \
      || die "$market production environment projection is not a file"
    spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file_resolved")
    [[ $spool == "/data/monday/spool/binance-lob/$market" ]] \
      || die "$market production spool is not the governed path"
    [[ $ROOT == / ]] || spool="$ROOT$spool"
    health="$spool/health.json"
    minimum_symbols=1000; [[ $market == usdm ]] && minimum_symbols=100
    policy="$target_release/deployment/rust-lob-runtime-health-policy.jq"
    deadline=$(( $(date +%s) + PRODUCTION_HEALTH_WAIT_SECONDS )); health_session=; ready=false
    while :; do
      # Process identity is sampled on every poll.  A restart or PID/exe
      # change while waiting for the first health publication is a contained
      # cutover failure, never a reason to accept a later healthy file.
      active=$(systemctl show "$unit" --property=ActiveState --value)
      sub=$(systemctl show "$unit" --property=SubState --value)
      [[ $active == active && $sub == running ]] || die "$market production unit is not running after cutover"
      enabled=$(systemctl show "$unit" --property=UnitFileState --value)
      [[ $enabled == enabled ]] || die "$market production unit is not enabled after cutover"
      pid=$(systemctl show "$unit" --property=MainPID --value)
      [[ $pid =~ ^[1-9][0-9]*$ ]] || die "$market production unit has no MainPID after cutover"
      restarts=$(systemctl show "$unit" --property=NRestarts --value)
      [[ $restarts == 0 ]] || die "$market production unit restarted during cutover"
      if [[ -z ${expected_pid[$market]:-} ]]; then
        expected_pid[$market]=$pid; expected_restarts[$market]=$restarts
      else
        [[ $pid == "${expected_pid[$market]}" ]] || die "$market production MainPID changed while awaiting health"
        [[ $restarts == "${expected_restarts[$market]}" ]] || die "$market production restart count changed while awaiting health"
      fi
      exe=$(readlink -f -- "$(monday_root_join "$ROOT" "proc/$pid/exe")") \
        || die "$market production executable is unavailable after cutover"
      exe_sha=$(monday_sha256_file "$exe") || die "$market production executable cannot be hashed"
      [[ $exe == "$target_binary" && $exe_sha == "$target_payload" ]] \
        || die "$market production executable identity differs from target"
      if [[ -z ${expected_exe_sha[$market]:-} ]]; then expected_exe_sha[$market]=$exe_sha
      else [[ $exe_sha == "${expected_exe_sha[$market]}" ]] || die "$market production executable changed while awaiting health"; fi

      now_ns=$(date +%s%N); ready=false
      if [[ -f $health && ! -L $health ]]; then
        session=$(jq -er '.session_id // empty' "$health" 2>/dev/null || true)
        updated=$(jq -er '.updated_at_ns // 0' "$health" 2>/dev/null || true)
        if [[ $updated =~ ^[0-9]+$ ]] && (( updated > now_ns )); then
          die "$market production health timestamp is in the future"
        fi
        if [[ -n $session && $updated =~ ^[0-9]+$ ]] && (( updated >= cutover_started_ns && updated <= now_ns )); then
          if [[ -n $health_session && $session != "$health_session" ]]; then
            die "$market production health session changed while awaiting health"
          fi
          health_session=$session
          if monday_verify_rust_lob_runtime_health "$policy" "$health" "$market" \
            "$(sed -n 's/^DATASET=//p' "$env_file_resolved")" "$minimum_symbols" \
            "$((cutover_started_ns - 1))" >/dev/null 2>&1; then
            ready=true
          fi
        fi
      fi
      [[ $ready == true ]] && break
      (( $(date +%s) < deadline )) || die "$market production health did not become fresh and synchronized within ${PRODUCTION_HEALTH_WAIT_SECONDS}s"
      sleep "$PRODUCTION_HEALTH_POLL_SECONDS"
    done
    production_process=$(jq -cn --argjson values "$production_process" --arg market "$market" \
      --argjson pid "$pid" --arg exe "$exe_sha" --argjson restarts "$restarts" \
      --arg session "$health_session" --arg unit_file_state "$enabled" --argjson observed "$updated" \
      '$values + {($market):{active:true,unit_file_state:$unit_file_state,main_pid:$pid,process_exe_sha256:$exe,n_restarts:$restarts,session_id:$session,observed_at_ns:$observed}}')
  done
  # Both lanes are ready now; take one final paired sample before emitting the
  # transition receipt.  Spot may change while USD-M is catching up, so a
  # per-lane ready result alone is not sufficient authorization.
  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    active=$(systemctl show "$unit" --property=ActiveState --value)
    sub=$(systemctl show "$unit" --property=SubState --value)
    [[ $active == active && $sub == running ]] || die "$market production unit changed after both lanes became ready"
    enabled=$(systemctl show "$unit" --property=UnitFileState --value)
    [[ $enabled == enabled ]] || die "$market production unit was disabled after both lanes became ready"
    pid=$(systemctl show "$unit" --property=MainPID --value)
    restarts=$(systemctl show "$unit" --property=NRestarts --value)
    [[ $pid =~ ^[1-9][0-9]*$ && $restarts == 0 ]] || die "$market production process identity changed after readiness"
    recorded_pid=$(jq -er --arg market "$market" '.[$market].main_pid' <<<"$production_process")
    recorded_restarts=$(jq -er --arg market "$market" '.[$market].n_restarts' <<<"$production_process")
    [[ $pid == "$recorded_pid" && $restarts == "$recorded_restarts" ]] \
      || die "$market production PID/restart changed after both lanes became ready"
    exe=$(readlink -f -- "$(monday_root_join "$ROOT" "proc/$pid/exe")") \
      || die "$market production executable disappeared after readiness"
    exe_sha=$(monday_sha256_file "$exe") || die "$market production executable cannot be hashed after readiness"
    recorded_exe=$(jq -er --arg market "$market" '.[$market].process_exe_sha256' <<<"$production_process")
    [[ $exe == "$target_binary" && $exe_sha == "$recorded_exe" ]] \
      || die "$market production executable changed after both lanes became ready"
    env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env") \
      || die "$market production environment path is invalid after readiness"
    env_file_resolved=$(readlink -f -- "$env_file") || die "$market production environment projection is dangling after readiness"
    spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file_resolved")
    [[ $ROOT == / ]] || spool="$ROOT$spool"
    health="$spool/health.json"
    session=$(jq -er '.session_id // empty' "$health") || die "$market production health session disappeared after readiness"
    recorded_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$production_process")
    [[ $session == "$recorded_session" ]] || die "$market production health session changed after both lanes became ready"
    updated=$(jq -er '.updated_at_ns // 0' "$health")
    now_ns=$(date +%s%N)
    recorded_observed=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$production_process")
    [[ $updated =~ ^[0-9]+$ && $updated -ge "$recorded_observed" \
      && $updated -ge "$cutover_started_ns" && $updated -le "$now_ns" ]] \
      || die "$market production health is stale or in the future after readiness"
    minimum_symbols=1000; [[ $market == usdm ]] && minimum_symbols=100
    policy="$target_release/deployment/rust-lob-runtime-health-policy.jq"
    monday_verify_rust_lob_runtime_health "$policy" "$health" "$market" \
      "$(sed -n 's/^DATASET=//p' "$env_file_resolved")" "$minimum_symbols" \
      "$((cutover_started_ns - 1))" \
      || die "$market production health policy failed after readiness"
    production_process=$(jq -cn --argjson values "$production_process" --arg market "$market" \
      --argjson observed "$updated" '$values + {($market):($values[$market] + {observed_at_ns:$observed})}')
  done
}
verify_production_process
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  monday_rust_lob_enable_recovery_schedulers \
    || die 'recovery schedulers did not become active and enabled after cutover'
fi
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  monday_rust_lob_verify_legacy_contained \
    || die 'legacy canonical writers escaped the successful V2 transition'
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
for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
  target=${controller_projection_target[$asset]}
  [[ -L $target && $(readlink -- "$target") == "$active_link/deployment/$asset" ]] \
    || die "stable controller projection is not active: $asset"
  resolved=$(readlink -f -- "$target") || die "stable controller projection is dangling: $asset"
  monday_file_direct "$resolved" || die "stable controller projection is not a file: $asset"
  [[ $(monday_sha256_file "$resolved") == "$(monday_sha256_file "$target_release/deployment/$asset")" ]] \
    || die "active controller projection differs from target: $asset"
done

recovery_scheduler_state='{}'
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  for recovery_timer in $(monday_rust_lob_recovery_timer_units); do
    recovery_market=${recovery_timer#binance-lob-archiver-recovery@}
    recovery_market=${recovery_market%.timer}
    recovery_scheduler_state=$(jq -cn --argjson values "$recovery_scheduler_state" \
      --arg market "$recovery_market" --arg timer "$recovery_timer" \
      '$values + {($market):{unit:$timer,active:true,enabled:true}}')
  done
fi

receipt_root=$(monday_root_join "$ROOT" data/monday/evidence/cutovers)
mkdir -p "$receipt_root/$TO"; receipt="$receipt_root/$TO/transition.json"
[[ ! -e $receipt && ! -L $receipt ]] || die 'transition receipt already exists for target controller'
completed_at_ns=$(date +%s%N)
[[ $completed_at_ns =~ ^[0-9]+$ ]] || die 'cutover completion timestamp is unavailable'
completed_at=$(monday_epoch_ns_rfc3339 "$completed_at_ns") || die 'cutover completion timestamp is invalid'
before_assets='{}'; installed_assets='{}'; installed_projections='{}'; installed_controller_projections='{}'
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
for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
  target=${controller_projection_target[$asset]}
  installed_controller_projections=$(jq -cn --argjson values "$installed_controller_projections" \
    --arg asset "$asset" --arg target "/opt/monday/releases/binance-lob-controller/active/deployment/$asset" \
    --arg sha "$(monday_sha256_file "$(readlink -f -- "$target")")" \
    '$values + {($asset):{target:$target,sha256:$sha}}')
done
gate_evidence=$(jq -cS '{candidate_control_bytes,resource_admission,io_full_psi_windows,shadow_staging,checks,markets}' "$GATE")
transition_tmp="$receipt.tmp.$$"
source_mode=stable
[[ $FROM == direct ]] && source_mode=direct
  jq -cS -n --arg from "$before_controller" --arg source_mode "$source_mode" --arg to "$TO" --arg payload "$target_payload" --arg runtime "$target_runtime" \
  --arg before_payload "$before_payload" --arg before_runtime "$before_runtime" --arg gate "$GATE" --arg gate_sha "$GATE_SHA" \
  --arg completed "$completed_at" --argjson completed_ns "$completed_at_ns" --arg stable "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" \
  --arg projection "$stable_projection" --argjson evidence "$gate_evidence" \
  --argjson production_runtime "$gate_production_runtime" --argjson production_process "$production_process" \
  --argjson recovery_schedulers "$recovery_scheduler_state" \
  --argjson before_assets "$before_assets" \
  --argjson installed_assets "$installed_assets" --argjson installed_projections "$installed_projections" \
  --argjson installed_controller_projections "$installed_controller_projections" \
  --argjson test_only "$TEST_ONLY" --argjson eligible "$( [[ $TEST_ONLY == true ]] && printf false || printf true )" \
  '{schema:"monday.rust_lob_pair_transition.v2",control_plane_version:2,operation:"cutover",
    test_only:$test_only,production_eligible:$eligible,source_mode:$source_mode,from_source_mode:$source_mode,
    from_controller_sha256:$from,controller_sha256:$to,
    payload_sha256:$payload,runtime_contract_sha256:$runtime,gate_receipt:$gate,gate_sha256:$gate_sha,
    production_runtime:$production_runtime,production_process:$production_process,
    recovery_schedulers:$recovery_schedulers,
    gate_evidence:$evidence,active_pair_committed:true,completed_at:$completed,completed_at_ns:$completed_ns,
    stable_production_projection:$stable,production_projection:$projection,
    before:{controller:$from,payload_sha256:$before_payload,runtime_contract_sha256:$before_runtime,
      production_projection:$stable,assets:$before_assets},
    installed_assets:$installed_assets,installed_projections:$installed_projections,
    installed_controller_projections:$installed_controller_projections,result:"success"}' >"$transition_tmp"
chmod 0640 "$transition_tmp"; mv -f -- "$transition_tmp" "$receipt"
receipt_sha=$(monday_sha256_file "$receipt"); printf '%s  transition.json\n' "$receipt_sha" >"$receipt.sha256"; chmod 0440 "$receipt.sha256"
trap - EXIT
printf 'Pair cutover complete\nTransition receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
