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
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true
[[ $TEST_ONLY == false || $ROOT != / ]] || die 'test mode requires an isolated fixture root'
RESTORE_HEALTH_WAIT_SECONDS=60
if [[ $TEST_ONLY == true && -n ${MONDAY_RESTORE_HEALTH_TIMEOUT_SECONDS:-} ]]; then
  RESTORE_HEALTH_WAIT_SECONDS=$MONDAY_RESTORE_HEALTH_TIMEOUT_SECONDS
fi
[[ $RESTORE_HEALTH_WAIT_SECONDS =~ ^[1-9][0-9]*$ && $RESTORE_HEALTH_WAIT_SECONDS -le 900 ]] \
  || die 'restore health timeout must be 1..900 seconds'
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

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
        case "${3#--property=}" in
          LoadState) printf '%s\n' "${fixture_unit_load_state[$unit]:-loaded}" ;;
          ActiveState) [[ ${fixture_unit_state[$unit]:-inactive} == active ]] && printf 'active\n' || printf 'inactive\n' ;;
          SubState) [[ ${fixture_unit_state[$unit]:-inactive} == active ]] && printf 'running\n' || printf 'dead\n' ;;
          UnitFileState) printf '%s\n' "${fixture_unit_file_state[$unit]:-disabled}" ;;
          MainPID) printf '%s\n' "${MONDAY_RESTORE_FIXTURE_PID:-$$}" ;;
          NRestarts) printf '0\n' ;;
          *) printf '\n' ;;
        esac
        return 0 ;;
      is-enabled)
        printf '%s\n' "${fixture_unit_file_state[$2]:-disabled}"
        [[ ${fixture_unit_file_state[$2]:-disabled} == enabled ]] && return 0
        return 1 ;;
      *) return 0 ;;
    esac
  }
fi

