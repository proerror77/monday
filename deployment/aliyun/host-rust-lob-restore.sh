#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() { printf '%s\n' "Usage: ${0##*/} --controller <active-sha> [--root <path>]" >&2; }
die() { printf '%s\n' "pair restore failed: $*" >&2; exit 1; }
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
readonly PRODUCTION_SLICE='system-binance\x2dlob\x2darchiver\x2dproduction.slice'
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true
RESTORE_HEALTH_WAIT_SECONDS=60
if [[ $TEST_ONLY == true && -n ${MONDAY_RESTORE_HEALTH_TIMEOUT_SECONDS:-} ]]; then
  RESTORE_HEALTH_WAIT_SECONDS=$MONDAY_RESTORE_HEALTH_TIMEOUT_SECONDS
fi
[[ $RESTORE_HEALTH_WAIT_SECONDS =~ ^[1-9][0-9]*$ && $RESTORE_HEALTH_WAIT_SECONDS -le 900 ]] \
  || die 'restore health timeout must be 1..900 seconds'
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
monday_control_plane_validate_mode "$ROOT" "$TEST_ONLY" \
  || die 'production uses canonical root or fixture mode lacks an explicit sentinel'

controller_root=$(monday_root_join "$ROOT" opt/monday/releases/binance-lob-controller)
lock_root=$(monday_root_join "$ROOT" run/lock); mkdir -p "$lock_root"
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

FIXTURE_SYSTEMD=false
if [[ $TEST_ONLY == true && ${MONDAY_RESTORE_FIXTURE_SYSTEMD:-0} == 1 ]]; then
  FIXTURE_SYSTEMD=true
  declare -A fixture_unit_state=() fixture_unit_file_state=() fixture_unit_load_state=()
  fixture_calls=$(monday_root_join "$ROOT" run/restore-fixture.calls)
  mkdir -p "$(dirname -- "$fixture_calls")"
  # A second idempotent invocation is a new shell process, so model the
  # already-successful pair from its immutable receipt instead of treating
  # an empty fixture systemd map as live state.  This is fixture plumbing
  # only; production always queries the real units below.
  fixture_restore_receipt=$(monday_root_join "$ROOT" "data/monday/evidence/restores/$CONTROLLER/restore.json")
  fixture_pid_from_receipt=
  if [[ -f $fixture_restore_receipt && ! -L $fixture_restore_receipt ]]; then
    fixture_pid_from_receipt=$(jq -r '.process_identity.spot.main_pid // empty' "$fixture_restore_receipt" 2>/dev/null || true)
    for fixture_market in spot usdm; do
      fixture_unit="binance-lob-archiver-production@${fixture_market}.service"
      fixture_unit_state[$fixture_unit]=active
      fixture_unit_file_state[$fixture_unit]=enabled
      fixture_unit_load_state[$fixture_unit]=loaded
    done
    while IFS= read -r fixture_unit; do
      fixture_unit_state[$fixture_unit]=inactive
      fixture_unit_file_state[$fixture_unit]=masked-runtime
      fixture_unit_load_state[$fixture_unit]=masked
    done < <(monday_rust_lob_legacy_writer_units)
    while IFS= read -r fixture_unit; do
      fixture_unit_state[$fixture_unit]=active
      fixture_unit_file_state[$fixture_unit]=enabled
      fixture_unit_load_state[$fixture_unit]=loaded
    done < <(monday_rust_lob_recovery_timer_units)
  fi
  systemctl() {
    local action=${1:-} unit=${2:-} argument
    case "$action" in
      start)
        if [[ ${MONDAY_RESTORE_FIXTURE_FAIL_USDM:-0} == 1 && $unit == *'@usdm.service' ]]; then
          printf 'start %s\n' "$unit" >>"$fixture_calls"
          return 1
        fi
        fixture_unit_state[$unit]=active
        [[ -n ${fixture_unit_file_state[$unit]:-} ]] || fixture_unit_file_state[$unit]=enabled
        fixture_unit_load_state[$unit]=loaded
        if [[ $unit == binance-lob-archiver-production@spot.service ]]; then
          : >"$(monday_root_join "$ROOT" run/restore-fixture-start-spot)"
        elif [[ $unit == binance-lob-archiver-production@usdm.service ]]; then
          : >"$(monday_root_join "$ROOT" run/restore-fixture-start-usdm)"
        fi
        printf 'start %s\n' "$unit" >>"$fixture_calls"; return 0 ;;
      stop|disable|mask|unmask|enable)
        shift
        for argument in "$@"; do
          [[ $argument == -* ]] && continue
          [[ $action == stop || $action == disable || $action == mask ]] && fixture_unit_state[$argument]=inactive
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
          if [[ ${MONDAY_RESTORE_FIXTURE_BAD_CONFIG:-0} == 1 ]]; then
            printf 'ControlGroup=/system.slice/wrong-production.slice\nMemoryMax=3758096385\nMemoryHigh=3221225472\n'
          else
            printf 'ControlGroup=/system.slice/system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice\nMemoryMax=3758096384\nMemoryHigh=3221225472\n'
          fi
          return 0
        fi
        if [[ $property == 'Slice,ControlGroup,MemoryMax' && $unit == binance-lob-archiver-production@* ]]; then
          printf 'verify-membership %s\n' "$unit" >>"$fixture_calls"
          market=${unit#*@}; market=${market%.service}
          if [[ ${MONDAY_RESTORE_FIXTURE_BAD_MEMBERSHIP:-0} == 1 && $market == spot ]]; then
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
          MainPID) printf '%s\n' "${MONDAY_RESTORE_FIXTURE_PID:-${fixture_pid_from_receipt:-$$}}" ;;
          NRestarts) printf '0\n' ;;
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

