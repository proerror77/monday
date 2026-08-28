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
  declare -A fixture_unit_state=()
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
        fixture_unit_state[$unit]=active; printf 'start %s\n' "$unit" >>"$fixture_calls"; return 0 ;;
      stop|disable|mask|unmask)
        shift
        for argument in "$@"; do
          [[ $argument == -* ]] && continue
          [[ $action == stop ]] && fixture_unit_state[$argument]=inactive
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
          MainPID) printf '%s\n' "$$" ;;
          NRestarts) printf '0\n' ;;
          *) printf '\n' ;;
        esac
        return 0 ;;
      *) return 0 ;;
    esac
  }
fi

active=$(monday_active_controller_sha "$ROOT") || die 'active controller link is invalid'
[[ $active == "$CONTROLLER" ]] || die 'restore target is not the active controller'
monday_verify_controller_release "$ROOT" "$CONTROLLER" || die 'active controller failed verification'
release="$controller_root/$CONTROLLER"; manifest="$release/release.json"
payload=$(monday_manifest_field "$manifest" artifact_sha256)
binary=$(monday_root_join "$ROOT" "opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver")
[[ -f $binary && ! -L $binary && $(monday_sha256_file "$binary") == "$payload" ]] \
  || die 'active payload is missing or has the wrong digest'