success=false
cleanup() {
  local status=$?; set +e
  if [[ $success != true && $status != 0 \
    && ($TEST_ONLY == false || $FIXTURE_SYSTEMD == true) ]]; then
    # Restore is fail-closed even when a preflight fails before the normal
    # containment boundary: no legacy, shadow, upload, or production writer
    # may continue against a pair whose identity was not proven.
    monday_rust_lob_contain_writers >/dev/null 2>&1 || true
    monday_rust_lob_verify_contained >/dev/null 2>&1 || true
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

# If this active pair already has a transition receipt, validate only the Gate
# path and digest named by that receipt.  A crash before the receipt exists is
# intentionally recoverable from active C alone; no Gate/previous-state scan is
# performed.
active_transition_receipt=$(monday_root_join "$ROOT" "data/monday/evidence/cutovers/$CONTROLLER/transition.json")
if [[ -e $active_transition_receipt || -L $active_transition_receipt ]]; then
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
  monday_validate_v2_transition "$active_transition_receipt" "$transition_validator_from" "$CONTROLLER" \
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

# A power loss can leave a direct bootstrap with the active link committed but
# its fixed projections not yet renamed.  Accept only an exact active-C source
# (or a missing link) for that recovery case; any foreign bytes/target are a
# hard refusal, never overwritten.
for asset in $(monday_runtime_assets); do
  target=$(monday_runtime_asset_target "$ROOT" "$asset") || die "unknown runtime asset: $asset"
  expected="$controller_root/active/deployment/$asset"
  if [[ -L $target ]]; then
    [[ $(readlink -- "$target") == "$expected" ]] || die "runtime projection target drifted: $asset"
    resolved=$(readlink -f -- "$target") || die "runtime projection is dangling: $asset"
    monday_file_direct "$resolved" || die "runtime projection is indirect: $asset"
    cmp -s "$resolved" "$release/deployment/$asset" || die "runtime projection bytes drifted: $asset"
  elif [[ -f $target && ! -L $target ]]; then
    cmp -s "$target" "$release/deployment/$asset" || die "runtime asset bytes drifted: $asset"
  elif [[ ! -e $target ]]; then
    : # A direct bootstrap may have crashed before this projection was linked.
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
    cmp -s "$target" "$release/deployment/$asset" || die "controller projection bytes drifted: $asset"
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
  env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env") \
    || die "production environment path is invalid: $market"
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
if [[ -L $production ]]; then
  production_target=$(readlink -- "$production") || die 'production projection target is unreadable'
  if [[ $production_target == "$stable_binary" ]]; then
    [[ $(readlink -f -- "$production") == "$binary" ]] \
      || die 'stable production projection differs from active payload'
  else
    production_resolved=$(readlink -f -- "$production") \
      || die 'production projection is dangling'
    [[ $production_resolved == "$binary" && $(monday_sha256_file "$production_resolved") == "$payload" ]] \
      || die 'production projection target drifted'
  fi
elif [[ -f $production && ! -L $production ]]; then
  [[ $(monday_sha256_file "$production") == "$payload" ]] \
    || die 'production projection bytes drifted'
elif [[ ! -e $production ]]; then
  : # A direct bootstrap may have crashed before the stable link was linked.
else
  die 'production projection is indirect'
fi
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  monday_rust_lob_contain_writers \
    || die 'could not contain all canonical writers before restore'
  monday_rust_lob_verify_contained \
    || die 'canonical writers are not stopped, disabled, and runtime-masked'
fi
ensure_projection() {
  local target=$1 expected=$2 source=${3:-} temporary="$1.restore.$$" resolved
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
    cmp -s "$target" "$source" || return 1
    rm -f -- "$target" || return 1
  fi
  rm -f -- "$temporary"
  ln -s "$expected" "$temporary"; mv -f -- "$temporary" "$target"
  [[ -L $target && $(readlink -- "$target") == "$expected" ]]
}
ensure_projection "$production" "$stable_binary" "$binary" || die 'could not converge stable production projection'
[[ $(readlink -f -- "$production") == "$binary" ]] || die 'stable production projection differs from active payload'
for asset in "${PAIR_ASSETS[@]}"; do
  target=$(monday_runtime_asset_target "$ROOT" "$asset") || die "unknown runtime asset: $asset"
  expected="$projection/deployment/$asset"
  ensure_projection "$target" "$expected" "$release/deployment/$asset" \
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
  ensure_projection "$target" "$expected" "$release/deployment/$asset" \
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

restore_started_ns=0
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  restore_started_ns=$(date +%s%N)
  systemctl daemon-reload || die 'daemon-reload failed'
  systemctl unmask binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service \
    || die 'could not unmask V2 production lanes'
  systemctl start binance-lob-archiver-production@spot.service \
    || die 'Spot failed to start during restore'
  systemctl start binance-lob-archiver-production@usdm.service \
    || die 'USD-M failed to start during restore'
fi

process_json='{}'; health_json='{}'
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
fi

receipt_root=${MONDAY_RESTORE_RECEIPT_ROOT:-$(monday_root_join "$ROOT" data/monday/evidence/restores)}
mkdir -p "$receipt_root/$CONTROLLER"; receipt="$receipt_root/$CONTROLLER/restore.json"
[[ ! -e $receipt && ! -L $receipt ]] || die 'restore receipt already exists for this controller'
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
tmp="$receipt.tmp.$$"; completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
jq -cS -n --arg controller "$CONTROLLER" --arg payload "$payload" --arg runtime "$runtime" \
  --arg policy_sha "$(monday_sha256_file "$release/deployment/rust-lob-runtime-health-policy.jq")" --arg completed "$completed_at" \
  --arg projection "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" \
  --argjson test_only "$TEST_ONLY" --argjson eligible "$( [[ $TEST_ONLY == true ]] && printf false || printf true )" \
  --argjson processes "$process_json" --argjson health "$health_json" \
  --argjson projections "$projections" --argjson controller_projections "$controller_projections" \
  --argjson production_runtime "$production_runtime" --argjson legacy_containment "$legacy_containment" \
  '{schema:"monday.rust_lob_pair_restore.v2",control_plane_version:2,operation:"restore",test_only:$test_only,production_eligible:$eligible,
    controller_sha256:$controller,payload_sha256:$payload,runtime_contract_sha256:$runtime,
    runtime_health_policy_sha256:$policy_sha,stable_production_projection:$projection,
    active_pair_converged:true,installed_projections:$projections,controller_projections:$controller_projections,
    production_runtime:$production_runtime,legacy_writer_containment:$legacy_containment,
    production_enabled:true,process_identity:$processes,health:$health,
    completed_at:$completed,result:"success"}' >"$tmp"
chmod 0640 "$tmp"; mv -f -- "$tmp" "$receipt"; success=true
receipt_sha=$(monday_sha256_file "$receipt"); printf '%s  restore.json\n' "$receipt_sha" >"$receipt.sha256"; chmod 0440 "$receipt" "$receipt.sha256"
printf 'Pair restore complete\nRestore receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