receipt_root=$(monday_root_join "$ROOT" data/monday/evidence/restores)
restore_receipt="$receipt_root/$CONTROLLER/restore.json"
restore_receipt_sha="$restore_receipt.sha256"
restore_receipt_tmp="$restore_receipt.tmp.$$"
restore_receipt_sha_tmp="$restore_receipt_sha.tmp.$$"
restore_receipt_written=0
restore_receipt_sha_written=0
success=false; readonly_idempotency_check=false; resume_existing_restore=false
cleanup() {
  local status=$? evidence_cleanup_failed=false; set +e
  if [[ $success != true && $status != 0 ]]; then
    if (( restore_receipt_sha_written == 1 )); then
      rm -f -- "$restore_receipt_sha" || evidence_cleanup_failed=true
      if [[ $evidence_cleanup_failed == false ]]; then
        sync -f "$receipt_root/$CONTROLLER" || evidence_cleanup_failed=true
      fi
      if [[ $evidence_cleanup_failed == false \
        && ${MONDAY_RESTORE_HARD_CRASH_AFTER_DIGEST_CLEANUP:-0} == 1 ]]; then
        kill -KILL "$$"
      fi
    fi
    if (( restore_receipt_written == 1 )) && [[ $evidence_cleanup_failed == false ]]; then
      rm -f -- "$restore_receipt" || evidence_cleanup_failed=true
      if [[ $evidence_cleanup_failed == false ]]; then
        sync -f "$receipt_root/$CONTROLLER" || evidence_cleanup_failed=true
      fi
    fi
  fi
  rm -f -- "$restore_receipt_tmp" "$restore_receipt_sha_tmp"
  if [[ $success != true && $status != 0 \
    && $readonly_idempotency_check != true \
    && ($TEST_ONLY == false || $FIXTURE_SYSTEMD == true) ]]; then
    # Restore is fail-closed even when a preflight fails before the normal
    # containment boundary: no legacy, shadow, upload, or production writer
    # may continue against a pair whose identity was not proven.
    monday_rust_lob_contain_writers >/dev/null 2>&1 || true
    monday_rust_lob_verify_contained >/dev/null 2>&1 || true
    monday_rust_lob_contain_recovery_schedulers >/dev/null 2>&1 || true
    monday_rust_lob_verify_recovery_schedulers_contained >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 143' HUP INT TERM

active=$(monday_active_controller_sha "$ROOT") || die 'active controller link is invalid'
[[ $active == "$CONTROLLER" ]] || die 'restore target is not the active controller'
monday_verify_controller_release "$ROOT" "$CONTROLLER" || die 'active controller failed verification'
release="$controller_root/$CONTROLLER"; manifest="$release/release.json"
payload=$(monday_manifest_field "$manifest" artifact_sha256)
runtime=$(monday_manifest_field "$manifest" runtime_contract_sha256)
binary=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver")
[[ -f $binary && ! -L $binary && $(monday_sha256_file "$binary") == "$payload" ]] \
  || die 'active payload is missing or has the wrong digest'

# Restore is anchored only in the active immutable ControllerRelease.  Verify
# the exact production contract and controller projections before touching a
# unit or link; the active C remains sufficient after a crash before any
# transition receipt was written.
production_runtime=$(monday_verify_production_runtime_assets \
  "$ROOT" "$release/deployment" "$payload") \
  || die 'active production runtime contract failed verification'

production=$(monday_root_join "$ROOT" opt/monday/bin/binance-lob-archiver)

process_started_at_ns() {
  local pid=$1 fixture_started_ns
  if [[ $TEST_ONLY == true && $FIXTURE_SYSTEMD == true ]]; then
    fixture_started_ns=${MONDAY_RESTORE_FIXTURE_PROCESS_STARTED_NS:-1}
    [[ $fixture_started_ns =~ ^[1-9][0-9]*$ ]] || return 1
    printf '%s\n' "$fixture_started_ns"
    return 0
  fi
  monday_process_started_at_ns "$ROOT" "$pid"
}

verify_existing_restore_state() {
  local receipt=$1 expected_runtime=$2 receipt_only=${3:-false} stable_binary expected resolved asset target
  local named_transition named_gate named_gate_sha receipt_gate receipt_gate_sha transition_from transition_mode transition_validator_from
  local receipt_process receipt_health market unit pid restarts exe env_file spool health dataset minimum_symbols
  local expected_exe expected_session expected_observed current_session updated now_ns process_started_ns
  [[ $receipt_only == true || $receipt_only == false ]] || die 'restore receipt verification mode is invalid'

  # The receipt is only an idempotency key after its complete read-only state
  # has been proven.  In particular, a stale success file never authorizes a
  # new containment, projection, or systemd mutation.
  jq -e --argjson require_runtime "$expected_runtime" '
    (.recovery_schedulers | type == "object")
    and (if $require_runtime then
      (.recovery_schedulers
       | ((keys | sort) == ["spot", "usdm"]
          and all(.[]; .active == true and .enabled == true
            and (.unit | type == "string" and test("^binance-lob-archiver-recovery@(spot|usdm)\\.timer$")))))
    else true end)
  ' "$receipt" >/dev/null || die 'existing restore receipt has invalid recovery scheduler evidence'

  # If the restore receipt names a transition, validate that exact immutable
  # transition and its Gate before looking at runtime state.  Do not scan or
  # guess an unrelated historical receipt.
  named_transition=$(jq -r '.transition_receipt // empty' "$receipt") || die 'existing restore receipt transition field is malformed'
  if [[ -n $named_transition ]]; then
    [[ $named_transition == "$(monday_root_join "$ROOT" "data/monday/evidence/cutovers/$CONTROLLER/transition.json")" ]] \
      || die 'existing restore transition path is not the active pair receipt'
    monday_file_direct "$named_transition" || die 'existing restore transition receipt is indirect'
    transition_from=$(jq -er '.from_controller_sha256' "$named_transition") \
      || die 'existing restore transition has no before controller'
    transition_mode=$(jq -er '.from_source_mode' "$named_transition") \
      || die 'existing restore transition has no source mode'
    case "$transition_mode" in
      direct) transition_validator_from=direct ;;
      stable) transition_validator_from=$transition_from ;;
      *) die 'existing restore transition has an invalid source mode' ;;
    esac
    named_gate=$(jq -er '.gate_receipt' "$named_transition") \
      || die 'existing restore transition has no Gate path'
    named_gate_sha=$(jq -er '.gate_sha256' "$named_transition") \
      || die 'existing restore transition has no Gate digest'
    receipt_gate=$(jq -r '.gate_receipt // empty' "$receipt") \
      || die 'existing restore Gate field is malformed'
    receipt_gate_sha=$(jq -r '.gate_sha256 // empty' "$receipt") \
      || die 'existing restore Gate digest field is malformed'
    [[ $receipt_gate == "$named_gate" && $receipt_gate_sha == "$named_gate_sha" ]] \
      || die 'existing restore Gate identity differs from its transition'
    monday_validate_v2_transition "$ROOT" "$named_transition" "$transition_validator_from" "$CONTROLLER" \
      "$named_gate" "$named_gate_sha" \
      || die 'existing restore transition failed exact Gate-chain validation'
  else
    receipt_gate=$(jq -r '.gate_receipt // empty' "$receipt") \
      || die 'existing restore Gate field is malformed'
    receipt_gate_sha=$(jq -r '.gate_sha256 // empty' "$receipt") \
      || die 'existing restore Gate digest field is malformed'
    if [[ -n $receipt_gate || -n $receipt_gate_sha ]]; then
      [[ -n $receipt_gate && -n $receipt_gate_sha ]] \
        || die 'existing restore recovery Gate identity is incomplete'
      monday_validate_v2_gate_authoritative "$ROOT" "$receipt_gate" direct \
        "$CONTROLLER" "$receipt_gate_sha" \
        || die 'existing restore recovery Gate is not authoritative'
      jq -e --arg payload "$payload" --arg runtime "$runtime" \
        --argjson test_only "$TEST_ONLY" '
          .candidate_payload_sha256 == $payload
          and .candidate_runtime_contract_sha256 == $runtime
          and .source_mode == "direct"
          and .test_only == $test_only
          and .production_eligible == ($test_only | not)
        ' "$receipt_gate" >/dev/null \
        || die 'existing restore recovery Gate differs from the active pair'
    fi
  fi

  receipt_process=$(jq -ce '.process_identity' "$receipt") \
    || die 'existing restore process identity evidence is malformed'
  receipt_health=$(jq -ce '.health' "$receipt") \
    || die 'existing restore health evidence is malformed'
  if [[ $expected_runtime == false ]]; then
    jq -e '
      .production_enabled == false
      and (.process_identity | type == "object" and length == 0)
      and (.health | type == "object" and length == 0)
    ' "$receipt" >/dev/null || die 'non-runtime restore receipt claims live production'
  else
    jq -e '.process_identity | type == "object" and (keys | sort) == ["spot", "usdm"]' \
      "$receipt" >/dev/null || die 'existing restore process identity is incomplete'
    jq -e '.health | type == "object" and (keys | sort) == ["spot", "usdm"]' \
      "$receipt" >/dev/null || die 'existing restore health identity is incomplete'
    for market in spot usdm; do
      minimum_symbols=1000; [[ $market == usdm ]] && minimum_symbols=100
      jq -e --arg market "$market" --arg payload "$payload" \
        --argjson minimum_symbols "$minimum_symbols" '
        .process_identity[$market] as $process
        | .health[$market] as $health
        | ($process | keys | sort) == [
            "active", "main_pid", "n_restarts", "observed_at_ns",
            "process_exe_sha256", "session_id"
          ]
          and ($health | keys | sort) == [
            "observed_at_ns", "sequence_gaps", "session_id", "status", "symbol_count"
          ]
          and ($process.main_pid | type == "number" and . > 0)
          and $process.process_exe_sha256 == $payload
          and $process.n_restarts == 0 and $process.active == true
          and ($process.session_id | type == "string" and length > 0)
          and ($process.observed_at_ns | type == "number" and . > 0)
          and $health.session_id == $process.session_id
          and $health.observed_at_ns == $process.observed_at_ns
          and $health.status == "synced" and $health.sequence_gaps == 0
          and ($health.symbol_count | type == "number" and . >= $minimum_symbols)
      ' "$receipt" >/dev/null || die "existing restore receipt runtime evidence is invalid: $market"
    done
  fi
  [[ $receipt_only == false ]] || return 0

  stable_binary="$controller_root/active/binance-lob-archiver"
  [[ -L $production && $(readlink -- "$production") == "$stable_binary" \
    && $(readlink -f -- "$production") == "$binary" ]] \
    || die 'existing restore stable production projection drifted'
  [[ $(monday_sha256_file "$binary") == "$payload" ]] || die 'existing restore active payload digest drifted'

  # Every runtime and controller projection must still be a direct symlink to
  # the active C.  A regular file, indirect link, or byte drift is a hard
  # failure, never repaired by an idempotent invocation.
  while IFS= read -r asset; do
    target=$(monday_runtime_asset_target "$ROOT" "$asset") \
      || die "existing restore runtime asset path is invalid: $asset"
    expected="$controller_root/active/deployment/$asset"
    [[ -L $target && $(readlink -- "$target") == "$expected" ]] \
      || die "existing restore runtime projection drifted: $asset"
    resolved=$(readlink -f -- "$target") || die "existing restore runtime projection is dangling: $asset"
    monday_file_direct "$resolved" || die "existing restore runtime projection is indirect: $asset"
    cmp -s "$resolved" "$release/deployment/$asset" \
      || die "existing restore runtime projection bytes drifted: $asset"
  done < <(monday_runtime_assets)
  while IFS= read -r asset; do
    target=$(monday_controller_projection_target "$ROOT" "$asset") \
      || die "existing restore controller projection path is invalid: $asset"
    expected="$controller_root/active/deployment/$asset"
    [[ -L $target && $(readlink -- "$target") == "$expected" ]] \
      || die "existing restore controller projection drifted: $asset"
    resolved=$(readlink -f -- "$target") || die "existing restore controller projection is dangling: $asset"
    monday_file_direct "$resolved" || die "existing restore controller projection is indirect: $asset"
    cmp -s "$resolved" "$release/deployment/$asset" \
      || die "existing restore controller projection bytes drifted: $asset"
  done < <(monday_controller_projection_assets)

  # A test-only restore without a fixture systemd view deliberately does not
  # claim live production.  Its immutable evidence and projections were
  # checked above; a production or systemd fixture receipt must also pass the
  # complete live unit, timer, process, and health contract below.
  if [[ $expected_runtime == false ]]; then
    return 0
  fi

  monday_rust_lob_verify_legacy_contained \
    || die 'existing restore legacy writers are not contained'
  monday_rust_lob_verify_recovery_schedulers_active \
    || die 'existing restore recovery timers are not active and enabled'
  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    systemctl is-active --quiet "$unit" || die "existing restore production unit is inactive: $market"
    [[ $(systemctl show "$unit" --property=SubState --value) == running ]] \
      || die "existing restore production unit is not running: $market"
    [[ $(systemctl show "$unit" --property=UnitFileState --value) == enabled ]] \
      || die "existing restore production unit is not enabled: $market"
    pid=$(systemctl show "$unit" --property=MainPID --value)
    restarts=$(systemctl show "$unit" --property=NRestarts --value)
    expected_exe=$(jq -er --arg market "$market" '.[$market].process_exe_sha256' <<<"$receipt_process") \
      || die "existing restore process executable evidence is missing: $market"
    [[ $pid =~ ^[1-9][0-9]*$ && $restarts == 0 ]] \
      || die "existing restore process identity is invalid: $market"
    exe=$(readlink -f -- "$(monday_root_join "$ROOT" "proc/$pid/exe")") \
      || die "existing restore process executable is unavailable: $market"
    [[ $exe == "$binary" && $(monday_sha256_file "$exe") == "$expected_exe" \
      && $expected_exe == "$payload" ]] \
      || die "existing restore process executable changed: $market"
    process_started_ns=$(process_started_at_ns "$pid") \
      || die "existing restore process start time is unavailable: $market"

    env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env") \
      || die "existing restore environment path is invalid: $market"
    spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file" | head -n1)
    [[ $spool == "/data/monday/spool/binance-lob/$market" ]] \
      || die "existing restore spool is non-canonical: $market"
    [[ $ROOT == / ]] || spool="$ROOT$spool"
    health="$spool/health.json"
    [[ -f $health && ! -L $health ]] || die "existing restore health is missing: $market"
    expected_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$receipt_process") \
      || die "existing restore process session evidence is missing: $market"
    expected_observed=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$receipt_process") \
      || die "existing restore process timestamp evidence is missing: $market"
    expected_health_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$receipt_health") \
      || die "existing restore health session evidence is missing: $market"
    expected_health_observed=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$receipt_health") \
      || die "existing restore health timestamp evidence is missing: $market"
    [[ $expected_session == "$expected_health_session" && $expected_observed == "$expected_health_observed" ]] \
      || die "existing restore process/health evidence does not agree: $market"
    current_session=$(jq -er '.session_id // empty' "$health") \
      || die "existing restore health session is missing: $market"
    updated=$(jq -er '.updated_at_ns // 0' "$health") \
      || die "existing restore health timestamp is missing: $market"
    now_ns=$(date +%s%N)
    [[ -n $current_session && $updated =~ ^[0-9]+$ \
      && $expected_observed =~ ^[0-9]+$ && $updated -ge $expected_observed \
      && $updated -ge $process_started_ns \
      && $updated -le $now_ns ]] \
      || die "existing restore health is stale or in the future: $market"
    # Receipt PID/session values are completion-time evidence.  Current health
    # is instead bound to the exact live executable and this PID's proc start.
    dataset=$(sed -n 's/^DATASET=//p' "$env_file" | head -n1)
    minimum_symbols=1000; [[ $market == usdm ]] && minimum_symbols=100
    monday_verify_rust_lob_runtime_health \
      "$release/deployment/rust-lob-runtime-health-policy.jq" "$health" "$market" \
      "$dataset" "$minimum_symbols" "$((expected_observed - 1))" \
      || die "existing restore health policy failed: $market"
  done
}