mapfile -t PAIR_ASSETS < <(monday_runtime_assets)
readonly PAIR_ASSETS
mapfile -t CONTROLLER_PROJECTION_ASSETS < <(monday_controller_projection_assets)
readonly CONTROLLER_PROJECTION_ASSETS
projection="$controller_root/active"
stable_binary="$projection/binance-lob-archiver"
declare -A installed_projections installed_sha installed_controller_projections installed_controller_sha
success=false
contain() {
  [[ $TEST_ONLY == true && $FIXTURE_SYSTEMD == false ]] && return 0
  systemctl stop binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service >/dev/null 2>&1 || true
  systemctl disable --now binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service >/dev/null 2>&1 || true
  systemctl mask --runtime binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service >/dev/null 2>&1 || true
}
cleanup() {
  local status=$?; set +e
  if [[ $success != true && $status != 0 ]]; then contain; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 143' HUP INT TERM
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  systemctl stop binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service \
    || { contain; die 'could not stop production lanes'; }
fi
ensure_projection() {
  local target=$1 expected=$2 temporary="$1.restore.$$" resolved
  mkdir -p "$(dirname -- "$target")"
  if [[ -L $target && $(readlink -- "$target") == "$expected" ]]; then
    resolved=$(readlink -f -- "$target") || return 1
    [[ -f $resolved && ! -L $resolved ]] || return 1
    return 0
  fi
  # Restore repairs only the stable link topology.  It never copies mutable
  # bytes over a live file and never guesses a previous release.
  rm -f -- "$temporary" "$target"
  ln -s "$expected" "$temporary"; mv -f -- "$temporary" "$target"
  [[ -L $target && $(readlink -- "$target") == "$expected" ]]
}
production=$(monday_root_join "$ROOT" opt/monday/bin/binance-lob-archiver)
ensure_projection "$production" "$stable_binary" || die 'could not converge stable production projection'
[[ $(readlink -f -- "$production") == "$binary" ]] || die 'stable production projection differs from active payload'
for asset in "${PAIR_ASSETS[@]}"; do
  target=$(monday_runtime_asset_target "$ROOT" "$asset") || die "unknown runtime asset: $asset"
  expected="$projection/deployment/$asset"
  ensure_projection "$target" "$expected" || die "could not converge stable pair projection: $asset"
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
  ensure_projection "$target" "$expected" \
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

restore_started_ns=0
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  restore_started_ns=$(date +%s%N)
  systemctl daemon-reload || { contain; die 'daemon-reload failed'; }
  systemctl unmask binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service >/dev/null 2>&1 || true
  systemctl start binance-lob-archiver-production@spot.service \
    || { contain; die 'Spot failed to start during restore'; }
  systemctl start binance-lob-archiver-production@usdm.service \
    || { contain; die 'USD-M failed to start during restore'; }
fi

process_json='{}'; health_json='{}'
verify_runtime_lane() {
  local market=$1 unit="binance-lob-archiver-production@${1}.service" pid restarts exe env_file spool health updated session now
  systemctl is-active --quiet "$unit" || return 1
  pid=$(systemctl show "$unit" --property=MainPID --value); [[ $pid =~ ^[1-9][0-9]*$ ]] || return 1
  restarts=$(systemctl show "$unit" --property=NRestarts --value); [[ $restarts == 0 ]] || return 1
  exe=$(readlink -f -- "$(monday_root_join "$ROOT" "proc/$pid/exe")") || return 1
  [[ $exe == "$binary" && $(monday_sha256_file "$exe") == "$payload" ]] || return 1
  env_file=$(monday_runtime_asset_target "$ROOT" "binance-lob-archiver-production-$market.env") || return 1
  spool=$(sed -n 's/^SPOOL_DIR=//p' "$env_file" | head -n1)
  if [[ -n $spool ]]; then
    [[ $spool == /* ]] || return 1
    [[ $ROOT == / ]] || spool="$ROOT$spool"
    health="$spool/health.json"
    if (( restore_started_ns > 0 )); then
      local deadline=$(( $(date +%s) + 60 ))
      while :; do
        session=; updated=0
        if [[ -f $health && ! -L $health ]]; then
          session=$(jq -er '.session_id // empty' "$health" 2>/dev/null || true)
          updated=$(jq -er '.updated_at_ns // 0' "$health" 2>/dev/null || printf '0')
        fi
        if [[ -n $session && $updated =~ ^[0-9]+$ && $updated -ge $restore_started_ns ]]; then
          break
        fi
        now=$(date +%s)
        (( now < deadline )) || return 1
        sleep 1
      done
    else
      [[ -f $health && ! -L $health ]] || return 1
      session=$(jq -er '.session_id // empty' "$health") || return 1
      [[ -n $session ]] || return 1
      updated=$(jq -er '.updated_at_ns // 0' "$health"); [[ $updated =~ ^[0-9]+$ && $updated -gt 0 ]] || return 1
    fi
    now=$(date +%s%N); [[ $updated -le $now ]] || return 1
    health_json=$(jq -cn --argjson values "$health_json" --arg market "$market" --arg session "$session" --argjson observed "$updated" \
      '$values + {($market):{session_id:$session,observed_at_ns:$observed}}')
  fi
  process_json=$(jq -cn --argjson values "$process_json" --arg market "$market" --argjson pid "$pid" --arg sha "$(monday_sha256_file "$exe")" --argjson restarts "$restarts" \
    '$values + {($market):{main_pid:$pid,process_exe_sha256:$sha,n_restarts:$restarts,active:true}}')
}
if [[ $TEST_ONLY == false || $FIXTURE_SYSTEMD == true ]]; then
  verify_runtime_lane spot || { contain; die 'Spot runtime identity or fresh health failed'; }
  verify_runtime_lane usdm || { contain; die 'USD-M runtime identity or fresh health failed'; }
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
tmp="$receipt.tmp.$$"; completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
jq -cS -n --arg controller "$CONTROLLER" --arg payload "$payload" --arg completed "$completed_at" \
  --arg projection "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" \
  --argjson test_only "$TEST_ONLY" --argjson eligible "$( [[ $TEST_ONLY == true ]] && printf false || printf true )" \
  --argjson processes "$process_json" --argjson health "$health_json" \
  --argjson projections "$projections" --argjson controller_projections "$controller_projections" \
  '{schema:"monday.rust_lob_pair_restore.v2",control_plane_version:2,operation:"restore",test_only:$test_only,production_eligible:$eligible,
    controller_sha256:$controller,payload_sha256:$payload,stable_production_projection:$projection,
    active_pair_converged:true,installed_projections:$projections,controller_projections:$controller_projections,
    process_identity:$processes,health:$health,
    completed_at:$completed,result:"success"}' >"$tmp"
chmod 0640 "$tmp"; mv -f -- "$tmp" "$receipt"; success=true
receipt_sha=$(monday_sha256_file "$receipt"); printf '%s  restore.json\n' "$receipt_sha" >"$receipt.sha256"; chmod 0440 "$receipt" "$receipt.sha256"
printf 'Pair restore complete\nRestore receipt: %s\nSHA-256: %s\n' "$receipt" "$receipt_sha"