# If this active pair already has a transition receipt, validate only the Gate
# path and digest named by that receipt.  A crash before the receipt exists is
# recovered from the exact pre-commit intent below; no Gate/history scan is
# performed.
active_transition_receipt=$(monday_root_join "$ROOT" "data/monday/evidence/cutovers/$CONTROLLER/transition.json")
active_transition_marker="$active_transition_receipt.sha256"
transition_receipt_ref=; transition_gate=; transition_gate_sha=
recovery_transition_marker_repair=false
if [[ -e $active_transition_receipt || -L $active_transition_receipt ]]; then
  transition_receipt_ref=$active_transition_receipt
  monday_file_direct "$active_transition_receipt" || die 'active transition receipt is indirect'
  transition_from=$(jq -er '.from_controller_sha256' "$active_transition_receipt") \
    || die 'active transition receipt has no before controller'
  transition_mode=$(jq -er '.from_source_mode' "$active_transition_receipt") \
    || die 'active transition receipt has no source mode'
  case "$transition_mode" in
    direct) transition_validator_from=direct ;;
    stable) transition_validator_from=$transition_from ;;
    *) die 'active transition receipt has an invalid source mode' ;;
  esac
  transition_gate=$(jq -er '.gate_receipt' "$active_transition_receipt") \
    || die 'active transition receipt has no Gate path'
  transition_gate_sha=$(jq -er '.gate_sha256' "$active_transition_receipt") \
    || die 'active transition receipt has no Gate digest'
  canonical_gate_root=$(monday_root_join "$ROOT" "data/monday/evidence/shadow-gates/$CONTROLLER/$runtime")
  gate_relative=${transition_gate#"$canonical_gate_root"/}
  [[ $gate_relative =~ ^runs/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*/gate\.json$ ]] \
    || die 'active transition Gate path is outside the canonical V2 run path'
  monday_validate_v2_transition "$ROOT" "$active_transition_receipt" "$transition_validator_from" "$CONTROLLER" \
    "$transition_gate" "$transition_gate_sha" \
    || die 'active transition receipt failed its exact Gate-chain validation'
  if [[ $TEST_ONLY == false ]]; then
    jq -e '.test_only == false and .production_eligible == true' "$active_transition_receipt" >/dev/null \
      || die 'production restore requires an eligible transition receipt'
    gate_dir=$(dirname -- "$transition_gate"); marker="$gate_dir/PASSED.sha256"
    [[ -f $marker && ! -L $marker ]] || die 'active transition Gate PASSED marker is missing'
    marker_sha=$(awk '$2 == "gate.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' "$marker") \
      || die 'active transition Gate marker is malformed'
    [[ $marker_sha == "$transition_gate_sha" ]] || die 'active transition Gate marker digest differs'
  fi
fi

# A power loss can leave a direct bootstrap with active=C1 while one or more
# fixed projections still carry Gate-authorized C0/P0/R0 bytes.  The durable
# recovery intent names that one exact Gate; without it only active-C bytes are
# accepted.
cutover_root=$(monday_root_join "$ROOT" data/monday/evidence/cutovers)
recovery_intent="$cutover_root/$CONTROLLER/recovery.json"
recovery_intent_valid=false
recovery_gate=; recovery_gate_sha=; recovery_before_controller=
recovery_before_payload=; recovery_before_runtime=; recovery_before_projection=
if [[ -e $recovery_intent || -L $recovery_intent ]]; then
  monday_file_direct "$recovery_intent" || die 'cutover recovery intent is indirect'
  [[ $(monday_file_mode "$recovery_intent") == 440 ]] \
    || die 'cutover recovery intent mode is invalid'
  recovery_gate=$(jq -er '.gate_receipt' "$recovery_intent") \
    || die 'cutover recovery intent has no Gate path'
  recovery_gate_sha=$(jq -er '.gate_sha256' "$recovery_intent") \
    || die 'cutover recovery intent has no Gate digest'
  monday_validate_v2_gate_authoritative "$ROOT" "$recovery_gate" direct \
    "$CONTROLLER" "$recovery_gate_sha" \
    || die 'cutover recovery intent Gate is not authoritative'
  if [[ $TEST_ONLY == false ]]; then
    jq -e '.test_only == false and .production_eligible == true' "$recovery_gate" >/dev/null \
      || die 'production recovery requires an eligible Gate'
  else
    jq -e '.test_only == true and .production_eligible == false' "$recovery_gate" >/dev/null \
      || die 'fixture recovery requires a fixture Gate'
  fi
  recovery_before_controller=$(jq -er '.from_controller_sha256' "$recovery_gate") \
    || die 'cutover recovery Gate has no before controller'
  recovery_before_payload=$(jq -er '.before.payload_sha256' "$recovery_gate") \
    || die 'cutover recovery Gate has no before payload'
  recovery_before_runtime=$(jq -er '.before.runtime_contract_sha256' "$recovery_gate") \
    || die 'cutover recovery Gate has no before runtime'
  recovery_before_projection=$(jq -er '.before.production_projection' "$recovery_gate") \
    || die 'cutover recovery Gate has no before production projection'
  jq -e --arg controller "$CONTROLLER" --arg payload "$payload" --arg runtime "$runtime" \
    --arg from "$recovery_before_controller" --arg before_payload "$recovery_before_payload" \
    --arg before_runtime "$recovery_before_runtime" \
    --arg before_projection "$recovery_before_projection" \
    --arg gate "$recovery_gate" --arg gate_sha "$recovery_gate_sha" '
      (keys | sort) == [
        "before_payload_sha256", "before_production_projection",
        "before_runtime_contract_sha256", "control_plane_version",
        "controller_sha256", "from_controller_sha256", "from_source_mode",
        "gate_receipt", "gate_sha256", "operation", "payload_sha256",
        "runtime_contract_sha256", "schema"
      ]
      and .schema == "monday.rust_lob_pair_cutover_recovery.v1"
      and .control_plane_version == 2 and .operation == "cutover"
      and .from_source_mode == "direct"
      and .from_controller_sha256 == $from
      and .controller_sha256 == $controller
      and .before_payload_sha256 == $before_payload
      and .before_runtime_contract_sha256 == $before_runtime
      and .before_production_projection == $before_projection
      and .payload_sha256 == $payload
      and .runtime_contract_sha256 == $runtime
      and .gate_receipt == $gate and .gate_sha256 == $gate_sha
    ' "$recovery_intent" >/dev/null \
    || die 'cutover recovery intent differs from its exact Gate and active controller'
  if [[ -n $transition_receipt_ref ]]; then
    [[ $recovery_gate == "$transition_gate" && $recovery_gate_sha == "$transition_gate_sha" ]] \
      || die 'cutover recovery intent differs from the active transition'
  fi
  recovery_before_binary=$(monday_root_join "$ROOT" \
    "opt/monday/releases/binance-lob-archiver/$recovery_before_payload/binance-lob-archiver")
  monday_file_direct "$recovery_before_binary" \
    || die 'cutover recovery before payload is missing'
  [[ $(monday_sha256_file "$recovery_before_binary") == "$recovery_before_payload" ]] \
    || die 'cutover recovery before payload digest differs'
  if [[ -z $transition_receipt_ref ]]; then
    transition_gate=$recovery_gate
    transition_gate_sha=$recovery_gate_sha
  fi
  recovery_intent_valid=true
fi

if [[ -n $transition_receipt_ref ]]; then
  if [[ -e $active_transition_marker || -L $active_transition_marker ]]; then
    monday_file_direct "$active_transition_marker" \
      || die 'active transition digest is indirect'
    [[ $(monday_file_mode "$active_transition_marker") == 440 ]] \
      || die 'active transition digest mode is invalid'
    transition_marker_sha=$(awk '$2 == "transition.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
      "$active_transition_marker") || die 'active transition digest is malformed'
    [[ $transition_marker_sha == "$(monday_sha256_file "$active_transition_receipt")" ]] \
      || die 'active transition digest differs from its receipt'
  elif [[ $recovery_intent_valid == true ]]; then
    recovery_transition_marker_repair=true
  else
    die 'active transition digest is missing without a recovery intent'
  fi
elif [[ -e $active_transition_marker || -L $active_transition_marker ]]; then
  die 'active transition digest exists without its receipt'
fi

repair_recovery_transition_marker() {
  [[ $recovery_transition_marker_repair == true ]] || return 0
  local marker_tmp="$active_transition_marker.restore.$$" marker_sha
  [[ ! -e $marker_tmp && ! -L $marker_tmp \
    && ! -e $active_transition_marker && ! -L $active_transition_marker ]] \
    || return 1
  marker_sha=$(monday_sha256_file "$active_transition_receipt") || return 1
  printf '%s  transition.json\n' "$marker_sha" >"$marker_tmp" || return 1
  chmod 0440 "$marker_tmp" || return 1
  sync -f "$marker_tmp" || return 1
  mv -f -- "$marker_tmp" "$active_transition_marker" || return 1
  sync -f "$(dirname -- "$active_transition_marker")" || return 1
  monday_file_direct "$active_transition_marker" || return 1
  [[ $(monday_file_mode "$active_transition_marker") == 440 ]] || return 1
  [[ $(awk '$2 == "transition.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
    "$active_transition_marker") == "$marker_sha" ]] || return 1
  recovery_transition_marker_repair=false
}
clear_recovery_intent() {
  rm -f -- "$recovery_intent" || return 1
  sync -f "$(dirname -- "$recovery_intent")" || return 1
  [[ ! -e $recovery_intent && ! -L $recovery_intent ]]
}

# Idempotency is decided before containment or any projection/systemd write.
# A crash may leave the validated restore receipt committed before its digest;
# the still-present exact recovery intent authorizes repairing that marker and
# reconverging mutable runtime identity after a host reboot.
if [[ -e $restore_receipt || -L $restore_receipt || -e $restore_receipt_sha || -L $restore_receipt_sha ]]; then
  monday_file_direct "$restore_receipt" || die 'existing restore receipt is indirect or missing'
  [[ $(monday_file_mode "$restore_receipt") == 440 ]] \
    || die 'existing restore receipt mode is invalid'
  expected_runtime=true
  [[ $TEST_ONLY == true && $FIXTURE_SYSTEMD == false ]] && expected_runtime=false
  jq -e --arg controller "$CONTROLLER" --arg payload "$payload" --arg runtime "$runtime" \
    --argjson test_only "$TEST_ONLY" --argjson expected_runtime "$expected_runtime" \
    '.schema == "monday.rust_lob_pair_restore.v2"
     and .operation == "restore" and .result == "success"
     and .active_pair_converged == true and .production_enabled == $expected_runtime
     and .test_only == $test_only
     and .controller_sha256 == $controller
     and .payload_sha256 == $payload
     and .runtime_contract_sha256 == $runtime
     and (.completed_at | type == "string" and length > 0)' \
    "$restore_receipt" >/dev/null \
    || die 'existing restore receipt is not an exact successful active pair'
  receipt_digest=$(monday_sha256_file "$restore_receipt")
  if [[ -e $restore_receipt_sha || -L $restore_receipt_sha ]]; then
    monday_file_direct "$restore_receipt_sha" || die 'existing restore receipt digest is indirect'
    [[ $(monday_file_mode "$restore_receipt_sha") == 440 ]] \
      || die 'existing restore receipt digest mode is invalid'
    marker_digest=$(awk '$2 == "restore.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
      "$restore_receipt_sha") || die 'existing restore receipt digest is malformed'
    [[ $marker_digest == "$receipt_digest" ]] \
      || die 'existing restore receipt digest does not match'
  else
    [[ $recovery_intent_valid == true ]] \
      || die 'existing restore receipt digest is missing without a recovery intent'
    [[ ! -e $restore_receipt_sha_tmp && ! -L $restore_receipt_sha_tmp ]] \
      || die 'restore receipt digest repair path already exists'
    printf '%s  restore.json\n' "$receipt_digest" >"$restore_receipt_sha_tmp"
    chmod 0440 "$restore_receipt_sha_tmp"
    sync -f "$restore_receipt_sha_tmp" \
      || die 'could not durably flush repaired restore receipt digest'
    mv -f -- "$restore_receipt_sha_tmp" "$restore_receipt_sha"
    restore_receipt_sha_written=1
    sync -f "$receipt_root/$CONTROLLER" \
      || die 'could not durably commit repaired restore receipt digest'
    monday_file_direct "$restore_receipt_sha" \
      || die 'repaired restore receipt digest is not a direct file'
    [[ $(monday_file_mode "$restore_receipt_sha") == 440 \
      && $(awk '$2 == "restore.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
        "$restore_receipt_sha") == "$receipt_digest" ]] \
      || die 'repaired restore receipt digest failed readback'
  fi
  if [[ $recovery_intent_valid == true ]]; then
    jq -e --arg gate "$recovery_gate" --arg gate_sha "$recovery_gate_sha" \
      '.gate_receipt == $gate and .gate_sha256 == $gate_sha' \
      "$restore_receipt" >/dev/null \
      || die 'existing restore receipt differs from its recovery intent Gate'
    repair_recovery_transition_marker \
      || die 'could not repair the recovery transition digest'
    # The receipt and digest are immutable completion evidence, but their PID,
    # restart counter, and health session are observations rather than reboot-
    # stable identity.  Validate the frozen receipt, then run the normal active-C
    # convergence and fresh runtime readback before clearing recovery authority.
    verify_existing_restore_state "$restore_receipt" "$expected_runtime" true
    resume_existing_restore=true
  else
    readonly_idempotency_check=true
    verify_existing_restore_state "$restore_receipt" "$expected_runtime"
    success=true
    printf 'Pair restore already complete (read-only): %s\n' "$restore_receipt"
    exit 0
  fi
fi

for asset in $(monday_runtime_assets); do
  target=$(monday_runtime_asset_target "$ROOT" "$asset") || die "unknown runtime asset: $asset"
  expected="$controller_root/active/deployment/$asset"
  if [[ -L $target ]]; then
    [[ $(readlink -- "$target") == "$expected" ]] || die "runtime projection target drifted: $asset"
    resolved=$(readlink -f -- "$target") || die "runtime projection is dangling: $asset"
    monday_file_direct "$resolved" || die "runtime projection is indirect: $asset"
    cmp -s "$resolved" "$release/deployment/$asset" || die "runtime projection bytes drifted: $asset"
  elif [[ -f $target && ! -L $target ]]; then
    if cmp -s "$target" "$release/deployment/$asset"; then
      :
    elif [[ $recovery_intent_valid == true ]]; then
      before_sha=$(monday_v2_gate_before_asset_sha256 "$recovery_gate" "$asset") \
        || die "cutover recovery before asset is invalid: $asset"
      [[ -n $before_sha && $(monday_sha256_file "$target") == "$before_sha" ]] \
        || die "runtime asset bytes differ from active C and Gate R0: $asset"
    else
      die "runtime asset bytes drifted: $asset"
    fi
  elif [[ ! -e $target ]]; then
    : # The verified active controller is the sole source for a missing projection.
  else
    die "runtime projection is indirect: $asset"
  fi
done
for asset in $(monday_controller_projection_assets); do
  target=$(monday_controller_projection_target "$ROOT" "$asset") || die "unknown controller projection: $asset"
  expected="$controller_root/active/deployment/$asset"
  if [[ -L $target ]]; then
    [[ $(readlink -- "$target") == "$expected" ]] || die "controller projection target drifted: $asset"
    resolved=$(readlink -f -- "$target") || die "controller projection is dangling: $asset"
    monday_file_direct "$resolved" || die "controller projection is indirect: $asset"
    cmp -s "$resolved" "$release/deployment/$asset" || die "controller projection bytes drifted: $asset"
  elif [[ -f $target && ! -L $target ]]; then
    owner=$(monday_file_uid "$target") || die "controller projection owner is unavailable: $asset"
    mode=$(monday_file_mode "$target") || die "controller projection mode is unavailable: $asset"
    [[ $((8#$mode & 022)) == 0 && ($owner == 0 || $TEST_ONLY == true) ]] \
      || die "controller projection regular file is not a safe legacy projection: $asset"
  elif [[ ! -e $target ]]; then
    : # The active C is the only source allowed to repair this missing link.
  else
    die "controller projection is indirect: $asset"
  fi
done

mapfile -t PAIR_ASSETS < <(monday_runtime_assets)
readonly PAIR_ASSETS
mapfile -t CONTROLLER_PROJECTION_ASSETS < <(monday_controller_projection_assets)
readonly CONTROLLER_PROJECTION_ASSETS
projection="$controller_root/active"
stable_binary="$projection/binance-lob-archiver"
declare -A installed_projections installed_sha installed_controller_projections installed_controller_sha
declare -A before_health_session
writer_snapshot=$(monday_root_join "$ROOT" run/restore-writer-state.tsv)
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  monday_rust_lob_writer_state_snapshot >"$writer_snapshot" \
    || die 'could not snapshot canonical writer states'
fi
for market in spot usdm; do
  env_file="$release/deployment/binance-lob-archiver-production-$market.env"
  spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file" | head -n1)
  [[ $spool == "/data/monday/spool/binance-lob/$market" ]] \
    || die "production spool is not canonical: $market"
  [[ $ROOT == / ]] || spool="$ROOT$spool"
  health="$spool/health.json"
  if [[ -f $health && ! -L $health ]]; then
    before_health_session[$market]=$(jq -er '.session_id // empty' "$health" 2>/dev/null || true)
  fi
done
production=$(monday_root_join "$ROOT" opt/monday/bin/binance-lob-archiver)
production_repair_source=$binary
if [[ -L $production ]]; then
  production_target=$(readlink -- "$production") || die 'production projection target is unreadable'
  if [[ $production_target == "$stable_binary" ]]; then
    [[ $(readlink -f -- "$production") == "$binary" ]] \
      || die 'stable production projection differs from active payload'
  else
    production_resolved=$(readlink -f -- "$production") \
      || die 'production projection is dangling'
    if [[ $production_resolved == "$binary" \
      && $(monday_sha256_file "$production_resolved") == "$payload" ]]; then
      :
    elif [[ $recovery_intent_valid == true \
      && $production_target == "$recovery_before_projection" \
      && $production_resolved == "$recovery_before_binary" \
      && $(monday_sha256_file "$production_resolved") == "$recovery_before_payload" ]]; then
      monday_verify_legacy_controller_release "$ROOT" "$recovery_before_controller" "$production" >/dev/null \
        || die 'cutover recovery production does not match immutable legacy C0/P0'
      production_repair_source=$recovery_before_binary
    else
      die 'production projection target drifted'
    fi
  fi
elif [[ -f $production && ! -L $production ]]; then
  [[ $(monday_sha256_file "$production") == "$payload" ]] \
    || die 'production projection bytes drifted'
elif [[ ! -e $production ]]; then
  : # The verified active controller is the sole source for a missing projection.
else
  die 'production projection is indirect'
fi
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  monday_rust_lob_contain_writers \
    || die 'could not contain all canonical writers before restore'
  monday_rust_lob_verify_contained \
    || die 'canonical writers are not stopped, disabled, and runtime-masked'
  monday_rust_lob_contain_recovery_schedulers \
    || die 'could not contain recovery schedulers before restore'
  monday_rust_lob_verify_recovery_schedulers_contained \
    || die 'recovery schedulers are not stopped, disabled, and runtime-masked'
fi
ensure_projection() {
  local target=$1 expected=$2 source=${3:-} accepted_sha=${4:-} temporary="$1.restore.$$" resolved
  mkdir -p "$(dirname -- "$target")"
  if [[ -L $target && $(readlink -- "$target") == "$expected" ]]; then
    resolved=$(readlink -f -- "$target") || return 1
    [[ -f $resolved && ! -L $resolved ]] || return 1
    return 0
  fi
  if [[ -L $target ]]; then
    [[ -n $source && $(readlink -f -- "$target") == "$(readlink -f -- "$source")" ]] || return 1
    rm -f -- "$target" || return 1
  elif [[ -e $target ]]; then
    [[ -n $source && -f $target ]] || return 1
    cmp -s "$target" "$source" \
      || [[ -n $accepted_sha && $(monday_sha256_file "$target") == "$accepted_sha" ]] \
      || return 1
    rm -f -- "$target" || return 1
  fi
  rm -f -- "$temporary"
  ln -s "$expected" "$temporary"; mv -f -- "$temporary" "$target"
  [[ -L $target && $(readlink -- "$target") == "$expected" ]]
}
ensure_controller_projection() {
  local target=$1 expected=$2 temporary="$1.restore.$$" resolved owner mode
  mkdir -p "$(dirname -- "$target")"
  if [[ -L $target ]]; then
    [[ $(readlink -- "$target") == "$expected" ]] || return 1
    resolved=$(readlink -f -- "$target") || return 1
    [[ -f $resolved && ! -L $resolved ]] || return 1
    return 0
  fi
  if [[ -e $target ]]; then
    [[ -f $target && ! -L $target ]] || return 1
    owner=$(monday_file_uid "$target") || return 1
    mode=$(monday_file_mode "$target") || return 1
    [[ $((8#$mode & 022)) == 0 && ($owner == 0 || $TEST_ONLY == true) ]] || return 1
    rm -f -- "$target" || return 1
  fi
  rm -f -- "$temporary"
  ln -s "$expected" "$temporary" && mv -f -- "$temporary" "$target"
  [[ -L $target && $(readlink -- "$target") == "$expected" ]]
}
ensure_projection "$production" "$stable_binary" "$production_repair_source" \
  || die 'could not converge stable production projection'
[[ $(readlink -f -- "$production") == "$binary" ]] || die 'stable production projection differs from active payload'
for asset in "${PAIR_ASSETS[@]}"; do
  target=$(monday_runtime_asset_target "$ROOT" "$asset") || die "unknown runtime asset: $asset"
  expected="$projection/deployment/$asset"
  before_sha=
  if [[ $recovery_intent_valid == true ]]; then
    before_sha=$(monday_v2_gate_before_asset_sha256 "$recovery_gate" "$asset") \
      || die "cutover recovery before asset is invalid during repair: $asset"
  fi
  ensure_projection "$target" "$expected" "$release/deployment/$asset" "$before_sha" \
    || die "could not converge stable pair projection: $asset"
  resolved=$(readlink -f -- "$target") || die "stable pair projection is dangling: $asset"
  monday_file_direct "$resolved" || die "stable pair projection is not a file: $asset"
  [[ $(monday_sha256_file "$resolved") == "$(monday_sha256_file "$release/deployment/$asset")" ]] \
    || die "stable pair projection differs from active controller: $asset"
  installed_projections[$asset]=$expected; installed_sha[$asset]=$(monday_sha256_file "$resolved")
done
for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
  target=$(monday_controller_projection_target "$ROOT" "$asset") \
    || die "unknown controller projection: $asset"
  expected="$projection/deployment/$asset"
  ensure_controller_projection "$target" "$expected" \
    || die "could not converge controller projection: $asset"
  resolved=$(readlink -f -- "$target") \
    || die "controller projection is dangling: $asset"
  monday_file_direct "$resolved" \
    || die "controller projection is not a file: $asset"
  [[ $(monday_sha256_file "$resolved") == \
    "$(monday_sha256_file "$release/deployment/$asset")" ]] \
    || die "controller projection differs from active controller: $asset"
  installed_controller_projections[$asset]=$expected
  installed_controller_sha[$asset]=$(monday_sha256_file "$resolved")
done
# The repaired projections must converge to one exact runtime-contract digest
# before any lane is started; this is the post-mutation counterpart to the
# byte-level preflight above and prevents a partial link repair from running.
live_runtime=$(monday_rust_lob_live_runtime_contract_sha256 "$ROOT") \
  || die 'installed runtime contract is missing or indirect after repair'
[[ $live_runtime == "$runtime" ]] \
  || die 'installed runtime contract differs from active controller after repair'
if [[ $recovery_intent_valid == true ]]; then
  repair_recovery_transition_marker \
    || die 'could not repair the recovery transition digest'
fi

restore_started_ns=0
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  restore_started_ns=$(date +%s%N)
  systemctl daemon-reload || die 'daemon-reload failed'
  monday_rust_lob_verify_systemd_production_slice_configured "$ROOT" \
    || die 'permanent production slice verification failed before restore'
  systemctl unmask binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service \
    || die 'could not unmask V2 production lanes'
  systemctl start binance-lob-archiver-production@spot.service \
    || die 'Spot failed to start during restore'
  systemctl start binance-lob-archiver-production@usdm.service \
    || die 'USD-M failed to start during restore'
  monday_rust_lob_verify_systemd_production_membership "$ROOT" \
    || die 'production child membership is not exact after restore start'
fi

process_json='{}'; health_json='{}'; runtime_observed=false
verify_runtime_lane() {
  local market=$1 unit="binance-lob-archiver-production@${1}.service" pid restarts exe env_file spool health updated session now
  local dataset minimum_symbols old_session policy
  systemctl is-active --quiet "$unit" || return 1
  [[ $(systemctl show "$unit" --property=SubState --value) == running ]] || return 1
  pid=$(systemctl show "$unit" --property=MainPID --value); [[ $pid =~ ^[1-9][0-9]*$ ]] || return 1
  restarts=$(systemctl show "$unit" --property=NRestarts --value); [[ $restarts == 0 ]] || return 1
  exe=$(readlink -f -- "$(monday_root_join "$ROOT" "proc/$pid/exe")") || return 1
  [[ $exe == "$binary" && $(monday_sha256_file "$exe") == "$payload" ]] || return 1
  env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env") || return 1
  spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file" | head -n1)
  [[ $spool == "/data/monday/spool/binance-lob/$market" ]] || return 1
  [[ $ROOT == / ]] || spool="$ROOT$spool"
  health="$spool/health.json"
  policy="$release/deployment/rust-lob-runtime-health-policy.jq"
  dataset=$(sed -n 's/^DATASET=//p' "$env_file" | head -n1)
  minimum_symbols=1000; [[ $market == usdm ]] && minimum_symbols=100
  old_session=${before_health_session[$market]:-}
  if (( restore_started_ns > 0 )); then
    local deadline=$(( $(date +%s) + RESTORE_HEALTH_WAIT_SECONDS ))
    while :; do
      session=; updated=0
      if [[ -f $health && ! -L $health ]]; then
        session=$(jq -er '.session_id // empty' "$health" 2>/dev/null || true)
        updated=$(jq -er '.updated_at_ns // 0' "$health" 2>/dev/null || printf '0')
      fi
      if [[ -n $session && $updated =~ ^[0-9]+$ && $updated -ge $restore_started_ns \
        && ( -z $old_session || $session != "$old_session" ) ]] &&
        monday_verify_rust_lob_runtime_health "$policy" "$health" "$market" "$dataset" \
          "$minimum_symbols" "$((restore_started_ns - 1))"; then
        break
      fi
      now=$(date +%s)
      (( now < deadline )) || return 1
      sleep 1
    done
  else
    [[ -f $health && ! -L $health ]] || return 1
    session=$(jq -er '.session_id // empty' "$health") || return 1
    [[ -n $session && ( -z $old_session || $session != "$old_session" ) ]] || return 1
    updated=$(jq -er '.updated_at_ns // 0' "$health"); [[ $updated =~ ^[0-9]+$ && $updated -gt 0 ]] || return 1
    monday_verify_rust_lob_runtime_health "$policy" "$health" "$market" "$dataset" \
      "$minimum_symbols" 0 || return 1
  fi
  now=$(date +%s%N); [[ $updated -le $now ]] || return 1
  health_json=$(jq -cn --argjson values "$health_json" --arg market "$market" --arg session "$session" \
    --argjson observed "$updated" --arg status "$(jq -er '.status' "$health")" \
    --argjson gaps "$(jq -er '.sequence_gaps' "$health")" \
    --argjson symbols "$(jq -er '.symbol_count' "$health")" \
    '$values + {($market):{session_id:$session,observed_at_ns:$observed,status:$status,sequence_gaps:$gaps,symbol_count:$symbols}}')
  process_json=$(jq -cn --argjson values "$process_json" --arg market "$market" --argjson pid "$pid" \
    --arg sha "$(monday_sha256_file "$exe")" --argjson restarts "$restarts" --arg session "$session" \
    --argjson observed "$updated" '$values + {($market):{main_pid:$pid,process_exe_sha256:$sha,n_restarts:$restarts,active:true,session_id:$session,observed_at_ns:$observed}}')
}
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  verify_runtime_lane spot || die 'Spot runtime identity or fresh synchronized health failed'
  verify_runtime_lane usdm || die 'USD-M runtime identity or fresh synchronized health failed'
  systemctl enable binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service \
    || die 'could not enable V2 production lanes after restore'
  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    [[ $(systemctl show "$unit" --property=UnitFileState --value) == enabled ]] \
      || die "V2 production lane is not enabled after restore: $market"
  done
  monday_rust_lob_verify_legacy_contained \
    || die 'legacy canonical writers escaped the restore containment'
  monday_rust_lob_enable_recovery_schedulers \
    || die 'recovery schedulers did not become active and enabled after restore'
  # Take one final paired sample.  The receipt must bind the same fresh
  # session/timestamp that passed the active-C health policy for both lanes.
  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    systemctl is-active --quiet "$unit" || die "production lane changed after restore: $market"
    [[ $(systemctl show "$unit" --property=SubState --value) == running ]] \
      || die "production lane is not running after restore: $market"
    pid=$(systemctl show "$unit" --property=MainPID --value)
    restarts=$(systemctl show "$unit" --property=NRestarts --value)
    expected_pid=$(jq -er --arg market "$market" '.[$market].main_pid' <<<"$process_json")
    expected_restarts=$(jq -er --arg market "$market" '.[$market].n_restarts' <<<"$process_json")
    [[ $pid == "$expected_pid" && $restarts == "$expected_restarts" && $restarts == 0 ]] \
      || die "production process identity changed after restore: $market"
    env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env")
    spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file" | head -n1); [[ $ROOT == / ]] || spool="$ROOT$spool"
    health="$spool/health.json"
    session=$(jq -er '.session_id // empty' "$health") || die "production health session disappeared: $market"
    observed=$(jq -er '.updated_at_ns // 0' "$health")
    expected_session=$(jq -er --arg market "$market" '.[$market].session_id' <<<"$process_json")
    expected_observed=$(jq -er --arg market "$market" '.[$market].observed_at_ns' <<<"$process_json")
    [[ $session == "$expected_session" && $observed =~ ^[0-9]+$ \
      && $observed -ge $expected_observed && $observed -le $(date +%s%N) ]] \
      || die "production health changed after paired restore sample: $market"
    dataset=$(sed -n 's/^DATASET=//p' "$env_file" | head -n1)
    minimum_symbols=1000; [[ $market == usdm ]] && minimum_symbols=100
    monday_verify_rust_lob_runtime_health "$release/deployment/rust-lob-runtime-health-policy.jq" \
      "$health" "$market" "$dataset" "$minimum_symbols" "$((expected_observed - 1))" \
      || die "production health policy failed after paired restore sample: $market"
  done
  runtime_observed=true
fi

# Make the repaired pair durable on every persistent filesystem before its
# success receipt can become the replacement for the recovery intent.
sync -f "$(monday_root_join "$ROOT" opt/monday)" \
  || die 'could not durably commit restored /opt projections'
sync -f "$(monday_root_join "$ROOT" etc/monday)" \
  || die 'could not durably commit restored runtime projections'
sync -f "$(monday_root_join "$ROOT" etc/systemd/system)" \
  || die 'could not durably commit restored systemd projections'

if [[ $resume_existing_restore == true ]]; then
  [[ $runtime_observed == "$expected_runtime" ]] \
    || die 'recovered runtime state differs from the committed restore receipt mode'
  receipt_sha=$(monday_sha256_file "$restore_receipt")
  marker_sha=$(awk '$2 == "restore.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
    "$restore_receipt_sha") || die 'recovered restore receipt digest is malformed'
  [[ $marker_sha == "$receipt_sha" ]] \
    || die 'recovered restore receipt digest differs during final readback'
  verify_existing_restore_state "$restore_receipt" "$expected_runtime" true
  jq -e --arg gate "$recovery_gate" --arg gate_sha "$recovery_gate_sha" \
    '.gate_receipt == $gate and .gate_sha256 == $gate_sha' "$restore_receipt" >/dev/null \
    || die 'recovered restore receipt differs from its recovery intent Gate'
  success=true
  clear_recovery_intent || die 'could not clear committed restore recovery intent durably'
  recovery_intent_valid=false
  printf 'Pair restore recovery complete\nRestore receipt: %s\nSHA-256: %s\n' \
    "$restore_receipt" "$receipt_sha"
  exit 0
fi

mkdir -p "$receipt_root/$CONTROLLER"; receipt=$restore_receipt
[[ ! -e $receipt && ! -L $receipt \
  && ! -e $restore_receipt_sha && ! -L $restore_receipt_sha \
  && ! -e $restore_receipt_tmp && ! -L $restore_receipt_tmp \
  && ! -e $restore_receipt_sha_tmp && ! -L $restore_receipt_sha_tmp ]] \
  || die 'restore receipt already exists for this controller'
projections='{}'
for asset in "${PAIR_ASSETS[@]}"; do projections=$(jq -cn --argjson values "$projections" --arg asset "$asset" --arg target "${installed_projections[$asset]}" --arg sha "${installed_sha[$asset]}" '$values + {($asset):{target:$target,sha256:$sha}}'); done
controller_projections='{}'
for asset in "${CONTROLLER_PROJECTION_ASSETS[@]}"; do
  controller_projections=$(jq -cn --argjson values "$controller_projections" \
    --arg asset "$asset" --arg target "${installed_controller_projections[$asset]}" \
    --arg sha "${installed_controller_sha[$asset]}" \
    '$values + {($asset):{target:$target,sha256:$sha}}')
  done
legacy_containment='{}'
while IFS= read -r unit; do
  IFS=$'\t' read -r load active_state enabled_state < <(monday_rust_lob_writer_state "$unit") \
    || die "could not read legacy writer containment: $unit"
  legacy_containment=$(jq -cn --argjson values "$legacy_containment" --arg unit "$unit" \
    --arg load "$load" --arg active "$active_state" --arg enabled "$enabled_state" \
    '$values + {($unit):{load_state:$load,active_state:$active,unit_file_state:$enabled,contained:($active != "active" and ($enabled == "masked" or ($enabled | startswith("masked-"))))}}')
done < <(monday_rust_lob_legacy_writer_units)
recovery_scheduler_state='{}'
if [[ $runtime_observed == true ]]; then
  monday_rust_lob_verify_recovery_schedulers_active \
    || die 'could not read active recovery scheduler state for restore receipt'
  while IFS= read -r unit; do
    market=${unit#binance-lob-archiver-recovery@}; market=${market%.timer}
    active_state=$(systemctl show "$unit" --property=ActiveState --value)
    enabled_state=$(systemctl show "$unit" --property=UnitFileState --value)
    [[ $active_state == active && $enabled_state == enabled ]] \
      || die "recovery scheduler state changed before restore receipt: $market"
    recovery_scheduler_state=$(jq -cn --argjson values "$recovery_scheduler_state" \
      --arg market "$market" --arg unit "$unit" \
      '$values + {($market):{unit:$unit,active:true,enabled:true}}')
  done < <(monday_rust_lob_recovery_timer_units)
fi
tmp=$restore_receipt_tmp; completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
jq -cS -n --arg controller "$CONTROLLER" --arg payload "$payload" --arg runtime "$runtime" \
  --arg policy_sha "$(monday_sha256_file "$release/deployment/rust-lob-runtime-health-policy.jq")" --arg completed "$completed_at" \
  --arg transition_receipt "$transition_receipt_ref" --arg gate_receipt "$transition_gate" --arg gate_sha "$transition_gate_sha" \
  --arg projection "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" \
  --argjson test_only "$TEST_ONLY" --argjson eligible "$( [[ $TEST_ONLY == true ]] && printf false || printf true )" \
  --argjson processes "$process_json" --argjson health "$health_json" \
  --argjson projections "$projections" --argjson controller_projections "$controller_projections" \
  --argjson production_runtime "$production_runtime" --argjson legacy_containment "$legacy_containment" \
  --argjson recovery_schedulers "$recovery_scheduler_state" \
  --argjson production_enabled "$runtime_observed" \
  '{schema:"monday.rust_lob_pair_restore.v2",control_plane_version:2,operation:"restore",test_only:$test_only,production_eligible:$eligible,
    controller_sha256:$controller,payload_sha256:$payload,runtime_contract_sha256:$runtime,
    transition_receipt:(if $transition_receipt == "" then null else $transition_receipt end),
    gate_receipt:(if $gate_receipt == "" then null else $gate_receipt end),
    gate_sha256:(if $gate_sha == "" then null else $gate_sha end),
    runtime_health_policy_sha256:$policy_sha,stable_production_projection:$projection,
    active_pair_converged:true,installed_projections:$projections,controller_projections:$controller_projections,
    production_runtime:$production_runtime,legacy_writer_containment:$legacy_containment,
    recovery_schedulers:$recovery_schedulers,
    production_enabled:$production_enabled,process_identity:$processes,health:$health,
    completed_at:$completed,result:"success"}' >"$tmp"
chmod 0440 "$tmp"
verify_existing_restore_state "$tmp" "$runtime_observed"
receipt_sha=$(monday_sha256_file "$tmp")
printf '%s  restore.json\n' "$receipt_sha" >"$restore_receipt_sha_tmp"
chmod 0440 "$restore_receipt_sha_tmp"
sync -f "$tmp" || die 'could not durably flush restore receipt'
sync -f "$restore_receipt_sha_tmp" || die 'could not durably flush restore receipt digest'
mv -f -- "$tmp" "$receipt"
restore_receipt_written=1
sync -f "$receipt_root/$CONTROLLER" || die 'could not durably commit restore receipt'
mv -f -- "$restore_receipt_sha_tmp" "$restore_receipt_sha"
restore_receipt_sha_written=1
sync -f "$receipt_root/$CONTROLLER" || die 'could not durably commit restore receipt digest'
monday_file_direct "$receipt" || die 'committed restore receipt is not a direct file'
monday_file_direct "$restore_receipt_sha" || die 'committed restore receipt digest is not a direct file'
[[ $(monday_file_mode "$receipt") == 440 && $(monday_file_mode "$restore_receipt_sha") == 440 ]] \
  || die 'committed restore evidence mode is invalid'
[[ $(monday_sha256_file "$receipt") == "$receipt_sha" ]] \
  || die 'committed restore receipt digest changed during readback'
marker_sha=$(awk '$2 == "restore.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
  "$restore_receipt_sha") || die 'committed restore receipt digest is malformed'
[[ $marker_sha == "$receipt_sha" ]] || die 'committed restore receipt digest differs during readback'
verify_existing_restore_state "$receipt" "$runtime_observed"
if [[ $recovery_intent_valid == true ]]; then
  jq -e --arg gate "$recovery_gate" --arg gate_sha "$recovery_gate_sha" \
    '.gate_receipt == $gate and .gate_sha256 == $gate_sha' "$receipt" >/dev/null \
    || die 'committed restore receipt differs from its recovery intent Gate'
fi
if [[ ${MONDAY_RESTORE_HARD_CRASH_AFTER_RECEIPT:-0} == 1 ]]; then
  kill -KILL "$$"
fi
if [[ ${MONDAY_RESTORE_FAIL_AFTER_RECEIPT:-0} == 1 ]]; then
  die 'fault injection after restore receipt commit'
fi
success=true
if [[ $recovery_intent_valid == true ]]; then
  clear_recovery_intent || die 'could not clear committed restore recovery intent durably'
  recovery_intent_valid=false
fi
restore_receipt_written=0
restore_receipt_sha_written=0
printf 'Pair restore complete\nRestore receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
