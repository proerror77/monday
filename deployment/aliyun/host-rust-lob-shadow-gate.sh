#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

# V2 shadow Gate. Candidate controller C1 owns this script and its policy.
readonly REQUIRED_DURATION_SECONDS=240
readonly HEALTH_SETTLE_SECONDS=240
readonly MAX_HEALTH_SILENCE_SECONDS=120
readonly MAX_SEGMENT_GAP_NS=90000000000
readonly HOST_MEMORY_RESERVE_BYTES=1073741824
readonly STRICT_VERIFIER_MEMORY_MAX_BYTES=1610612736
readonly IO_PSI_WINDOW_SECONDS=15
readonly IO_PSI_WINDOW_US=15000000
readonly IO_PSI_FULL_DELTA_LIMIT_US=150000
readonly IO_PSI_CONSECUTIVE_HIT_LIMIT=3
readonly UPLOAD_DRAIN_MEMORY_MAX_BYTES=536870912
readonly SERVICE_USER=hftcollector
readonly SERVICE_HOME=/var/lib/hft-collector
readonly SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
readonly GATE_UNIT_PREFIX=monday-rust-lob-gate-
readonly -a SHADOW_ASSETS=(
  binance-lob-archiver-rust@.service
  binance-lob-archiver-rust-upload@.service
  binance-lob-archiver-rust-spot.env
  binance-lob-archiver-rust-usdm.env
)
readonly -a PRODUCTION_ASSETS=(
  'system-binance\x2dlob\x2darchiver\x2dproduction.slice'
  binance-lob-archiver-production@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
)
# systemd's escaped unit-template slice is part of the production contract.
# Keep the literal backslash-x2d spelling returned by systemctl on the live host.
readonly PRODUCTION_SLICE='system-binance\x2dlob\x2darchiver\x2dproduction.slice'
readonly FIXTURE_PRODUCTION_SPOT_PID=51001
readonly FIXTURE_PRODUCTION_USDM_PID=51002

die() { printf 'shadow gate failed: %s\n' "$*"; exit 1; }
usage() {
  cat >&2 <<'EOF'
Usage: host-rust-lob-shadow-gate.sh --from-controller <direct|sha256> \
  --candidate-controller <sha256> [--root <fixture-root>]
       host-rust-lob-shadow-gate.sh --recover-bootstrap-lease <state> [--root <fixture-root>]
EOF
}

ROOT=${MONDAY_ROOT:-/}; FROM_CONTROLLER=; CANDIDATE_CONTROLLER=
RECOVER_STATE=
while (($#)); do
  case "$1" in
    --from-controller) (($# >= 2)) || { usage; exit 2; }; FROM_CONTROLLER=$2; shift 2 ;;
    --candidate-controller) (($# >= 2)) || { usage; exit 2; }; CANDIDATE_CONTROLLER=$2; shift 2 ;;
    --recover-bootstrap-lease) (($# >= 2)) || { usage; exit 2; }; RECOVER_STATE=$2; shift 2 ;;
    --root) (($# >= 2)) || { usage; exit 2; }; ROOT=$2; shift 2 ;;
    --help|-h) usage >&1; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
ROOT=${ROOT%/}; [[ -n $ROOT ]] || ROOT=/
if [[ -z $RECOVER_STATE ]]; then
  FROM_CONTROLLER=$(printf '%s' "$FROM_CONTROLLER" | tr '[:upper:]' '[:lower:]')
  CANDIDATE_CONTROLLER=$(printf '%s' "$CANDIDATE_CONTROLLER" | tr '[:upper:]' '[:lower:]')
  [[ $FROM_CONTROLLER == direct || $FROM_CONTROLLER =~ ^[a-f0-9]{64}$ ]] ||
    die 'before controller must be direct or a 64-character SHA-256'
  [[ $CANDIDATE_CONTROLLER =~ ^[a-f0-9]{64}$ ]] ||
    die 'candidate controller must be a 64-character SHA-256'
else
  [[ -z $FROM_CONTROLLER && -z $CANDIDATE_CONTROLLER ]] ||
    die 'lease recovery does not accept a controller transition'
fi
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true
[[ $TEST_ONLY == false || $ROOT != / ]] || die 'test mode requires an isolated fixture root'

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck disable=SC1090,SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
monday_control_plane_validate_mode "$ROOT" "$TEST_ONLY" \
  || die 'production uses canonical root or fixture mode lacks an explicit sentinel'

GATE_DURATION_SECONDS=$REQUIRED_DURATION_SECONDS
HEALTH_SETTLE_DURATION_SECONDS=$HEALTH_SETTLE_SECONDS
resolve_test_duration() {
  local name value current formal
  for name in MONDAY_GATE_TEST_SECONDS MONDAY_TEST_HEALTH_SETTLE_SECONDS; do
    value=${!name:-}
    [[ -n $value ]] || continue
    [[ $TEST_ONLY == true && ${MONDAY_ALLOW_SHORT_GATE_FOR_TESTS:-0} == 1 ]] \
      || die "$name is only allowed for an explicitly authorised fixture Gate"
    [[ $value =~ ^[1-9][0-9]*$ ]] || die "$name must be a positive integer"
    if [[ $name == MONDAY_GATE_TEST_SECONDS ]]; then current=$value; formal=$REQUIRED_DURATION_SECONDS
    else current=$value; formal=$HEALTH_SETTLE_SECONDS; fi
    (( current < formal )) || die "$name must be shorter than the formal Gate contract"
    if [[ $name == MONDAY_GATE_TEST_SECONDS ]]; then GATE_DURATION_SECONDS=$current
    else HEALTH_SETTLE_DURATION_SECONDS=$current; fi
  done
}
resolve_test_duration

OPT_ROOT=$(monday_root_join "$ROOT" opt/monday); RELEASE_ROOT="$OPT_ROOT/releases/binance-lob-archiver"
CONTROLLER_ROOT="$OPT_ROOT/releases/binance-lob-controller"; BIN_ROOT="$OPT_ROOT/bin"
SYSTEMD_ROOT=$(monday_root_join "$ROOT" etc/systemd/system); CONFIG_ROOT=$(monday_root_join "$ROOT" etc/monday)
LOCK_FILE=$(monday_root_join "$ROOT" run/lock/monday-rust-lob-control-plane.lock)
OVERRIDE_ROOT=$(monday_root_join "$ROOT" run/monday)
DATA_ROOT=$(monday_root_join "$ROOT" data/monday); EVIDENCE_ROOT="$DATA_ROOT/evidence/shadow-gates"
# Gate writers are always run-scoped.  Nothing under /etc or the stable
# /opt/monday/bin projection is mutated by this operation.
RUN_SPOOL_ROOT="$DATA_ROOT/spool/binance-lob-rust-shadow/gate"; PROC_ROOT=$(monday_root_join "$ROOT" proc)
CGROUP_ROOT=$(monday_root_join "$ROOT" sys/fs/cgroup)
GATE_UNIT_ROOT="$OVERRIDE_ROOT/rust-lob-gate"; GATE_SYSTEMD_ROOT=$(monday_root_join "$ROOT" run/systemd/system); SHADOW_BINARY="$BIN_ROOT/binance-lob-archiver-shadow"
PSI_SOURCE="$PROC_ROOT/pressure/io"
PRODUCTION_BINARY="$BIN_ROOT/binance-lob-archiver"
LIB_SOURCE="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"; POLICY_SOURCE="$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq"
[[ -f $LIB_SOURCE && -f $POLICY_SOURCE ]] || die 'V2 control-plane assets are missing'

# Hold the release lock before reading candidate, active, or production
# identities.  The fixture skips flock but still exercises the same ordering.
mkdir -p "$(dirname -- "$LOCK_FILE")"
if [[ $TEST_ONLY == true ]]; then
  true
else
  exec 9>"$LOCK_FILE"
  flock -n 9 || die 'another collector control-plane action is running'
fi

if [[ -z $RECOVER_STATE ]]; then
  for command in awk bash chmod cmp cp date dirname find grep install jq mkdir mktemp mv readlink rm sed sha256sum sleep sort stat tr wc zstd; do
    command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
  done
  if [[ $TEST_ONLY != true ]]; then
    for command in aliyun flock id mountpoint runuser systemctl systemd-analyze systemd-run; do
      command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
    done
    mountpoint -q "$(monday_root_join "$ROOT" data)" || die 'data filesystem must be a mount point'
    [[ -r "$PROC_ROOT/uptime" && -r $PSI_SOURCE ]] || die 'proc timing/PSI sources are unavailable'
    id "$SERVICE_USER" >/dev/null 2>&1 || die "missing service user: $SERVICE_USER"
  fi
else
  for command in bash chmod date find grep install jq mkdir mv readlink rm sha256sum sort; do
    command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
  done
  [[ $TEST_ONLY == true ]] || command -v systemctl >/dev/null 2>&1 \
    || die 'missing required command: systemctl'
fi

# The offline fixture supplies a tiny systemd double.  Production always uses
# the real systemctl binary; the double only models the state fields consumed
# by this action and cannot mutate a host unit.
if [[ $TEST_ONLY == true ]]; then
  declare -A fixture_unit_state=()
  systemctl() {
    local action=${1:-} unit_name=${2:-} property value item output_values=false
    local -a properties=()
    case "$action" in
      list-units)
        if [[ -n ${MONDAY_GATE_FIXTURE_RECOVERY_RUN:-} ]]; then
          printf '%s loaded active running\n' \
            "${GATE_UNIT_PREFIX}${MONDAY_GATE_FIXTURE_RECOVERY_RUN}-spot.service"
        fi
        return 0 ;;
      start)
        [[ ${MONDAY_GATE_FIXTURE_RECORD_CALLS:-0} == 1 ]] && printf 'start %s\n' "$unit_name" >>"$ROOT/run/gate-fixture.calls"
        fixture_unit_state[$unit_name]=active; return 0 ;;
      stop)
        [[ ${MONDAY_GATE_FIXTURE_RECORD_CALLS:-0} == 1 ]] && printf 'stop %s\n' "$unit_name" >>"$ROOT/run/gate-fixture.calls"
        fixture_unit_state[$unit_name]=inactive; return 0 ;;
      set-property)
        [[ ${MONDAY_GATE_FIXTURE_RECORD_CALLS:-0} == 1 ]] && printf 'set-property\n' >>"$ROOT/run/gate-fixture.calls"
        return 0 ;;
      reset-failed|daemon-reload) return 0 ;;
      is-active)
        if [[ $2 == --quiet ]]; then unit_name=$3; else unit_name=$2; fi
        [[ ${fixture_unit_state[$unit_name]:-inactive} == active ]] && { [[ $2 == --quiet ]] || printf 'active\n'; return 0; }
        [[ $2 == --quiet ]] && return 3; printf 'inactive\n'; return 3 ;;
      show)
        unit_name=$2; property=${3#--property=}; property=${property#--property};
        if [[ $property == *=* ]]; then property=${property#*=}; fi
        if [[ ${4:-} == --value ]]; then
          output_values=true
          IFS=, read -r -a properties <<<"$property"
        elif [[ $unit_name == binance-lob-archiver-production@* ]]; then
          # Real systemd does not guarantee request order; keep the fixture
          # deliberately in the observed live order to exercise key parsing.
          properties=(ActiveState SubState MainPID NRestarts Slice ControlGroup MemoryMax)
        elif [[ $unit_name == "${GATE_WORKER_SLICE:-}" ]]; then
          # The aggregate slice readback is likewise order-independent.  Keep
          # the fixture in a different order from the request to prove that
          # callers parse KEY=VALUE rather than positional output.
          properties=(ControlGroup MemoryMax MemoryHigh)
        else
          output_values=false
          IFS=, read -r -a properties <<<"$property"
        fi
        for item in "${properties[@]}"; do
          case "$item" in
            ActiveState)
              if [[ $unit_name == binance-lob-archiver-production@* || $unit_name == "$PRODUCTION_SLICE" ]]; then value=active
              else value=${fixture_unit_state[$unit_name]:-inactive}; fi ;;
            SubState)
              if [[ $unit_name == binance-lob-archiver-production@* || $unit_name == "$PRODUCTION_SLICE" ]]; then value=running
              elif [[ ${fixture_unit_state[$unit_name]:-inactive} == active ]]; then value=running
              else value=dead; fi ;;
            NRestarts)
              if [[ $unit_name == binance-lob-archiver-production@* ]]; then value=${MONDAY_GATE_FIXTURE_PRODUCTION_RESTARTS:-8}
              elif [[ ${MONDAY_GATE_FIXTURE_FAIL_RESTART:-0} == 1 ]]; then value=1
              else value=0; fi ;;
            MainPID)
              if [[ $unit_name == binance-lob-archiver-production@spot.service ]]; then value=$FIXTURE_PRODUCTION_SPOT_PID
              elif [[ $unit_name == binance-lob-archiver-production@usdm.service ]]; then value=$FIXTURE_PRODUCTION_USDM_PID
              else value=$$; fi ;;
            MemoryCurrent) value=1048576 ;;
            MemoryPeak) value=1048576 ;;
            MemoryMax)
              if [[ $unit_name == "$PRODUCTION_SLICE" && ${MONDAY_GATE_FIXTURE_BOOTSTRAP_UNLIMITED:-0} == 1 ]]; then value=infinity
              elif [[ $unit_name == binance-lob-archiver-production@* ]]; then value=2684354560
              elif [[ $unit_name == "$PRODUCTION_SLICE" ]]; then value=3758096384
              else value=1610612736; fi ;;
            MemoryHigh)
              if [[ $unit_name == "$PRODUCTION_SLICE" && ${MONDAY_GATE_FIXTURE_BOOTSTRAP_UNLIMITED:-0} == 1 ]]; then value=infinity
              elif [[ $unit_name == "$PRODUCTION_SLICE" ]]; then value=3221225472
              else value=1342177280; fi ;;
            Slice) [[ $unit_name == binance-lob-archiver-production@* ]] && value=$PRODUCTION_SLICE || value=system.slice ;;
            ControlGroup)
              if [[ $unit_name == binance-lob-archiver-production@spot.service ]]; then value="/system.slice/$PRODUCTION_SLICE/binance-lob-archiver-production@spot.service"
              elif [[ $unit_name == binance-lob-archiver-production@usdm.service ]]; then value="/system.slice/$PRODUCTION_SLICE/binance-lob-archiver-production@usdm.service"
            elif [[ $unit_name == "$PRODUCTION_SLICE" ]]; then value="/system.slice/$PRODUCTION_SLICE"
            elif [[ $unit_name == "${GATE_WORKER_SLICE:-}" ]]; then value="/${GATE_WORKER_SLICE}"
            else value=; fi ;;
            CPUUsageNSec) value=1000000 ;;
            CPUQuotaPerSecUSec) value=800ms ;;
            DropInPaths) value= ;;
            OOMScoreAdjust) value=500 ;;
            *) value= ;;
          esac
          if [[ ${output_values:-false} == true ]]; then
            printf '%s\n' "$value"
          else
            printf '%s=%s\n' "$item" "$value"
          fi
        done
        return 0 ;;
      *) return 0 ;;
    esac
  }
  aliyun() {
    local tool=${1:-} action=${2:-} source target object
    [[ $tool == ossutil ]] || return 2
    shift 2
    case "$action" in
      ls)
        fixture_date=2026-08-28
        fixture_hour=05
        for object in "${spool_dir[$OSS_FIXTURE_MARKET]}"/*.manifest.json; do
          [[ -f $object ]] || continue
          printf 'oss://fixture/lake/raw/venue=binance/market=%s/dataset=%s/shard=all/date=%s/hour=%s/%s\n' \
            "$OSS_FIXTURE_MARKET" "${dataset[$OSS_FIXTURE_MARKET]}" "$fixture_date" "$fixture_hour" "${object##*/}"
          if [[ ${MONDAY_GATE_FIXTURE_EXTRA_NESTED:-0} == 1 ]]; then
            printf 'oss://fixture/lake/raw/venue=binance/market=%s/dataset=%s/shard=all/extra/date=%s/hour=%s/%s\n' \
              "$OSS_FIXTURE_MARKET" "${dataset[$OSS_FIXTURE_MARKET]}" "$fixture_date" "$fixture_hour" "${object##*/}"
          fi
        done
        ;;
      cp)
        source=${1:-}; target=${2:-}
        object=${source##*/}
        [[ $object != "$source" && -n $target ]] || return 2
        cp -p -- "${spool_dir[$OSS_FIXTURE_MARKET]}/$object" "$target"
        if [[ ${MONDAY_GATE_FIXTURE_TAMPER_OSS:-0} == 1 && $object == *.jsonl.zst ]]; then
          printf '\n' >>"$target"
        fi
        ;;
      *) return 2 ;;
    esac
  }
fi

direct_directory() { local path=$1; [[ -d $path && ! -L $path && $(readlink -f -- "$path") == "$path" ]]; }
direct_directory_or_absent() { local path=$1; [[ ! -e $path && ! -L $path ]] || direct_directory "$path"; }
regular_file() { [[ -f $1 && ! -L $1 ]]; }
directory_mode() {
  local path=$1 value
  if value=$(stat -c '%a' -- "$path" 2>/dev/null); then printf '%s\n' "$value"; else stat -f '%Lp' -- "$path"; fi
}
directory_owner_group() {
  local path=$1 value
  if value=$(stat -c '%U:%G' -- "$path" 2>/dev/null); then printf '%s\n' "$value"; else stat -f '%Su:%Sg' -- "$path"; fi
}
ensure_run_spool_dir() {
  local path=$1 owner_group
  direct_directory_or_absent "$path" || die "run spool parent is indirect: $path"
  if [[ $TEST_ONLY == true ]]; then
    install -d -m 0750 "$path"
  else
    install -d -o "$SERVICE_USER" -g "$SERVICE_USER" -m 0750 "$path"
  fi
  direct_directory "$path" || die "run spool parent is not a direct directory: $path"
  [[ $(directory_mode "$path") == 750 ]] || die "run spool parent mode is not 0750: $path"
  if [[ $TEST_ONLY != true ]]; then
    owner_group=$(directory_owner_group "$path")
    [[ $owner_group == "$SERVICE_USER:$SERVICE_USER" || $owner_group == "root:$SERVICE_USER" ]] \
      || die "run spool parent ownership is not safe: $path ($owner_group)"
  fi
}
secure_file() {
  local path=$1 mode owner; regular_file "$path" || die "required regular file is missing: $path"
  if [[ $TEST_ONLY != true ]]; then owner=$(stat -c %u -- "$path"); mode=$(stat -c %a -- "$path")
    [[ $owner == 0 ]] || die "required file is not root-owned: $path"
    (( (8#$mode & 022) == 0 )) || die "required file is writable by group/world: $path"; fi
}
sha256_file() { monday_sha256_file "$1"; }

for path in "$OPT_ROOT" "$RELEASE_ROOT" "$CONTROLLER_ROOT" "$BIN_ROOT" \
  "$SYSTEMD_ROOT" "$CONFIG_ROOT" "$DATA_ROOT" "$DATA_ROOT/spool" \
  "$DATA_ROOT/spool/binance-lob-rust-shadow" "$CGROUP_ROOT"; do
  direct_directory_or_absent "$path" || die "control-plane path is indirect: $path"
done
direct_directory_or_absent "$(dirname -- "$LOCK_FILE")" || die 'control-plane lock path is indirect'
direct_directory_or_absent "$OVERRIDE_ROOT" || die 'shadow override path is indirect'
direct_directory_or_absent "$(monday_root_join "$ROOT" run/systemd)" || die 'systemd runtime path is indirect'
direct_directory_or_absent "$GATE_SYSTEMD_ROOT" || die 'systemd runtime unit path is indirect'

meminfo_bytes() {
  local field=$1 source="$PROC_ROOT/meminfo" value
  if [[ ! -f $source && $TEST_ONLY == true ]]; then case "$field" in
    MemTotal) printf '8589934592\n';;
    MemAvailable)
      if [[ ${MONDAY_GATE_FIXTURE_FRESH_ADMISSION_FAIL:-0} == 1 \
        && ${fixture_last_calibrated_phase:-} == shadow-spot ]]; then
        printf '3000000000\n'
      else
        printf '6442450944\n'
      fi ;;
    SwapTotal) printf '0\n';;
    esac; return; fi
  value=$(awk -v key="$field:" '$1 == key { count++; value=$2 } END { if (count != 1 || value !~ /^[0-9]+$/) exit 1; print value }' "$source") || return 1
  printf '%s\n' "$((value * 1024))"
}
monotonic_seconds() { if [[ $TEST_ONLY == true && ! -r "$PROC_ROOT/uptime" ]]; then printf '%s\n' "$(date +%s)"; else awk '{print int($1)}' "$PROC_ROOT/uptime"; fi; }
proc_starttime() {
  local pid=$1 stat_file
  [[ $pid =~ ^[1-9][0-9]*$ ]] || return 1
  stat_file="$PROC_ROOT/$pid/stat"
  [[ -r $stat_file ]] || return 1
  awk '{ if ($22 !~ /^[0-9]+$/) exit 1; print $22 }' "$stat_file"
}
io_total_us() { if [[ $TEST_ONLY == true && ! -f $PSI_SOURCE ]]; then printf '0\n'; else monday_io_full_psi_total_us "$PSI_SOURCE"; fi; }
systemctl_show() { systemctl show "$1" --property="$2" --value 2>/dev/null; }
systemctl_show_many() { systemctl show "$1" --property="$2" 2>/dev/null; }
systemctl_active() { systemctl is-active --quiet "$1"; }

# Recover a temporary production-slice lease from its persisted state.  This
# is intentionally an internal mode of the signed Gate script, rather than a
# second cleanup script: the transient systemd service invokes this exact
# candidate byte path after a SIGKILL.  It stops only the matching run's
# workers, restores the recorded limits, reads them back, then removes its own
# timer/service and marks the state recovered.
recover_bootstrap_lease() {
  [[ $# -eq 1 ]] || die 'lease recovery requires one state file'
  local state=$1 state_name run mode applied restored before_high before_max
  local expected_service expected_timer service_name timer_name slice_name
  local candidate_controller gate_script gate_script_sha gate_pid gate_starttime owner_starttime expected_gate_script
  local listed unit unit_file output item key value restored_high restored_max temporary
  local -A fields=()
  direct_directory "$GATE_UNIT_ROOT" || die 'lease state root is not a direct directory'
  [[ $state == "$GATE_UNIT_ROOT"/bootstrap-slice-lease-*.json ]] || die 'lease state is outside the canonical root'
  state_name=${state##*/}
  [[ $state_name =~ ^bootstrap-slice-lease-([0-9]{8}T[0-9]{6}Z-[1-9][0-9]*)\.json$ ]] \
    || die 'lease state filename is not run-scoped'
  secure_file "$state"
  jq -e --arg slice "$PRODUCTION_SLICE" '
    type == "object"
    and .schema == "monday.rust_lob_bootstrap_slice_lease.v1"
    and (.run_id | type == "string" and test("^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$"))
    and .slice == $slice
    and .mode == "temporary-bootstrap"
    and (.before_memory_high | type == "string" and test("^(infinity|[0-9]+)$"))
    and (.before_memory_max | type == "string" and test("^(infinity|[0-9]+)$"))
    and (.before_parent_control_group | type == "string" and . == ("/system.slice/" + $slice))
    and (.before_parent_memory_current_bytes | type == "number" and floor == . and . >= 0 and . <= 3758096384)
    and (.before_parent_memory_anon_bytes | type == "number" and floor == . and . >= 0 and . <= 3758096384)
    and .requested_memory_high == "3072M"
    and .requested_memory_max == "3584M"
    and (.candidate_controller_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.gate_script | type == "string" and length > 0)
    and (.gate_script_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.gate_pid | type == "number" and floor == . and . >= 1)
    and (.gate_starttime | type == "number" and floor == . and . >= 1)
    and (.applied | type == "boolean")
    and (.restored | type == "boolean")
    and (.recovery_service | type == "string")
    and (.recovery_timer | type == "string")' "$state" >/dev/null \
    || die 'lease state schema is invalid'
  run=$(jq -er '.run_id' "$state") || die 'lease state run id is unavailable'
  expected_service="${GATE_UNIT_PREFIX}${run}-lease-recovery.service"
  expected_timer="${GATE_UNIT_PREFIX}${run}-lease-recovery.timer"
  service_name=$(jq -er '.recovery_service' "$state") || die 'lease recovery service is unavailable'
  timer_name=$(jq -er '.recovery_timer' "$state") || die 'lease recovery timer is unavailable'
  [[ $service_name == "$expected_service" && $timer_name == "$expected_timer" ]] \
    || die 'lease recovery unit identity is not run-scoped'
  mode=$(jq -er '.mode' "$state") || die 'lease state mode is unavailable'
  applied=$(jq -r '.applied' "$state") || die 'lease state applied flag is unavailable'
  restored=$(jq -r '.restored' "$state") || die 'lease state restored flag is unavailable'
  before_high=$(jq -er '.before_memory_high' "$state") || die 'lease state high limit is unavailable'
  before_max=$(jq -er '.before_memory_max' "$state") || die 'lease state max limit is unavailable'
  [[ $mode == temporary-bootstrap ]] || die 'lease state mode is not temporary bootstrap'
  candidate_controller=$(jq -er '.candidate_controller_sha256' "$state") || die 'lease candidate controller is unavailable'
  gate_script=$(jq -er '.gate_script' "$state") || die 'lease Gate script is unavailable'
  gate_script_sha=$(jq -er '.gate_script_sha256' "$state") || die 'lease Gate script digest is unavailable'
  gate_pid=$(jq -er '.gate_pid' "$state") || die 'lease Gate owner PID is unavailable'
  gate_starttime=$(jq -er '.gate_starttime' "$state") || die 'lease Gate owner starttime is unavailable'
  expected_gate_script="$CONTROLLER_ROOT/$candidate_controller/deployment/host-rust-lob-shadow-gate.sh"
  [[ $gate_script == "$expected_gate_script" ]] || die 'lease Gate script is not the candidate deployment path'
  monday_file_direct "$gate_script" || die 'lease Gate script is not a direct file'
  [[ $(sha256_file "$gate_script") == "$gate_script_sha" ]] || die 'lease Gate script digest changed'

  # The recovery timer is a watchdog, not a delayed normal cleanup.  While
  # the Gate owner still has the recorded PID/starttime pair, leave its lease
  # and workers untouched; the repeating timer will check again later.  A
  # missing owner or PID reuse is the only condition that permits recovery.
  if [[ $applied == true && $restored == false ]] && kill -0 "$gate_pid" 2>/dev/null; then
    owner_starttime=$(proc_starttime "$gate_pid" 2>/dev/null || true)
    [[ -n $owner_starttime ]] || die 'lease Gate owner starttime is unverifiable'
    [[ $owner_starttime == "$gate_starttime" ]] && return 0
  fi

  # The recovery service is run-scoped.  Do not use a broad unit wildcard or
  # pkill: any non-matching name is ignored and no unrelated collector unit is
  # touched.
  listed=$(systemctl list-units --all --type=service --no-legend --plain \
    "${GATE_UNIT_PREFIX}${run}-*.service") || die 'could not list run-scoped Gate workers'
  while read -r unit _; do
    [[ -n ${unit:-} ]] || continue
    [[ $unit =~ ^${GATE_UNIT_PREFIX}${run}-(spot|usdm|spot-upload|usdm-upload|strict-[1-9][0-9]*|oss-(spot|usdm)-[1-9][0-9]*)\.service$ ]] \
      || continue
    systemctl stop "$unit" >/dev/null 2>&1 || true
    systemctl reset-failed "$unit" >/dev/null 2>&1 || true
  done <<<"$listed"
  direct_directory "$GATE_SYSTEMD_ROOT" || die 'systemd runtime unit path is not direct'
  while IFS= read -r unit_file; do
    unit=${unit_file##*/}
    [[ $unit =~ ^${GATE_UNIT_PREFIX}${run}-(spot|usdm|spot-upload|usdm-upload|strict-[1-9][0-9]*|oss-(spot|usdm)-[1-9][0-9]*)\.service$ ]] \
      || continue
    rm -f -- "$unit_file"
  done < <(find "$GATE_SYSTEMD_ROOT" -maxdepth 1 -type f -name "${GATE_UNIT_PREFIX}${run}-*.service" -print)
  # Keep the aggregate slice top-level in systemd.  Dashes in a slice name
  # encode hierarchy, so the run timestamp/PID is rendered as digits only.
  slice_name="mondayrustlobgate${run//[^0-9]/}.slice"
  if [[ -f "$GATE_SYSTEMD_ROOT/$slice_name" && ! -L "$GATE_SYSTEMD_ROOT/$slice_name" ]]; then
    systemctl stop "$slice_name" >/dev/null 2>&1 || true
    rm -f -- "$GATE_SYSTEMD_ROOT/$slice_name"
  fi

  if [[ $applied == true && $restored == false ]]; then
    systemctl set-property --runtime "$PRODUCTION_SLICE" \
      "MemoryHigh=$before_high" "MemoryMax=$before_max" >/dev/null 2>&1 \
      || die 'could not restore the bootstrap production slice lease'
    output=$(systemctl_show_many "$PRODUCTION_SLICE" 'MemoryHigh,MemoryMax') \
      || die 'restored production slice limits are unavailable'
    output=${output%$'\n'}; fields=()
    while IFS= read -r item; do
      [[ $item == *=* ]] || die 'restored production slice limits are malformed'
      key=${item%%=*}; value=${item#*=}
      case $key in MemoryHigh|MemoryMax) ;; *) die 'restored production slice has an unexpected field' ;; esac
      [[ -z ${fields[$key]+x} ]] || die 'restored production slice has duplicate fields'
      fields[$key]=$value
    done <<<"$output"
    [[ ${#fields[@]} == 2 ]] || die 'restored production slice limits are incomplete'
    restored_high=${fields[MemoryHigh]:-}; restored_max=${fields[MemoryMax]:-}
    [[ $restored_high == "$before_high" && $restored_max == "$before_max" ]] \
      || die 'restored production slice limits differ from lease state'
  fi

  # This function runs as the recovery oneshot itself.  Stop/reset only the
  # timer and run workers; stopping the current service would self-interrupt
  # before its state/readback and unit removal complete.  Normal EXIT cleanup
  # uses cancel_bootstrap_lease_recovery, which may stop both units.
  systemctl stop "$timer_name" >/dev/null 2>&1 || true
  systemctl reset-failed "$timer_name" >/dev/null 2>&1 || true
  rm -f -- "$GATE_SYSTEMD_ROOT/$timer_name" "$GATE_SYSTEMD_ROOT/$service_name"
  systemctl daemon-reload >/dev/null 2>&1 || die 'could not reload systemd after lease recovery'
  temporary="${state}.tmp.$$"
  jq -cS --arg recovered_by "$run" \
    '.restored = true | .recovered_by_run_id = $recovered_by' "$state" >"$temporary" \
    || die 'could not mark bootstrap lease recovered'
  chmod 0640 "$temporary" || die 'could not protect recovered lease state'
  mv -f -- "$temporary" "$state" || die 'could not publish recovered lease state'
}

if [[ -n $RECOVER_STATE ]]; then
  recover_bootstrap_lease "$RECOVER_STATE"
  exit 0
fi

# A production snapshot is one immutable view of the systemd template pair
# and its automatically assigned slice.  The snapshot carries cgroup limits
# and audit counters separately from process identity so resource admission can
# use conservative current/limit values while the monitor compares only the
# identity-bearing fields.
cgroup_file_value() {
  local path=$1
  [[ -f $path && ! -L $path ]] || return 1
  tr -d '\n' <"$path"
}
cgroup_numeric_value() {
  local path=$1 value
  value=$(cgroup_file_value "$path") || return 1
  [[ $value =~ ^(0|[1-9][0-9]*)$ ]] || return 1
  printf '%s\n' "$value"
}
cgroup_events_json() {
  local path=$1
  [[ -f $path && ! -L $path ]] || return 1
  jq -Rsc '
    split("\n")
    | map(select(length > 0) | capture("^(?<key>[A-Za-z0-9_]+)[[:space:]]+(?<value>[0-9]+)$")
      | {key:.key,value:(.value|tonumber)})
    | from_entries
  ' "$path"
}
cgroup_memory_stat_json() {
  local path=$1
  [[ -f $path && ! -L $path ]] || return 1
  jq -Rsc '
    split("\n")
    | map(select(length > 0) | capture("^(?<key>[A-Za-z0-9_]+)[[:space:]]+(?<value>[0-9]+)$")
      | {key:.key,value:(.value|tonumber)}) as $rows
    | (($rows | map(.key) | unique | length) == ($rows | length))
    | if . then ($rows | from_entries) else error("duplicate memory.stat key") end
  ' "$path"
}
cgroup_child_path() {
  local control_group=$1 normalized
  [[ $control_group == /* && $control_group != */ && $control_group != *..* ]] || return 1
  normalized=${control_group#/}
  [[ -n $normalized && $normalized != *'//'* ]] || return 1
  printf '%s/%s\n' "$CGROUP_ROOT" "$normalized"
}

fixture_prepare_production_cgroup() {
  [[ $TEST_ONLY == true ]] || return 0
  local parent="$CGROUP_ROOT/system.slice/$PRODUCTION_SLICE" child unit market fixture_pid fixture_cgroup_pid
  local fixture_parent_current=${MONDAY_GATE_FIXTURE_BOOTSTRAP_PARENT_CURRENT:-1101067264}
  local fixture_parent_anon=${MONDAY_GATE_FIXTURE_BOOTSTRAP_PARENT_ANON:-317067264}
  mkdir -p "$parent"
  : >"$parent/cgroup.procs"
  printf '%s\n' "$fixture_parent_current" >"$parent/memory.current"
  printf '5100000000\n' >"$parent/memory.peak"
  printf '3221225472\n' >"$parent/memory.high"
  printf '3758096384\n' >"$parent/memory.max"
  printf 'anon %s\nfile 784000000\n' "$fixture_parent_anon" >"$parent/memory.stat"
  printf 'high 0\noom 0\noom_kill 0\n' >"$parent/memory.events"
  for market in spot usdm; do
    child="$parent/binance-lob-archiver-production@${market}.service"
    mkdir -p "$child"
    if [[ $market == spot ]]; then fixture_pid=$FIXTURE_PRODUCTION_SPOT_PID
    else fixture_pid=$FIXTURE_PRODUCTION_USDM_PID; fi
    if [[ ${MONDAY_GATE_FIXTURE_PID_MISMATCH:-0} == 1 && $market == spot ]]; then
      fixture_cgroup_pid=$FIXTURE_PRODUCTION_USDM_PID
    else
      fixture_cgroup_pid=$fixture_pid
    fi
    printf '%s\n' "$fixture_cgroup_pid" >"$child/cgroup.procs"
    printf '2684354560\n' >"$child/memory.max"
    mkdir -p "$PROC_ROOT/$fixture_pid"
    rm -f -- "$PROC_ROOT/$fixture_pid/exe"
    ln -s "$RELEASE_ROOT/$before_payload/binance-lob-archiver" \
      "$PROC_ROOT/$fixture_pid/exe"
  done
  if [[ ${MONDAY_GATE_FIXTURE_EXTRA_CHILD:-0} != 1 ]]; then
    rm -rf -- "$parent/foreign.service"
  fi
  if [[ ${MONDAY_GATE_FIXTURE_EXTRA_CHILD:-0} == 1 ]]; then
    child="$parent/foreign.service"
    mkdir -p "$child"
    printf '%s\n' "$$" >"$child/cgroup.procs"
    printf '1\n' >"$child/memory.max"
  fi
  if [[ ${MONDAY_GATE_FIXTURE_PARENT_PROCS:-0} == 1 ]]; then
    printf '%s\n' "$$" >"$parent/cgroup.procs"
  fi
}

capture_production_snapshot() {
  local market unit state substate slice control_group parent_group memory_max pid exe_sha n_restarts
  local child_path parent_path parent_procs_json active_children_json child_max_sum=0
  local parent_current parent_peak parent_events parent_high parent_max parent_anon parent_file parent_stat
  local slice_systemd_high slice_systemd_max slice_control_group
  local children_json='{}' expected_parent='' expected_slice=''
  local show_output item key value
  local slice_show_output slice_show_item slice_show_key slice_show_value
  declare -A show_fields=()
  declare -A slice_show_fields=()
  local -a active_children=()
  if [[ $TEST_ONLY == true ]]; then
    fixture_prepare_production_cgroup
  fi

  for market in spot usdm; do
    unit="binance-lob-archiver-production@${market}.service"
    show_output=$(systemctl_show_many "$unit" 'ActiveState,SubState,Slice,ControlGroup,MemoryMax,MainPID,NRestarts') || return 1
    show_output=${show_output%$'\n'}
    show_fields=()
    while IFS= read -r item; do
      [[ $item == *=* ]] || return 1
      key=${item%%=*}; value=${item#*=}
      case $key in
        ActiveState|SubState|Slice|ControlGroup|MemoryMax|MainPID|NRestarts) ;;
        *) return 1 ;;
      esac
      [[ -z ${show_fields[$key]+x} ]] || return 1
      show_fields[$key]=$value
    done <<<"$show_output"
    (( ${#show_fields[@]} == 7 )) || return 1
    state=${show_fields[ActiveState]:-}; substate=${show_fields[SubState]:-}; slice=${show_fields[Slice]:-}
    control_group=${show_fields[ControlGroup]:-}; memory_max=${show_fields[MemoryMax]:-}
    pid=${show_fields[MainPID]:-}; n_restarts=${show_fields[NRestarts]:-}
    [[ $state == active && $substate == running ]] || return 1
    [[ $slice == "$PRODUCTION_SLICE" ]] || return 1
    slice=${slice#/}
    [[ $memory_max =~ ^2684354560$ ]] || return 1
    [[ $pid =~ ^[1-9][0-9]*$ && $n_restarts =~ ^[0-9]+$ ]] || return 1
    [[ -r "$PROC_ROOT/$pid/exe" ]] || return 1
    exe_sha=$(sha256_file "$(readlink -f -- "$PROC_ROOT/$pid/exe")") || return 1
    [[ $exe_sha == "$before_payload" ]] || return 1
    if [[ -z $expected_slice ]]; then
      expected_slice=$slice
      expected_parent="/system.slice/$slice"
    else
      [[ $slice == "$expected_slice" ]] || return 1
    fi
    parent_group=${control_group%/*}
    [[ $parent_group == "$expected_parent" ]] || return 1
    [[ $control_group == "$expected_parent/binance-lob-archiver-production@${market}.service" ]] || return 1
    child_path=$(cgroup_child_path "$control_group") || return 1
    parent_path=$(cgroup_child_path "$parent_group") || return 1
    monday_path_direct "$child_path" || return 1
    monday_path_direct "$parent_path" || return 1
    awk -v expected_pid="$pid" '$1 == expected_pid { found=1 } END { exit(found ? 0 : 1) }' \
      "$child_path/cgroup.procs" || return 1
    [[ $(cgroup_numeric_value "$child_path/memory.max") == 2684354560 ]] || return 1
    child_max_sum=$((child_max_sum + 2684354560))
    children_json=$(jq -cn --argjson values "$children_json" --arg market "$market" --arg slice "$slice" \
      --arg control_group "$control_group" --argjson pid "$pid" --arg exe "$exe_sha" \
      --argjson restarts "$n_restarts" --argjson systemd_max "$memory_max" \
      '$values + {($market):{market:$market,slice:$slice,control_group:$control_group,main_pid:$pid,
        process_exe_sha256:$exe,n_restarts:$restarts,active:true,
        systemd_memory_max_bytes:$systemd_max,memory_max_bytes:2684354560}}')
  done
  # The aggregate slice is governed by one explicit signed unit.  Keep the
  # parser order-independent because systemctl does not promise request order.
  slice_show_output=$(systemctl_show_many "$PRODUCTION_SLICE" 'MemoryHigh,MemoryMax,ControlGroup') || return 1
  slice_show_output=${slice_show_output%$'\n'}
  slice_show_fields=()
  while IFS= read -r slice_show_item; do
    [[ $slice_show_item == *=* ]] || return 1
    slice_show_key=${slice_show_item%%=*}; slice_show_value=${slice_show_item#*=}
    case $slice_show_key in MemoryHigh|MemoryMax|ControlGroup) ;; *) return 1 ;; esac
    [[ -z ${slice_show_fields[$slice_show_key]+x} ]] || return 1
    slice_show_fields[$slice_show_key]=$slice_show_value
  done <<<"$slice_show_output"
  (( ${#slice_show_fields[@]} == 3 )) || return 1
  slice_systemd_high=${slice_show_fields[MemoryHigh]:-}
  slice_systemd_max=${slice_show_fields[MemoryMax]:-}
  slice_control_group=${slice_show_fields[ControlGroup]:-}
  [[ $slice_systemd_high == 3221225472 && $slice_systemd_max == 3758096384 ]] || return 1
  [[ $slice_control_group == "/system.slice/$PRODUCTION_SLICE" ]] || return 1
  parent_path=$(cgroup_child_path "$expected_parent") || return 1
  parent_high=$(cgroup_numeric_value "$parent_path/memory.high") || return 1
  parent_max=$(cgroup_numeric_value "$parent_path/memory.max") || return 1
  [[ $parent_high == 3221225472 && $parent_max == 3758096384 ]] || return 1
  parent_procs_json=$(awk 'NF {bad=1; values[++n]=$1} END {if (bad) {printf "["; for (i=1;i<=n;i++) printf "%s%s", (i>1?",":""), values[i]; printf "]"} else print "[]"}' \
    "$parent_path/cgroup.procs") || return 1
  [[ $parent_procs_json == '[]' ]] || return 1
  while IFS= read -r child_path; do
    [[ -d $child_path && ! -L $child_path ]] || continue
    [[ -f $child_path/cgroup.procs && ! -L $child_path/cgroup.procs ]] || return 1
    if awk 'NF {found=1} END {exit(found ? 0 : 1)}' "$child_path/cgroup.procs"; then
      active_children+=("$expected_parent/${child_path##*/}")
    fi
  done < <(find "$parent_path" -mindepth 1 -maxdepth 1 -type d -print)
  active_children_json=$(printf '%s\n' "${active_children[@]}" | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  parent_current=$(cgroup_numeric_value "$parent_path/memory.current") || return 1
  parent_peak=$(cgroup_numeric_value "$parent_path/memory.peak") || return 1
  parent_stat=$(cgroup_memory_stat_json "$parent_path/memory.stat") || return 1
  parent_anon=$(jq -er '.anon' <<<"$parent_stat") || return 1
  parent_file=$(jq -er '.file' <<<"$parent_stat") || return 1
  [[ $parent_anon =~ ^[0-9]+$ && $parent_file =~ ^[0-9]+$ ]] || return 1
  (( parent_anon <= parent_current )) || return 1
  parent_events=$(cgroup_events_json "$parent_path/memory.events") || return 1
  jq -cn --arg slice "$expected_slice" --arg parent "$expected_parent" \
    --argjson procs "$parent_procs_json" --argjson active "$active_children_json" \
    --argjson children "$children_json" --argjson current "$parent_current" \
    --argjson peak "$parent_peak" --argjson high "$parent_high" --argjson max "$parent_max" \
    --argjson systemd_high "$slice_systemd_high" --argjson systemd_max "$slice_systemd_max" \
    --argjson anon "$parent_anon" --argjson file "$parent_file" --argjson stat "$parent_stat" \
    --argjson sum "$child_max_sum" --argjson events "$parent_events" \
    '{slice:$slice,parent_control_group:$parent,parent_cgroup_procs:$procs,
      active_child_control_groups:$active,children:$children,
      production_slice_memory_high_bytes:$high,production_slice_memory_max_bytes:$max,
      systemd_production_slice_memory_high_bytes:$systemd_high,
      systemd_production_slice_memory_max_bytes:$systemd_max,
      parent_memory_current_bytes:$current,parent_memory_peak_bytes:$peak,
      parent_memory_anon_bytes:$anon,parent_memory_file_bytes:$file,parent_memory_stat:$stat,
      child_memory_max_sum_bytes:$sum,parent_memory_events:$events}'
}

production_snapshot_json=''; production_snapshot_identity_json=''
production_parent_current=''; production_parent_anon=''; production_parent_file=''
production_child_max_sum=''; production_slice_memory_max=''; production_growth=''
production_memory_json='{}' production_process_json='{}'
refresh_production_snapshot() {
  local first second first_identity second_identity current_a current_b anon_a anon_b file_a file_b
  local sum_a sum_b slice_max_a slice_max_b slice_high_a slice_high_b conservative_current conservative_anon conservative_file conservative_sum
  first=$(capture_production_snapshot) || return 1
  monday_validate_lob_production_snapshot "$first" || return 1
  first_identity=$(monday_lob_production_snapshot_identity "$first") || return 1
  if [[ -n ${production_snapshot_identity_json:-} && $first_identity != "$production_snapshot_identity_json" ]]; then
    return 1
  fi
  second=$(capture_production_snapshot) || return 1
  monday_validate_lob_production_snapshot "$second" || return 1
  second_identity=$(monday_lob_production_snapshot_identity "$second") || return 1
  [[ $second_identity == "$first_identity" ]] || return 1
  current_a=$(jq -er '.parent_memory_current_bytes' <<<"$first") || return 1
  current_b=$(jq -er '.parent_memory_current_bytes' <<<"$second") || return 1
  anon_a=$(jq -er '.parent_memory_anon_bytes' <<<"$first") || return 1
  anon_b=$(jq -er '.parent_memory_anon_bytes' <<<"$second") || return 1
  file_a=$(jq -er '.parent_memory_file_bytes' <<<"$first") || return 1
  file_b=$(jq -er '.parent_memory_file_bytes' <<<"$second") || return 1
  sum_a=$(jq -er '.child_memory_max_sum_bytes' <<<"$first") || return 1
  sum_b=$(jq -er '.child_memory_max_sum_bytes' <<<"$second") || return 1
  slice_max_a=$(jq -er '.production_slice_memory_max_bytes' <<<"$first") || return 1
  slice_max_b=$(jq -er '.production_slice_memory_max_bytes' <<<"$second") || return 1
  slice_high_a=$(jq -er '.production_slice_memory_high_bytes' <<<"$first") || return 1
  slice_high_b=$(jq -er '.production_slice_memory_high_bytes' <<<"$second") || return 1
  conservative_current=$current_a; (( current_b < conservative_current )) && conservative_current=$current_b
  conservative_anon=$anon_a; (( anon_b < conservative_anon )) && conservative_anon=$anon_b
  conservative_file=$file_a; (( file_b > conservative_file )) && conservative_file=$file_b
  conservative_sum=$sum_a; (( sum_b > conservative_sum )) && conservative_sum=$sum_b
  [[ $slice_max_a == "$slice_max_b" && $slice_high_a == "$slice_high_b" ]] || return 1
  production_snapshot_json=$(jq -cn --argjson value "$second" --argjson current "$conservative_current" \
    --argjson anon "$conservative_anon" --argjson file "$conservative_file" --argjson sum "$conservative_sum" \
    '$value | .parent_memory_current_bytes=$current | .parent_memory_anon_bytes=$anon
      | .parent_memory_file_bytes=$file | .child_memory_max_sum_bytes=$sum')
  production_parent_current=$conservative_current
  production_parent_anon=$conservative_anon
  production_parent_file=$conservative_file
  production_child_max_sum=$conservative_sum
  production_slice_memory_max=$slice_max_a
  production_growth=$((production_slice_memory_max - production_parent_anon))
  production_memory_json=$(jq -c --argjson anon "$conservative_anon" --argjson file "$conservative_file" \
    '{slice,parent_control_group,parent_cgroup_procs,active_child_control_groups,children,
    production_slice_memory_high_bytes,production_slice_memory_max_bytes,
    systemd_production_slice_memory_high_bytes,systemd_production_slice_memory_max_bytes,
    parent_memory_current_bytes,parent_memory_peak_bytes,parent_memory_anon_bytes,parent_memory_file_bytes,parent_memory_stat,
    child_memory_max_sum_bytes,parent_memory_events}
    | .parent_memory_stat.anon=$anon | .parent_memory_stat.file=$file' <<<"$production_snapshot_json")
  production_process_json=$(jq -c '(.children | with_entries(.value |= {main_pid,process_exe_sha256,n_restarts,active}))' <<<"$production_snapshot_json")
  if [[ -z ${production_snapshot_identity_json:-} ]]; then
    production_snapshot_identity_json=$first_identity
  fi
}
env_value() {
  local file=$1 key=$2 count value; count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || die "$file must contain exactly one ${key}= entry"
  value=$(sed -n "s/^${key}=//p" "$file"); [[ -n $value ]] || die "$file has an empty $key"; printf '%s\n' "$value"
}
run_spool_dir() {
  local run_id=$1 market=$2
  [[ $run_id =~ ^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$ ]] || return 1
  [[ $market == spot || $market == usdm ]] || return 1
  printf '%s/%s/%s\n' "$RUN_SPOOL_ROOT" "$run_id" "$market"
}
is_usdm_top100() {
  local value=$1 unique; [[ $value =~ ^[A-Z0-9]+(,[A-Z0-9]+)*$ ]] || return 1
  local -a values; IFS=, read -r -a values <<<"$value"; (( ${#values[@]} == 100 )) || return 1
  unique=$(printf '%s\n' "${values[@]}" | sort -u | wc -l); ((unique == 100))
}

candidate_release="$CONTROLLER_ROOT/$CANDIDATE_CONTROLLER"; candidate_deployment="$candidate_release/deployment"
candidate_manifest="$candidate_release/release.json"
monday_verify_controller_release "$ROOT" "$CANDIDATE_CONTROLLER" || die 'candidate controller release is not an exact immutable V2 release'
[[ $TEST_ONLY == true || $(readlink -f -- "${BASH_SOURCE[0]}") == "$candidate_deployment/host-rust-lob-shadow-gate.sh" ]] || die 'Gate must execute from candidate controller bytes'
candidate_payload=$(monday_manifest_field "$candidate_manifest" artifact_sha256)
candidate_runtime=$(monday_manifest_field "$candidate_manifest" runtime_contract_sha256)
candidate_bundle=$(monday_manifest_field "$candidate_manifest" deployment_bundle_sha256)
candidate_source=$(monday_manifest_field "$candidate_manifest" deployment_source_revision)
candidate_payload_dir="$RELEASE_ROOT/$candidate_payload"; candidate_binary="$candidate_payload_dir/binance-lob-archiver"
secure_file "$candidate_binary"; [[ -x $candidate_binary && $(sha256_file "$candidate_binary") == "$candidate_payload" ]] || die 'candidate payload identity failed'
# The production unit/env pair is part of the Gate contract even though the
# Gate process itself runs only the isolated shadow pair.
candidate_production_runtime=$(monday_verify_production_runtime_assets \
  "$ROOT" "$candidate_deployment" "$candidate_payload") \
  || die 'candidate production runtime contract failed static verification'
candidate_control_bytes_sha=$(sha256_file "$candidate_release/deployment.sha256")
candidate_control_assets='{}'
while IFS= read -r control_asset; do
  [[ -n $control_asset ]] || continue
  control_sha=$(sha256_file "$candidate_deployment/$control_asset")
  candidate_control_assets=$(jq -cn --argjson values "$candidate_control_assets" \
    --arg asset "$control_asset" --arg sha "$control_sha" '$values + {($asset):$sha}')
done < <(monday_controller_assets)

active_before=direct; before_controller=; source_mode=stable; legacy_controller=; legacy_target=; legacy_payload=; legacy_runtime=; before_payload=; before_runtime=; before_bundle=; before_source=; before_deployment=; before_production_projection=
if [[ $FROM_CONTROLLER != direct ]]; then
  active_before=$(monday_active_controller_sha "$ROOT") || die 'requested before controller is not active'
  [[ $active_before == "$FROM_CONTROLLER" ]] || die 'active pair differs from requested before controller'
  before_controller=$FROM_CONTROLLER
  before_release="$CONTROLLER_ROOT/$FROM_CONTROLLER"; before_deployment="$before_release/deployment"
  monday_verify_controller_release "$ROOT" "$FROM_CONTROLLER" || die 'before controller release is invalid'
  monday_verify_controller_projections "$ROOT" "$FROM_CONTROLLER" || die 'before controller projections are not stable'
  before_payload=$(monday_manifest_field "$before_release/release.json" artifact_sha256)
  before_runtime=$(monday_manifest_field "$before_release/release.json" runtime_contract_sha256)
  before_bundle=$(monday_manifest_field "$before_release/release.json" deployment_bundle_sha256)
  before_source=$(monday_manifest_field "$before_release/release.json" deployment_source_revision)
  [[ -L $PRODUCTION_BINARY && $(readlink -- "$PRODUCTION_BINARY") == "$CONTROLLER_ROOT/active/binance-lob-archiver" ]] \
    || die 'production binary is not the stable active projection'
  before_production_projection="$CONTROLLER_ROOT/active/binance-lob-archiver"
else
  source_mode=direct
  [[ -L $CONTROLLER_ROOT/active ]] || die 'direct bootstrap requires an existing legacy active controller'
  legacy_target=$(readlink -f -- "$CONTROLLER_ROOT/active") || die 'legacy active controller is dangling'
  legacy_controller=${legacy_target##*/}
  [[ $legacy_target == "$CONTROLLER_ROOT/$legacy_controller" ]] \
    || die 'legacy active controller is not digest-addressed'
  monday_verify_legacy_controller_release "$ROOT" "$legacy_controller" "$PRODUCTION_BINARY" \
    || die 'direct bootstrap requires an immutable v1 active controller'
  before_controller=$legacy_controller
  before_release="$legacy_target"
  # Never source or execute the legacy deployment.  C0 contributes only its
  # immutable manifest identity; all candidate control bytes come from C1.
  before_deployment=$candidate_deployment
  production_target=$(readlink -f -- "$PRODUCTION_BINARY") || die 'direct bootstrap requires a production binary'
  before_payload=$(sha256_file "$production_target") || die 'direct bootstrap requires a production binary'
  legacy_payload=$(jq -er '.artifact_sha256' "$legacy_target/release.json") || die 'legacy controller payload is invalid'
  legacy_runtime=$(jq -er '.runtime_contract_sha256' "$legacy_target/release.json") || die 'legacy controller runtime is invalid'
  [[ $before_payload == "$legacy_payload" ]] || die 'direct production does not match the legacy controller payload'
  [[ $before_payload == "$candidate_payload" ]] || die 'direct bootstrap requires P0 equal to P1'
  before_runtime=$legacy_runtime
  before_bundle=$(jq -er '.deployment_bundle_sha256' "$legacy_target/release.json") || die 'legacy controller bundle is invalid'
  before_source=$(jq -er '.deployment_source_revision' "$legacy_target/release.json") || die 'legacy controller source is invalid'
  [[ -L $PRODUCTION_BINARY && $(readlink -f -- "$PRODUCTION_BINARY") == "$production_target" ]] \
    || die 'direct production identity differs'
  before_production_projection=$(readlink -- "$PRODUCTION_BINARY")
fi

# The before runtime contract is established from every live unit/env byte,
# never from the candidate manifest.  This is especially important for direct
# bootstrap: C0's R0 must be true of the installed P0 topology before any
# shadow staging occurs.
if [[ $FROM_CONTROLLER == direct ]]; then
  # Direct bootstrap is the typed R0(v1, eight assets) -> R2(v2, nine assets)
  # migration. Always hash the legacy eight-asset view, even if a stale slice
  # happens to be present, so the candidate's V2 identity is not mistaken for
  # the immutable pre-bootstrap identity.
  live_runtime=$(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT") \
    || die 'before legacy runtime contract is missing or indirect'
elif live_runtime=$(monday_rust_lob_live_runtime_contract_sha256 "$ROOT" 2>/dev/null); then
  :
else
  die 'before runtime contract is missing or indirect'
fi
[[ $live_runtime == "$before_runtime" ]] \
  || die 'before runtime bytes differ from the immutable before controller'

production_asset_json='{}'
for asset in "${PRODUCTION_ASSETS[@]}"; do
  if [[ $asset == *.service || $asset == "$PRODUCTION_SLICE" ]]; then production_target="$SYSTEMD_ROOT/$asset"; else production_target="$CONFIG_ROOT/$asset"; fi
  if [[ -L $production_target ]]; then
    [[ $(readlink -- "$production_target") == "$CONTROLLER_ROOT/active/deployment/$asset" ]] \
      || die "installed production asset is not the stable projection: $asset"
    production_resolved=$(readlink -f -- "$production_target") \
      || die "installed production asset projection is dangling: $asset"
  else
    production_resolved=$production_target
  fi
  if [[ $FROM_CONTROLLER == direct && $asset == "$PRODUCTION_SLICE" \
    && ! -e $production_target && ! -L $production_target ]]; then
    # The direct bootstrap is the sole typed 8 -> 9 migration.  Record the
    # missing aggregate slice explicitly; Cutover installs it atomically from
    # the verified candidate release.  No other runtime asset may be absent.
    production_asset_json=$(jq -cn --argjson values "$production_asset_json" \
      --arg asset "$asset" '$values + {($asset):null}')
    continue
  fi
  regular_file "$production_resolved" || die "installed production asset is missing: $production_target"
  cmp -s "$before_deployment/$asset" "$production_resolved" \
    || die "installed production asset differs from before controller: $asset"
  production_asset_json=$(jq -cn --argjson values "$production_asset_json" --arg asset "$asset" --arg sha "$(sha256_file "$production_resolved")" '$values + {($asset):$sha}')
done

declare -A installed_asset saved_state saved_sha saved_target
declare -A candidate_asset_sha restored_asset_sha
tmp_dir=$(mktemp -d)
for asset in "${SHADOW_ASSETS[@]}"; do
  if [[ $asset == *.service || $asset == "$PRODUCTION_SLICE" ]]; then installed_asset[$asset]="$SYSTEMD_ROOT/$asset"
  else installed_asset[$asset]="$CONFIG_ROOT/$asset"; fi
  if regular_file "${installed_asset[$asset]}"; then
    saved_state[$asset]=present; saved_sha[$asset]=$(sha256_file "${installed_asset[$asset]}")
  elif [[ -L ${installed_asset[$asset]} ]]; then
    saved_target[$asset]=$(readlink -- "${installed_asset[$asset]}") || die "shadow projection is unreadable: $asset"
    [[ ${saved_target[$asset]} == "$CONTROLLER_ROOT/active/deployment/$asset" ]] \
      || die "shadow asset path is not the stable projection: $asset"
    saved_resolved=$(readlink -f -- "${installed_asset[$asset]}") || die "shadow projection is dangling: $asset"
    regular_file "$saved_resolved" || die "shadow projection target is not a file: $asset"
    saved_state[$asset]=projection; saved_sha[$asset]=$(sha256_file "$saved_resolved")
  else saved_state[$asset]=absent; saved_sha[$asset]=; fi
done
old_shadow_target=; old_shadow_target_sha256=; old_shadow_present=false
if [[ -L $SHADOW_BINARY ]]; then old_shadow_target=$(readlink -- "$SHADOW_BINARY"); old_shadow_present=true
elif [[ -e $SHADOW_BINARY ]]; then die 'shadow binary path is not a symlink'; fi
if [[ $old_shadow_present == true ]]; then
  old_shadow_target_resolved=$(readlink -f -- "$SHADOW_BINARY") || die 'existing shadow binary link is dangling'
  secure_file "$old_shadow_target_resolved"
  old_shadow_target_sha256=$(sha256_file "$old_shadow_target_resolved")
fi

# Direct bootstrap may run once on a host whose production slice predates V2
# and is still unlimited.  In that one migration mode, install a runtime-only
# lease for the duration of this Gate.  The exact previous values are retained
# in a run-scoped state file before mutation and restored on normal EXIT paths;
# the next serialized Gate also recovers an un-restored lease left by SIGKILL.
# Stable V2 Gates only verify the permanent slice and never mutate it.
bootstrap_slice_lease_mode=permanent
bootstrap_slice_lease_before_high=3072M; bootstrap_slice_lease_before_max=3584M
bootstrap_slice_lease_applied=false; bootstrap_slice_lease_restored=true
bootstrap_slice_lease_state_file=
bootstrap_slice_lease_gate_pid=
bootstrap_slice_lease_gate_starttime=
bootstrap_slice_lease_candidate_controller=
bootstrap_slice_lease_gate_script=
bootstrap_slice_lease_gate_script_sha256=
bootstrap_slice_lease_parent_control_group=
bootstrap_slice_lease_parent_current=0
bootstrap_slice_lease_parent_anon=0
write_bootstrap_slice_lease_state() {
  [[ -n ${bootstrap_slice_lease_state_file:-} ]] || return 1
  local temporary="${bootstrap_slice_lease_state_file}.tmp.$$"
  jq -cS -n \
    --arg run "$run_id" --arg slice "$PRODUCTION_SLICE" \
    --arg mode "$bootstrap_slice_lease_mode" \
    --arg before_high "$bootstrap_slice_lease_before_high" \
    --arg before_max "$bootstrap_slice_lease_before_max" \
    --arg parent_control_group "$bootstrap_slice_lease_parent_control_group" \
    --arg controller "$bootstrap_slice_lease_candidate_controller" \
    --arg gate_script "$bootstrap_slice_lease_gate_script" \
    --arg gate_script_sha "$bootstrap_slice_lease_gate_script_sha256" \
    --arg recovery_service "${bootstrap_slice_lease_recovery_service##*/}" \
    --arg recovery_timer "${bootstrap_slice_lease_recovery_timer##*/}" \
    --argjson gate_pid "$bootstrap_slice_lease_gate_pid" \
    --argjson gate_starttime "$bootstrap_slice_lease_gate_starttime" \
    --argjson parent_current "$bootstrap_slice_lease_parent_current" \
    --argjson parent_anon "$bootstrap_slice_lease_parent_anon" \
    --argjson applied "$bootstrap_slice_lease_applied" \
    --argjson restored "$bootstrap_slice_lease_restored" \
    '{schema:"monday.rust_lob_bootstrap_slice_lease.v1",run_id:$run,slice:$slice,
      mode:$mode,before_memory_high:$before_high,before_memory_max:$before_max,
      before_parent_control_group:$parent_control_group,
      before_parent_memory_current_bytes:$parent_current,
      before_parent_memory_anon_bytes:$parent_anon,
      requested_memory_high:"3072M",requested_memory_max:"3584M",
      candidate_controller_sha256:$controller,gate_script:$gate_script,
      gate_script_sha256:$gate_script_sha,gate_pid:$gate_pid,gate_starttime:$gate_starttime,
      recovery_service:$recovery_service,recovery_timer:$recovery_timer,
      applied:$applied,restored:$restored}' >"$temporary" || return 1
  chmod 0640 "$temporary" || return 1
  mv -f -- "$temporary" "$bootstrap_slice_lease_state_file"
}
read_bootstrap_slice_limits() {
  local output item key value
  local -A fields=()
  output=$(systemctl_show_many "$PRODUCTION_SLICE" 'MemoryHigh,MemoryMax') || return 1
  output=${output%$'\n'}
  while IFS= read -r item; do
    [[ $item == *=* ]] || return 1
    key=${item%%=*}; value=${item#*=}
    case $key in MemoryHigh|MemoryMax) ;; *) return 1 ;; esac
    [[ -z ${fields[$key]+x} ]] || return 1
    fields[$key]=$value
  done <<<"$output"
  [[ ${fields[MemoryHigh]+x} && ${fields[MemoryMax]+x} ]] || return 1
  printf '%s\t%s\n' "${fields[MemoryHigh]}" "${fields[MemoryMax]}"
}
read_bootstrap_slice_usage() {
  local output item key value control_group parent_path current parent_stat anon
  local -A fields=()
  [[ $TEST_ONLY == true && ${MONDAY_GATE_FIXTURE_RECORD_CALLS:-0} == 1 ]] \
    && printf 'read-bootstrap-slice-usage\n' >>"$ROOT/run/gate-fixture.calls"
  # The fixture must create the same direct cgroup files before this
  # pre-mutation read; production uses the already-mounted cgroup tree.
  [[ $TEST_ONLY == true ]] && fixture_prepare_production_cgroup
  output=$(systemctl_show_many "$PRODUCTION_SLICE" 'ControlGroup') || return 1
  output=${output%$'\n'}
  while IFS= read -r item; do
    [[ $item == *=* ]] || return 1
    key=${item%%=*}; value=${item#*=}
    [[ $key == ControlGroup ]] || return 1
    [[ -z ${fields[$key]+x} ]] || return 1
    fields[$key]=$value
  done <<<"$output"
  [[ ${#fields[@]} == 1 ]] || return 1
  control_group=${fields[ControlGroup]:-}
  [[ $control_group == "/system.slice/$PRODUCTION_SLICE" ]] || return 1
  parent_path=$(cgroup_child_path "$control_group") || return 1
  monday_path_direct "$parent_path" || return 1
  current=$(cgroup_numeric_value "$parent_path/memory.current") || return 1
  parent_stat=$(cgroup_memory_stat_json "$parent_path/memory.stat") || return 1
  anon=$(jq -er '.anon' <<<"$parent_stat") || return 1
  [[ $current =~ ^(0|[1-9][0-9]*)$ && $anon =~ ^(0|[1-9][0-9]*)$ ]] || return 1
  if (( current > 3758096384 )); then
    [[ $TEST_ONLY == true && ${MONDAY_GATE_FIXTURE_RECORD_CALLS:-0} == 1 ]] \
      && printf 'usage-over-cap-current\n' >>"$ROOT/run/gate-fixture.calls"
    return 1
  fi
  if (( anon > 3758096384 )); then
    [[ $TEST_ONLY == true && ${MONDAY_GATE_FIXTURE_RECORD_CALLS:-0} == 1 ]] \
      && printf 'usage-over-cap-anon\n' >>"$ROOT/run/gate-fixture.calls"
    return 1
  fi
  printf '%s\t%s\t%s\n' "$control_group" "$current" "$anon"
}
install_bootstrap_lease_recovery() {
  [[ $TEST_ONLY == true ]] && return 0
  local service_name=${bootstrap_slice_lease_recovery_service##*/}
  local timer_name=${bootstrap_slice_lease_recovery_timer##*/}
  local candidate_gate_script="$candidate_deployment/host-rust-lob-shadow-gate.sh"
  local temporary_service="${bootstrap_slice_lease_recovery_service}.tmp.$$"
  local temporary_timer="${bootstrap_slice_lease_recovery_timer}.tmp.$$"
  monday_file_direct "$candidate_gate_script" || return 1
  [[ ! -e $bootstrap_slice_lease_recovery_service && ! -L $bootstrap_slice_lease_recovery_service \
    && ! -e $bootstrap_slice_lease_recovery_timer && ! -L $bootstrap_slice_lease_recovery_timer ]] || return 1
  printf '[Unit]\nDescription=Recover Monday Rust LOB bootstrap slice lease (%s)\n\n[Service]\nType=oneshot\nExecStart=%s --recover-bootstrap-lease %s\nTimeoutStartSec=120\n' \
    "$run_id" "$candidate_gate_script" "$bootstrap_slice_lease_state_file" >"$temporary_service" || return 1
  # Run once after a short grace period and again after each completed
  # watchdog service invocation.  The recovery service itself returns while
  # the recorded Gate owner is alive, so a healthy Gate never loses its lease.
  printf '[Unit]\nDescription=Recover Monday Rust LOB bootstrap slice lease (%s)\n\n[Timer]\nOnActiveSec=30s\nOnUnitInactiveSec=30s\nAccuracySec=1s\nUnit=%s\n' \
    "$run_id" "$service_name" >"$temporary_timer" || return 1
  chmod 0644 "$temporary_service" "$temporary_timer" || return 1
  mv -f -- "$temporary_service" "$bootstrap_slice_lease_recovery_service" || return 1
  mv -f -- "$temporary_timer" "$bootstrap_slice_lease_recovery_timer" || return 1
  systemctl daemon-reload >/dev/null 2>&1 || return 1
  systemctl start "$timer_name" >/dev/null 2>&1 || return 1
}
cancel_bootstrap_lease_recovery() {
  [[ $TEST_ONLY == true ]] && return 0
  local service_name=${bootstrap_slice_lease_recovery_service##*/}
  local timer_name=${bootstrap_slice_lease_recovery_timer##*/}
  if [[ -e $bootstrap_slice_lease_recovery_service || -L $bootstrap_slice_lease_recovery_service \
    || -e $bootstrap_slice_lease_recovery_timer || -L $bootstrap_slice_lease_recovery_timer ]]; then
    systemctl stop "$timer_name" >/dev/null 2>&1 || true
    systemctl stop "$service_name" >/dev/null 2>&1 || true
    systemctl reset-failed "$timer_name" >/dev/null 2>&1 || true
    systemctl reset-failed "$service_name" >/dev/null 2>&1 || true
    rm -f -- "$bootstrap_slice_lease_recovery_timer" "$bootstrap_slice_lease_recovery_service"
    systemctl daemon-reload >/dev/null 2>&1 || return 1
  fi
}
apply_bootstrap_slice_lease() {
  [[ $FROM_CONTROLLER == direct ]] || return 0
  local limits before_high before_max usage parent_control parent_current parent_anon
  limits=$(read_bootstrap_slice_limits) || die 'production slice limits are unavailable before Gate'
  IFS=$'\t' read -r before_high before_max <<<"$limits"
  [[ $before_high =~ ^(infinity|[0-9]+)$ && $before_max =~ ^(infinity|[0-9]+)$ ]] \
    || die 'production slice limits are malformed before Gate'
  if [[ $before_high == 3221225472 || $before_high == 3072M ]] &&
     [[ $before_max == 3758096384 || $before_max == 3584M ]]; then
    bootstrap_slice_lease_mode=permanent
    bootstrap_slice_lease_before_high=$before_high
    bootstrap_slice_lease_before_max=$before_max
    bootstrap_slice_lease_applied=false
    bootstrap_slice_lease_restored=true
    return 0
  fi
  if [[ $TEST_ONLY == true && ${MONDAY_GATE_FIXTURE_BOOTSTRAP_LEASE_USAGE:-0} != 1 ]]; then
    die 'fixture production slice must expose the governed V2 limits'
  fi
  [[ $before_high == infinity || $before_high == 0 || $before_high == max ]] \
    || die 'direct bootstrap refuses an unexpected production MemoryHigh'
  [[ $before_max == infinity || $before_max == max ]] \
    || die 'direct bootstrap refuses an unexpected production MemoryMax'
  # Read the exact production parent usage before arming recovery or changing
  # either limit.  Current and anon are both bounded independently so a
  # high-current legacy slice cannot be hidden behind a smaller anon value.
  usage=$(read_bootstrap_slice_usage) \
    || die 'direct bootstrap could not read the production slice usage before Gate'
  IFS=$'\t' read -r parent_control parent_current parent_anon <<<"$usage"
  bootstrap_slice_lease_parent_control_group=$parent_control
  bootstrap_slice_lease_parent_current=$parent_current
  bootstrap_slice_lease_parent_anon=$parent_anon
  # Mark the lease as pending before the mutating call so an abnormal exit
  # between set-property and the bookkeeping assignments still enters the
  # EXIT cleanup path with the original limits available for restoration.
  bootstrap_slice_lease_mode='temporary-bootstrap'
  bootstrap_slice_lease_before_high=$before_high
  bootstrap_slice_lease_before_max=$before_max
  bootstrap_slice_lease_applied=true
  bootstrap_slice_lease_restored=false
  bootstrap_slice_lease_gate_pid=$$
  bootstrap_slice_lease_gate_starttime=$(proc_starttime "$$") \
    || die 'could not record the Gate owner starttime for lease recovery'
  bootstrap_slice_lease_candidate_controller=$CANDIDATE_CONTROLLER
  bootstrap_slice_lease_gate_script="$candidate_deployment/host-rust-lob-shadow-gate.sh"
  monday_file_direct "$bootstrap_slice_lease_gate_script" \
    || die 'candidate Gate script is not a direct file for lease recovery'
  bootstrap_slice_lease_gate_script_sha256=$(sha256_file "$bootstrap_slice_lease_gate_script") \
    || die 'could not record candidate Gate script identity for lease recovery'
  [[ $TEST_ONLY == true && ${MONDAY_GATE_FIXTURE_RECORD_CALLS:-0} == 1 ]] \
    && printf 'write-lease-state\n' >>"$ROOT/run/gate-fixture.calls"
  write_bootstrap_slice_lease_state \
    || die 'could not persist the temporary production slice lease'
  [[ $TEST_ONLY == true && ${MONDAY_GATE_FIXTURE_RECORD_CALLS:-0} == 1 ]] \
    && printf 'install-lease-recovery\n' >>"$ROOT/run/gate-fixture.calls"
  install_bootstrap_lease_recovery \
    || die 'could not arm automatic bootstrap lease recovery'
  systemctl set-property --runtime "$PRODUCTION_SLICE" \
    MemoryHigh=3072M MemoryMax=3584M >/dev/null \
    || die 'could not apply the temporary production slice lease'
  limits=$(read_bootstrap_slice_limits) || die 'temporary production slice lease could not be read back'
  IFS=$'\t' read -r before_high before_max <<<"$limits"
  [[ $before_high == 3221225472 || $before_high == 3072M ]] \
    || die 'temporary production MemoryHigh lease is not exact'
  [[ $before_max == 3758096384 || $before_max == 3584M ]] \
    || die 'temporary production MemoryMax lease is not exact'
}
restore_bootstrap_slice_lease() {
  [[ ${bootstrap_slice_lease_mode:-permanent} == temporary-bootstrap ]] || return 0
  [[ ${bootstrap_slice_lease_restored:-false} == true ]] && return 0
  [[ $TEST_ONLY != true ]] || return 1
  systemctl set-property --runtime "$PRODUCTION_SLICE" \
    "MemoryHigh=$bootstrap_slice_lease_before_high" \
    "MemoryMax=$bootstrap_slice_lease_before_max" >/dev/null 2>&1 || return 1
  local limits restored_high restored_max
  limits=$(read_bootstrap_slice_limits) || return 1
  IFS=$'\t' read -r restored_high restored_max <<<"$limits"
  [[ $restored_high == "$bootstrap_slice_lease_before_high" \
    && $restored_max == "$bootstrap_slice_lease_before_max" ]] || return 1
  cancel_bootstrap_lease_recovery || return 1
  bootstrap_slice_lease_restored=true
  write_bootstrap_slice_lease_state || return 1
}

# Candidate shadow units are rendered into a run-scoped /run directory.  The
# source template may only contribute the reviewed security/resource fields;
# all identity, spool, restart, and lifetime fields are rewritten below.
verify_shadow_unit_template() {
  local file=$1
  monday_file_direct "$file" || return 1
  monday_validate_unit_allowlist "$file" shadow || return 1
  monday_unit_exact_line "$file" Type simple || return 1
  monday_unit_exact_line "$file" User hftcollector || return 1
  monday_unit_exact_line "$file" Group hftcollector || return 1
  monday_unit_exact_line "$file" Restart always || return 1
  monday_unit_exact_line "$file" RestartSec 5 || return 1
  monday_unit_exact_line "$file" RuntimeMaxSec 21600 || return 1
  monday_unit_exact_line "$file" KillMode mixed || return 1
  monday_unit_exact_line "$file" TimeoutStopSec 600 || return 1
  monday_unit_exact_line "$file" NoNewPrivileges true || return 1
  monday_unit_exact_line "$file" PrivateTmp true || return 1
  monday_unit_exact_line "$file" ProtectSystem strict || return 1
  monday_unit_exact_line "$file" ProtectHome true || return 1
  monday_unit_exact_line "$file" ProtectKernelTunables true || return 1
  monday_unit_exact_line "$file" ProtectKernelModules true || return 1
  monday_unit_exact_line "$file" ProtectControlGroups true || return 1
  monday_unit_exact_line "$file" LockPersonality true || return 1
  monday_unit_exact_line "$file" RestrictSUIDSGID true || return 1
  monday_unit_exact_line "$file" StateDirectory hft-collector || return 1
  monday_unit_exact_line "$file" ReadWritePaths /data/monday/spool/binance-lob-rust-shadow || return 1
  monday_unit_exact_line "$file" CPUQuota '80%' || return 1
  monday_unit_exact_line "$file" OOMScoreAdjust 500 || return 1
  monday_unit_exact_line "$file" MemoryHigh '1792M' || return 1
  monday_unit_exact_line "$file" MemoryMax '2048M' || return 1
  [[ $(grep -c '^ExecStart=' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'ExecStart=/opt/monday/bin/binance-lob-archiver-shadow' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'ExecStartPre=/opt/monday/bin/binance-lob-archiver-shadow --self-test' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'EnvironmentFile=/etc/monday/binance-lob-archiver-rust-%i.env' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'EnvironmentFile=-/run/monday/binance-lob-archiver-rust-%i-soak.env' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -c '^EnvironmentFile=' "$file" || true) -eq 2 ]] || return 1
}

declare -A market_env spool_dir candidate_shadow_spool dataset symbols unit upload_unit expected_oss_prefix
declare -A candidate_upload_unit_sha
declare -A oss_bucket oss_endpoint oss_region aliyun_profile
declare -A phase_segments_json phase_triplets_json phase_health_json
markets=(spot usdm)
for market in "${markets[@]}"; do
  market_env[$market]="$candidate_deployment/binance-lob-archiver-rust-${market}.env"
  dataset[$market]=$(env_value "${market_env[$market]}" DATASET); symbols[$market]=$(env_value "${market_env[$market]}" SYMBOLS)
  oss_bucket[$market]=$(env_value "${market_env[$market]}" OSS_BUCKET)
  oss_endpoint[$market]=$(env_value "${market_env[$market]}" OSS_ENDPOINT)
  oss_region[$market]=$(env_value "${market_env[$market]}" OSS_REGION)
  aliyun_profile[$market]=$(env_value "${market_env[$market]}" ALIYUN_PROFILE)
  [[ $(env_value "${market_env[$market]}" MARKET) == "$market" ]] || die "$market env has wrong market"
  candidate_shadow_spool[$market]=$(env_value "${market_env[$market]}" SPOOL_DIR)
  [[ ${candidate_shadow_spool[$market]} != /data/monday/spool/binance-lob \
    && ${candidate_shadow_spool[$market]} != /data/monday/spool/binance-lob/* ]] \
    || die "$market shadow spool overlaps production spool"
  [[ $(env_value "${market_env[$market]}" SHARD_ID) == all ]] \
    || die "$market shadow SHARD_ID must be all"
  [[ ${oss_bucket[$market]} == monday-lob-apne1-1045353359 ]] \
    || die "$market shadow OSS bucket is not the production bucket"
  [[ ${oss_endpoint[$market]} == oss-ap-northeast-1-internal.aliyuncs.com ]] \
    || die "$market shadow OSS endpoint is not the internal Tokyo endpoint"
  [[ ${oss_region[$market]} == ap-northeast-1 ]] || die "$market shadow OSS region is not Tokyo"
  [[ ${aliyun_profile[$market]} == ecs-role ]] || die "$market shadow OSS profile is not ecs-role"
  verify_shadow_unit_template "$candidate_deployment/binance-lob-archiver-rust@.service" \
    || die 'candidate shadow service template failed security/resource verification'
  spool_dir[$market]=""
  unit[$market]=""
done
if [[ $TEST_ONLY != true ]]; then
  [[ ${symbols[spot]} == ALL && ${dataset[spot]} == spot_all_rust_shadow ]] || die 'Spot identity is invalid'
  is_usdm_top100 "${symbols[usdm]}" || die 'USD-M catalog is not frozen'
  [[ ${dataset[usdm]} == usdm_perpetual_top100_lob_rust_shadow ]] || die 'USD-M dataset identity is invalid'
fi
for market in "${markets[@]}"; do
  phase_segments_json[$market]='[]'
  phase_triplets_json[$market]='[]'
  phase_health_json[$market]='{}'
done

host_memory_total=$(meminfo_bytes MemTotal) || die 'MemTotal is unavailable'; host_memory_available=$(meminfo_bytes MemAvailable) || die 'MemAvailable is unavailable'; host_swap_total=$(meminfo_bytes SwapTotal) || die 'SwapTotal is unavailable'

resource_samples='[]'; psi_windows='[]'; resource_monitor_pid=; resource_monitor_control=; resource_monitor_log=; resource_monitor_phase=
strict_unit_seq=0
oss_unit_seq=0
declare -A resource_phase_required resource_phase_limit resource_phase_parent_current resource_phase_child_sum resource_phase_growth
record_resource() {
  local phase=$1 phase_max=$2 required sample now available_before available_after
  available_before=$(meminfo_bytes MemAvailable) || die 'MemAvailable became unavailable during Gate'
  refresh_production_snapshot || die "production cgroup identity drifted before $phase admission"
  available_after=$(meminfo_bytes MemAvailable) || die 'MemAvailable became unavailable during Gate'
  host_memory_available=$available_before
  (( available_after < host_memory_available )) && host_memory_available=$available_after
  required=$(monday_shadow_memory_admission "$host_memory_available" "$HOST_MEMORY_RESERVE_BYTES" "$phase_max" \
    "$production_parent_anon" "$production_slice_memory_max") || die "insufficient memory for $phase"
  resource_phase_required[$phase]=$required
  resource_phase_limit[$phase]=$phase_max
  resource_phase_parent_current[$phase]=$production_parent_current
  resource_phase_child_sum[$phase]=$production_child_max_sum
  resource_phase_growth[$phase]=$production_growth
  now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  sample=$(jq -cn --arg phase "$phase" --argjson available "$host_memory_available" --argjson required "$required" --argjson phase_max "$phase_max" \
    --argjson available_before "$available_before" --argjson available_after "$available_after" \
    --argjson reserve "$HOST_MEMORY_RESERVE_BYTES" --argjson current "$production_parent_current" \
    --argjson anon "$production_parent_anon" --argjson file "$production_parent_file" \
    --argjson slice_max "$production_slice_memory_max" --argjson child_sum "$production_child_max_sum" \
    --argjson growth "$production_growth" --arg now "$now" \
    '{phase:$phase,started_at:$now,ended_at:$now,samples:1,host_memory_available_bytes:$available,
      host_memory_available_before_bytes:$available_before,host_memory_available_after_bytes:$available_after,
      max_memory_available_bytes:$available,current_memory_available_bytes:$available,breach:false,
      host_memory_reserve_bytes:$reserve,production_parent_memory_current_bytes:$current,
      production_parent_memory_anon_bytes:$anon,production_parent_memory_file_bytes:$file,
      production_slice_memory_max_bytes:$slice_max,
      production_child_memory_max_sum_bytes:$child_sum,production_memory_growth_bytes:$growth,
      production_unallocated_bytes:$growth,
      required_bytes:$required,phase_memory_max_bytes:$phase_max}')
  resource_samples=$(jq -cn --argjson values "$resource_samples" --argjson value "$sample" '$values + [$value]')
}
verify_gate_worker_slice() {
  local output item key value
  local -A fields=()
  output=$(systemctl_show_many "$GATE_WORKER_SLICE" 'MemoryHigh,MemoryMax,ControlGroup') || return 1
  output=${output%$'\n'}
  while IFS= read -r item; do
    [[ $item == *=* ]] || return 1
    key=${item%%=*}; value=${item#*=}
    case $key in MemoryHigh|MemoryMax|ControlGroup) ;; *) return 1 ;; esac
    [[ -z ${fields[$key]+x} ]] || return 1
    fields[$key]=$value
  done <<<"$output"
  [[ ${#fields[@]} == 3 ]] || return 1
  [[ ${fields[MemoryHigh]:-} == "$GATE_WORKER_MEMORY_HIGH_BYTES" \
    && ${fields[MemoryMax]:-} == "$GATE_WORKER_MEMORY_MAX_BYTES" \
    && ${fields[ControlGroup]:-} == "/$GATE_WORKER_SLICE" ]] || return 1
  gate_worker_slice_control_group=${fields[ControlGroup]}
}
resource_monitor_identity_guard() {
  local snapshot=$1 snapshot_identity
  if [[ -z $snapshot ]] || ! monday_validate_lob_production_snapshot "$snapshot" 2>/dev/null; then
    printf 'production-snapshot-invalid\n' >"$tmp_dir/resource-monitor-breach"
    return 1
  fi
  snapshot_identity=$(monday_lob_production_snapshot_identity "$snapshot" 2>/dev/null || true)
  if [[ -z $snapshot_identity || $snapshot_identity != "$production_snapshot_identity_json" ]]; then
    printf 'production-identity-drift\n' >"$tmp_dir/resource-monitor-breach"
    return 1
  fi
}
resource_monitor_start() {
  local phase=$1 phase_max=$2 initial_available initial_psi parent_pid parent_starttime
  resource_monitor_phase=$phase
  record_resource "$phase" "$phase_max"
  verify_gate_worker_slice || die "run-scoped Gate worker slice is not an exact live envelope before $phase"
  if [[ $TEST_ONLY == true ]]; then
    # The fixture normally skips the asynchronous monitor, but this hook
    # exercises the same identity guard synchronously before a writer starts.
    if [[ ${MONDAY_GATE_FIXTURE_IDENTITY_DRIFT:-0} == 1 ]]; then
      MONDAY_GATE_FIXTURE_PRODUCTION_RESTARTS=9
      local snapshot
      snapshot=$(capture_production_snapshot 2>/dev/null || true)
      resource_monitor_identity_guard "$snapshot" || die "production identity drifted before $phase"
    fi
    return 0
  fi
  resource_monitor_control="$tmp_dir/resource-monitor-$phase.running"
  resource_monitor_log="$tmp_dir/resource-monitor-$phase.tsv"
  : >"$resource_monitor_control"; : >"$resource_monitor_log"
  initial_available=$(meminfo_bytes MemAvailable) || die 'MemAvailable became unavailable during Gate'
  initial_psi=$(io_total_us 2>/dev/null || printf 0)
  printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$initial_available" "$initial_psi" >"$resource_monitor_log"
  parent_pid=$$
  parent_starttime=$(proc_starttime "$parent_pid") || die 'resource monitor parent starttime is unavailable'
  printf '%s %s\n' "$parent_pid" "$parent_starttime" >"$tmp_dir/resource-monitor-$phase.parent"
  (
    local previous_psi=$initial_psi available current_psi consecutive_hits=0 delta current_parent_starttime
    local snapshot
    while [[ -e $resource_monitor_control ]]; do
      current_parent_starttime=$(proc_starttime "$parent_pid" 2>/dev/null || true)
      if ! kill -0 "$parent_pid" 2>/dev/null || [[ -z $current_parent_starttime || $current_parent_starttime != "$parent_starttime" ]]; then
        printf 'parent-disappeared\n' >"$tmp_dir/resource-monitor-parent-exit"
        break
      fi
      available=$(meminfo_bytes MemAvailable 2>/dev/null || printf 0)
      current_psi=$(io_total_us 2>/dev/null || printf 0)
      printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$available" "$current_psi" >>"$resource_monitor_log"
      snapshot=$(capture_production_snapshot 2>/dev/null || true)
      if ! resource_monitor_identity_guard "$snapshot"; then
        kill -TERM "$parent_pid" 2>/dev/null || true
        break
      fi
      if (( available < HOST_MEMORY_RESERVE_BYTES )); then
        printf 'memory-breach\n' >"$tmp_dir/resource-monitor-breach"
        kill -TERM "$parent_pid" 2>/dev/null || true
        break
      fi
      if (( current_psi < previous_psi )); then
        printf 'psi-regressed\n' >"$tmp_dir/resource-monitor-breach"
        kill -TERM "$parent_pid" 2>/dev/null || true
        break
      fi
      delta=$((current_psi - previous_psi))
      if (( delta * 3 >= IO_PSI_FULL_DELTA_LIMIT_US )); then
        consecutive_hits=$((consecutive_hits + 1))
      else
        consecutive_hits=0
      fi
      if (( consecutive_hits >= IO_PSI_CONSECUTIVE_HIT_LIMIT )); then
        printf 'psi-stop-rule\n' >"$tmp_dir/resource-monitor-breach"
        kill -TERM "$parent_pid" 2>/dev/null || true
        break
      fi
      previous_psi=$current_psi
      sleep 5
    done
  ) &
  resource_monitor_pid=$!
}
resource_monitor_stop() {
  local phase=$resource_monitor_phase ended samples max_available current_available breach required phase_max parent_pid parent_starttime
  local parent_current child_sum growth
  [[ -n ${resource_monitor_pid:-} ]] || return 0
  if [[ $TEST_ONLY == true ]]; then
    resource_monitor_pid=; resource_monitor_phase=; return 0
  fi
  rm -f -- "$resource_monitor_control"
  wait "$resource_monitor_pid" 2>/dev/null || true
  ended=$(tail -n1 "$resource_monitor_log" | cut -f1)
  samples=$(wc -l <"$resource_monitor_log" | tr -d ' ')
  read -r parent_pid parent_starttime <"$tmp_dir/resource-monitor-$phase.parent" || true
  max_available=$(awk -F '\t' 'BEGIN{m=0} $2>m{m=$2} END{print m+0}' "$resource_monitor_log")
  current_available=$(tail -n1 "$resource_monitor_log" | cut -f2)
  required=${resource_phase_required[$phase]:-1}
  phase_max=${resource_phase_limit[$phase]:-1}
  parent_current=${resource_phase_parent_current[$phase]:-0}
  child_sum=${resource_phase_child_sum[$phase]:-0}
  growth=${resource_phase_growth[$phase]:-0}
  breach=false; [[ -f $tmp_dir/resource-monitor-breach ]] && breach=true
  [[ $breach == false ]] || die "resource monitor breached during $phase"
  resource_samples=$(jq -cn --argjson prior "$resource_samples" --arg phase "$phase" --arg ended "$ended" \
    --argjson samples "${samples:-0}" --argjson max "${max_available:-0}" --argjson current "${current_available:-0}" \
    --argjson required "${required:-1}" --argjson phase_max "${phase_max:-1}" \
    --argjson reserve "$HOST_MEMORY_RESERVE_BYTES" --argjson parent_current "$parent_current" \
    --argjson child_sum "$child_sum" --argjson growth "$growth" \
    --arg parent_pid "${parent_pid:-}" --arg parent_starttime "${parent_starttime:-}" \
    '$prior | map(if .phase == $phase then . + {
      ended_at:$ended,samples:$samples,
      max_memory_available_bytes:$max,current_memory_available_bytes:$current,
      breach:false,host_memory_reserve_bytes:$reserve,
      production_parent_memory_current_bytes:$parent_current,
      production_child_memory_max_sum_bytes:$child_sum,
      production_memory_growth_bytes:$growth,required_bytes:$required,
      phase_memory_max_bytes:$phase_max,
      parent_pid:($parent_pid|if length == 0 then null else tonumber end),
      parent_proc_starttime:($parent_starttime|if length == 0 then null else tonumber end)
    } else . end)')
  unset "resource_phase_required[$phase]" "resource_phase_limit[$phase]" \
    "resource_phase_parent_current[$phase]" "resource_phase_child_sum[$phase]" "resource_phase_growth[$phase]"
  resource_monitor_pid=; resource_monitor_phase=
}
calibrate_psi() {
  local phase=$1 previous current transition delta ratio hit consecutive=0 i
  if [[ $TEST_ONLY == true ]]; then
    fixture_last_calibrated_phase=$phase
    psi_windows=$(jq -cn --argjson values "$psi_windows" --arg phase "$phase" '$values + [{phase:$phase,stage:"fixture",hit:false,consecutive_hits:0}]'); return
  fi
  previous=$(io_total_us) || die "I/O PSI unavailable before $phase"
  for i in 1 2 3; do sleep "$IO_PSI_WINDOW_SECONDS"; current=$(io_total_us) || die "I/O PSI unavailable during $phase"
    transition=$(monday_io_full_psi_window "$previous" "$current" "$IO_PSI_WINDOW_US" "$IO_PSI_WINDOW_US" "$IO_PSI_FULL_DELTA_LIMIT_US" "$consecutive") || die 'I/O PSI moved backwards'
    read -r delta ratio hit consecutive <<<"$transition"; [[ $hit == false || $consecutive -lt $IO_PSI_CONSECUTIVE_HIT_LIMIT ]] || die 'I/O PSI threshold exceeded'
    psi_windows=$(jq -cn --argjson values "$psi_windows" --arg phase "$phase" --argjson delta "$delta" --argjson ratio "$ratio" --argjson hit "$hit" --argjson consecutive "$consecutive" '$values + [{phase:$phase,stage:"calibration",delta_us:$delta,ratio:$ratio,hit:$hit,consecutive_hits:$consecutive}]'); previous=$current
  done
}
assert_host_memory_reserve() {
  local available
  [[ $TEST_ONLY == true ]] && return 0
  available=$(meminfo_bytes MemAvailable) || die 'MemAvailable became unavailable during Gate'
  ((available >= HOST_MEMORY_RESERVE_BYTES)) || die "host memory reserve was consumed: available=$available reserve=$HOST_MEMORY_RESERVE_BYTES"
}

run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
evidence_dir="$EVIDENCE_ROOT/$CANDIDATE_CONTROLLER/$candidate_runtime/runs/$run_id"; gate_json="$evidence_dir/gate.json"; passed_marker="$evidence_dir/PASSED.sha256"; run_spool="$RUN_SPOOL_ROOT/$run_id"; run_json="$evidence_dir/run.json"
gate_unit_dir="$GATE_UNIT_ROOT/$run_id"
bootstrap_slice_lease_state_file="$GATE_UNIT_ROOT/bootstrap-slice-lease-$run_id.json"
bootstrap_slice_lease_recovery_service="$GATE_SYSTEMD_ROOT/${GATE_UNIT_PREFIX}${run_id}-lease-recovery.service"
bootstrap_slice_lease_recovery_timer="$GATE_SYSTEMD_ROOT/${GATE_UNIT_PREFIX}${run_id}-lease-recovery.timer"
# All Gate workers share one run-scoped hard envelope.  The production pair
# remains in its separately governed permanent slice; this transient slice is
# removed by the EXIT cleanup and is never treated as a production asset.
# A dash in a systemd slice name denotes a hierarchy component.  Use a
# digits-only run suffix so this aggregate remains one top-level cgroup.
GATE_WORKER_SLICE="mondayrustlobgate${run_id//[^0-9]/}.slice"
gate_worker_slice_file="$GATE_SYSTEMD_ROOT/$GATE_WORKER_SLICE"
GATE_WORKER_MEMORY_HIGH_BYTES=1342177280
GATE_WORKER_MEMORY_MAX_BYTES=1610612736
gate_worker_slice_control_group=

# BSD/GNU install only applies -m to the leaf when creating a nested path.
# Create each shadow spool component explicitly so the collector user can
# traverse a fresh tree without widening the parent permissions.
ensure_run_spool_dir "$DATA_ROOT/spool/binance-lob-rust-shadow"
ensure_run_spool_dir "$RUN_SPOOL_ROOT"
ensure_run_spool_dir "$run_spool"
ensure_run_spool_dir "$GATE_UNIT_ROOT"

# A killed Gate cannot run its EXIT trap.  On the next serialized Gate, only
# our own run-scoped names are stopped and removed; production units, links,
# and /etc bytes are never addressed by this cleanup.
valid_gate_transient_unit() {
  local candidate_unit_name=$1
  [[ $candidate_unit_name =~ ^${GATE_UNIT_PREFIX}[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*-(spot|usdm|spot-upload|usdm-upload|strict-[1-9][0-9]*|oss-(spot|usdm)-[1-9][0-9]*)\.service$ ]]
}
valid_gate_transient_slice() {
  local candidate_slice_name=$1
  [[ $candidate_slice_name =~ ^mondayrustlobgate[0-9]{15,}\.slice$ ]]
}
cleanup_stale_gate_units() {
  local listed unit unit_file
  [[ $TEST_ONLY == true ]] && return 0
  listed=$(systemctl list-units --all --type=service --no-legend --plain \
    "${GATE_UNIT_PREFIX}*.service") || return 1
  while read -r unit _; do
    [[ -n ${unit:-} ]] || continue
    valid_gate_transient_unit "$unit" || continue
    systemctl stop "$unit" >/dev/null 2>&1 || true
    systemctl reset-failed "$unit" >/dev/null 2>&1 || true
  done <<<"$listed"
  direct_directory "$GATE_SYSTEMD_ROOT" || return 1
  while IFS= read -r unit_file; do
    unit=${unit_file##*/}
    valid_gate_transient_unit "$unit" || continue
    rm -f -- "$unit_file"
  done < <(find "$GATE_SYSTEMD_ROOT" -maxdepth 1 -type f -name 'monday-rust-lob-gate-*.service' -print)
  while IFS= read -r unit_file; do
    unit=${unit_file##*/}
    valid_gate_transient_slice "$unit" || continue
    systemctl stop "$unit" >/dev/null 2>&1 || true
    rm -f -- "$unit_file"
  done < <(find "$GATE_SYSTEMD_ROOT" -maxdepth 1 -type f -name 'mondayrustlobgate*.slice' -print)
  systemctl daemon-reload >/dev/null 2>&1 || return 1
}
cleanup_stale_gate_runs() {
  local dir old old_spool unit_file unit market
  [[ -n $RUN_SPOOL_ROOT ]] || return 1
  [[ -d $GATE_UNIT_ROOT ]] || return 0
  while IFS= read -r dir; do
    [[ -n $dir && $dir != "$gate_unit_dir" ]] || continue
    old=${dir##*/}
    [[ $old =~ ^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$ ]] || continue
    direct_directory "$dir" || return 1
    old_spool="$RUN_SPOOL_ROOT/$old"
    if [[ -e $old_spool || -L $old_spool ]]; then
      direct_directory "$old_spool" || return 1
    fi
    while IFS= read -r unit_file; do
      unit=${unit_file##*/}; unit=${unit%.service}
      valid_gate_transient_unit "$unit.service" || continue
      systemctl stop "$unit.service" >/dev/null 2>&1 || true
      systemctl reset-failed "$unit.service" >/dev/null 2>&1 || true
    done < <(find "$dir" -maxdepth 1 -type f -name 'monday-rust-lob-gate-*.service' -print)
    for market in spot usdm; do
      rm -f -- "$GATE_SYSTEMD_ROOT/monday-rust-lob-gate-${old}-${market}.service" \
        "$GATE_SYSTEMD_ROOT/monday-rust-lob-gate-${old}-${market}-upload.service"
    done
    while IFS= read -r unit_file; do
      unit=${unit_file##*/}
      valid_gate_transient_slice "$unit" || continue
      systemctl stop "$unit" >/dev/null 2>&1 || true
      rm -f -- "$unit_file"
    done < <(find "$GATE_SYSTEMD_ROOT" -maxdepth 1 -type f -name 'mondayrustlobgate*.slice' -print)
    rm -rf -- "$dir" "$old_spool"
  done < <(find "$GATE_UNIT_ROOT" -mindepth 1 -maxdepth 1 -type d -print)
  [[ $TEST_ONLY == true ]] || systemctl daemon-reload >/dev/null 2>&1 || return 1
}
cleanup_stale_bootstrap_leases() {
  [[ $TEST_ONLY == true ]] && return 0
  local state
  direct_directory "$GATE_UNIT_ROOT" || return 1
  while IFS= read -r state; do
    [[ -n $state ]] || continue
    recover_bootstrap_lease "$state" || return 1
  done < <(find "$GATE_UNIT_ROOT" -maxdepth 1 -type f -name 'bootstrap-slice-lease-*.json' -print | sort)
}
cleanup_stale_gate_units
cleanup_stale_gate_runs
cleanup_stale_bootstrap_leases
# Every Gate, including production, writes only to this run-scoped spool.  A
# prior test-only conditional left production's spool_dir empty and made the
# first install attempt fail before any market work; keep construction before
# the common install path so both modes exercise the same topology.
for market in "${markets[@]}"; do
  spool_dir[$market]=$(run_spool_dir "$run_id" "$market")
done
install -d -m 0750 "$EVIDENCE_ROOT" "$evidence_dir"
install -d -m 0755 "$GATE_SYSTEMD_ROOT"
ensure_run_spool_dir "$gate_unit_dir"
for market in "${markets[@]}"; do
  ensure_run_spool_dir "${spool_dir[$market]}"
done
if [[ ${MONDAY_GATE_FIXTURE_PATH_ONLY:-0} != 1 ]]; then
  while IFS= read -r prior_receipt; do
    if jq -e '(.schema | test("^monday[.]rust_lob_shadow_gate[.]v[0-9]+$")) and .passed == true' "$prior_receipt" >/dev/null 2>&1; then
      die 'a passed Gate receipt already exists for this controller identity'
    fi
  done < <(find "$EVIDENCE_ROOT/$CANDIDATE_CONTROLLER/$candidate_runtime" -type f -name gate.json -print)
fi
gate_started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ); gate_finished=false
declare -A phase_pid phase_exe_sha phase_session phase_segments phase_oss phase_runtime
declare -A phase_strict_lob phase_strict_aggregate phase_strict_raw
declare -A market_gate_started_ns market_observation_started_ns frozen_symbol_count frozen_catalog_sha256
declare -A market_observed_at_ns
declare -A initial_upload_failure_count last_health_updated_ns last_health_advance_mono
declare -A max_health_silence_seconds health_samples
write_run_json() {
  jq -cn --arg run "$run_id" --arg controller "$CANDIDATE_CONTROLLER" --arg payload "$candidate_payload" --arg runtime "$candidate_runtime" --arg spool "$run_spool" --arg worker_slice "$GATE_WORKER_SLICE" --arg worker_slice_sha "$gate_worker_slice_sha256" --arg worker_slice_cgroup "$gate_worker_slice_control_group" --argjson worker_high "$GATE_WORKER_MEMORY_HIGH_BYTES" --argjson worker_max "$GATE_WORKER_MEMORY_MAX_BYTES" --argjson requested "$GATE_DURATION_SECONDS" --argjson settle "$HEALTH_SETTLE_DURATION_SECONDS" --argjson resources "$resource_samples" --argjson psi "$psi_windows" \
    '{schema:"monday.rust_lob_shadow_gate_run.v3",control_plane_version:2,run_id:$run,candidate_controller_sha256:$controller,candidate_payload_sha256:$payload,candidate_runtime_contract_sha256:$runtime,run_spool:$spool,worker_slice:{name:$worker_slice,sha256:$worker_slice_sha,cgroup:$worker_slice_cgroup,memory_high_bytes:$worker_high,memory_max_bytes:$worker_max},segment_seconds:120,requested_duration_seconds:$requested,health_settle_seconds:$settle,resource_admission:$resources,io_full_psi_windows:$psi}' >"$run_json.tmp"
  chmod 0640 "$run_json.tmp"; mv -f -- "$run_json.tmp" "$run_json"
}
cleanup() {
  local status=$? cleanup_failed=false; set +e
  resource_monitor_stop >/dev/null 2>&1 || cleanup_failed=true
  for market in "${markets[@]}"; do
    [[ -n ${unit[$market]:-} ]] || continue
    systemctl stop "${unit[$market]}" >/dev/null 2>&1 || true
    rm -f -- "$GATE_SYSTEMD_ROOT/${unit[$market]}" \
      "$GATE_SYSTEMD_ROOT/monday-rust-lob-gate-${run_id}-${market}-upload.service"
  done
  restore_bootstrap_slice_lease || cleanup_failed=true
  systemctl stop "$GATE_WORKER_SLICE" >/dev/null 2>&1 || true
  rm -f -- "$gate_worker_slice_file"
  [[ $TEST_ONLY == true ]] || systemctl daemon-reload >/dev/null 2>&1 || cleanup_failed=true
  rm -rf -- "$gate_unit_dir" "$run_spool" "$tmp_dir"
  [[ $gate_finished == true ]] || rm -f -- "$passed_marker" "$evidence_dir/.PASSED.sha256.tmp"
  [[ $cleanup_failed == false ]] || { printf 'run-scoped Gate cleanup was incomplete\n' >&2; status=1; }; exit "$status"
}
trap cleanup EXIT; trap 'exit 143' HUP INT TERM

# Install a run-scoped aggregate slice only after the EXIT cleanup trap is
# armed.  If a write, daemon-reload, or start fails, the same cleanup path
# removes this slice and any workers without requiring a second handler.
[[ ! -e $gate_worker_slice_file && ! -L $gate_worker_slice_file ]] \
  || die 'run-scoped Gate worker slice already exists'
printf '[Slice]\nMemoryHigh=1280M\nMemoryMax=1536M\n' >"$gate_worker_slice_file"
chmod 0644 "$gate_worker_slice_file"
[[ $(grep -Fxc 'MemoryHigh=1280M' "$gate_worker_slice_file" || true) -eq 1 \
  && $(grep -Fxc 'MemoryMax=1536M' "$gate_worker_slice_file" || true) -eq 1 \
  && $(grep -c '^' "$gate_worker_slice_file" || true) -eq 3 ]] \
  || die 'run-scoped Gate worker slice envelope is invalid'
gate_worker_slice_sha256=$(sha256_file "$gate_worker_slice_file")
[[ $gate_worker_slice_sha256 =~ ^[a-f0-9]{64}$ ]] || die 'run-scoped Gate worker slice digest is invalid'
[[ $TEST_ONLY == true ]] || systemctl daemon-reload >/dev/null 2>&1 || die 'could not load run-scoped Gate worker slice'
systemctl start "$GATE_WORKER_SLICE" >/dev/null 2>&1 \
  || die 'could not activate run-scoped Gate worker slice'
verify_gate_worker_slice || die 'run-scoped Gate worker slice did not read back exactly'

# Apply the bootstrap lease only after stale Gate workers and any interrupted
# lease have been cleaned, and after the EXIT trap is armed.  A failure or
# signal after this point therefore restores the exact pre-Gate limits.
apply_bootstrap_slice_lease
if [[ $TEST_ONLY == true ]]; then
  refresh_production_snapshot || die 'fixture production cgroup snapshot is invalid'
else
  refresh_production_snapshot || die 'production cgroup snapshot is invalid'
fi

# Fixture-only path coverage reaches the same run-scoped preparation without
# opening sockets, invoking OSS, or starting an external market process.
if [[ $TEST_ONLY == true && ${MONDAY_GATE_FIXTURE_PATH_ONLY:-0} == 1 ]]; then
  for path in "$DATA_ROOT/spool/binance-lob-rust-shadow" "$RUN_SPOOL_ROOT" \
    "$run_spool" "$gate_unit_dir" "${spool_dir[spot]}" "${spool_dir[usdm]}"; do
    [[ $(directory_mode "$path") == 750 ]] || die "fixture run directory mode is not 0750: $path"
  done
  for market in "${markets[@]}"; do
    [[ ${spool_dir[$market]} == "$run_spool/$market" ]] \
      || die "$market fixture Gate spool path is not run-scoped"
  done
  printf 'V2 Gate spool preparation: %s permissions=750\n' "$run_spool"
  exit 0
fi

calibrate_psi preflight; resource_monitor_start preflight "$STRICT_VERIFIER_MEMORY_MAX_BYTES"; resource_monitor_stop; write_run_json
if [[ $FROM_CONTROLLER != direct ]]; then
  for asset in "${SHADOW_ASSETS[@]}"; do
    [[ ${saved_state[$asset]} == present || ${saved_state[$asset]} == projection ]] || die "before shadow asset is absent: $asset"
    shadow_resolved=${installed_asset[$asset]}
    [[ ${saved_state[$asset]} == projection ]] && shadow_resolved=$(readlink -f -- "$shadow_resolved")
    cmp -s "$before_deployment/$asset" "$shadow_resolved" || die "installed shadow asset differs from before controller: $asset"
  done
else
  for asset in "${SHADOW_ASSETS[@]}"; do
    [[ ${saved_state[$asset]} == present || ${saved_state[$asset]} == projection ]] || die "direct bootstrap shadow asset is absent: $asset"
    shadow_resolved=${installed_asset[$asset]}
    [[ ${saved_state[$asset]} == projection ]] && shadow_resolved=$(readlink -f -- "$shadow_resolved")
    cmp -s "$candidate_deployment/$asset" "$shadow_resolved" || die "direct bootstrap installed shadow asset differs: $asset"
  done
fi

fixture_seed_market() {
  local market=$1 dir="${spool_dir[$1]}" i file data_sha now; [[ $TEST_ONLY == true ]] || return 0
  now=$(monotonic_seconds); mkdir -p "$dir"
  jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" --arg session "fixture-$run_id-$market" --argjson updated "$((now * 1000000000))" '{market:$market,dataset:$dataset,updated_at_ns:$updated,status:"synced",sequence_gaps:0,symbol_count:1,symbols:{FIXTURE:{}},snapshot_ready_count:1,bridged_count:1,stream_coverage_verified_count:1,snapshot_only_symbols:[],all_symbols_bridged:true,all_stream_coverage_verified:true,full_stream_coverage_verified:true,queue_saturated:false,disk_warning:false,upload_warning:false,upload_failure_count:0,session_id:$session}' >"$dir/health.json"
  for i in 1 2; do
    file="part-$((now+i)).jsonl"; printf '{"schema":"binance.market_tape.v2","type":"session_start"}\n' >"$dir/$file"; zstd -q -f "$dir/$file" -o "$dir/$file.zst"; rm -f -- "$dir/$file"; file="$file.zst"; data_sha=$(sha256_file "$dir/$file")
    jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" --arg file "$file" --arg sha "$data_sha" --arg session "fixture-$run_id-$market" --argjson start "$((now+i*1000))" --argjson end "$((now+i*1000+900))" '{schema:"binance.market_tape.v2",market:$market,dataset:$dataset,shard_id:"all",start_received_at_ns:$start,end_received_at_ns:$end,file:$file,sha256:$sha,symbols:["FIXTURE"],stream_types:["depth@100ms"],event_types:{agg_trade:0,raw_trade:0,book_ticker:0,force_order:0},has_replay_safe_checkpoint:true,lob_continuity:{sequence_gaps:0,reconnect_boundary:false,capture_session_id:$session}}' >"$dir/$file.manifest.json"
    printf '%s\n' "$data_sha" >"$dir/$file._SUCCESS"
  done
}
render_shadow_unit() {
  local market=$1 source_unit source_upload source_env rendered_unit rendered_upload rendered_env spool canonical_upload
  source_unit="$candidate_deployment/binance-lob-archiver-rust@.service"
  source_upload="$candidate_deployment/binance-lob-archiver-rust-upload@.service"
  source_env="$candidate_deployment/binance-lob-archiver-rust-${market}.env"
  rendered_unit="$gate_unit_dir/monday-rust-lob-gate-${run_id}-${market}.service"
  rendered_upload="$gate_unit_dir/monday-rust-lob-gate-${run_id}-${market}-upload.service"
  rendered_env="$gate_unit_dir/monday-rust-lob-gate-${run_id}-${market}.env"
  spool="${spool_dir[$market]}"
  monday_validate_unit_allowlist "$source_upload" shadow_upload \
    || die 'candidate shadow upload template failed security/resource verification'
  [[ -n $spool && $spool == "$run_spool/$market" ]] || die "$market Gate spool is not run-scoped"
  sed -e '/^EnvironmentFile=-\/run\/monday\/binance-lob-archiver-rust-%i-soak.env$/d' \
      -e "/^\[Service\]$/a\\
Slice=$GATE_WORKER_SLICE" \
      -e "s|^EnvironmentFile=/etc/monday/binance-lob-archiver-rust-%i.env$|EnvironmentFile=$rendered_env|" \
      -e "s|^ExecStartPre=.*$|ExecStartPre=$candidate_binary --self-test|" \
      -e "s|^ExecStart=.*$|ExecStart=$candidate_binary|" \
      -e 's|^Restart=.*$|Restart=no|' \
      -e 's|^RuntimeMaxSec=.*$|RuntimeMaxSec=1800|' \
      -e "s|^ReadWritePaths=.*$|ReadWritePaths=$spool|" \
      "$source_unit" >"$rendered_unit"
  # shellcheck disable=SC2086
  sed -e '/^\[Service\]$/a\
Slice='$GATE_WORKER_SLICE'\
Restart=no\
RuntimeMaxSec=1800' \
      -e "s|^EnvironmentFile=.*$|EnvironmentFile=$rendered_env|" \
      -e "s|^ExecStart=.*$|ExecStart=$candidate_binary --upload-only|" \
      -e "s|^ReadWritePaths=.*$|ReadWritePaths=$spool|" \
      "$source_upload" >"$rendered_upload"
  sed -e "s|^SPOOL_DIR=.*$|SPOOL_DIR=$spool|" "$source_env" >"$rendered_env"
  chmod 0640 "$rendered_unit" "$rendered_upload" "$rendered_env"
  [[ $(grep -Fxc "EnvironmentFile=$rendered_env" "$rendered_unit" || true) -eq 1 ]] || die "$market Gate unit env path is not exact"
  [[ $(grep -Fxc 'Restart=no' "$rendered_unit" || true) -eq 1 ]] || die "$market Gate unit restart policy is not bounded"
  [[ $(grep -Fxc 'RuntimeMaxSec=1800' "$rendered_unit" || true) -eq 1 ]] || die "$market Gate unit runtime is not bounded"
  [[ $(grep -Fxc "ReadWritePaths=$spool" "$rendered_unit" || true) -eq 1 ]] || die "$market Gate unit spool is not exact"
  [[ $(grep -Fxc "EnvironmentFile=$rendered_env" "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload env path is not exact"
  [[ $(grep -Fxc "ExecStart=$candidate_binary --upload-only" "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload identity is not exact"
  [[ $(grep -Fxc "ReadWritePaths=$spool" "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload spool is not exact"
  [[ $(grep -Fxc "Slice=$GATE_WORKER_SLICE" "$rendered_unit" || true) -eq 1 ]] || die "$market Gate worker slice is not exact"
  [[ $(grep -Fxc "Slice=$GATE_WORKER_SLICE" "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload worker slice is not exact"
  [[ $(grep -Fxc 'Restart=no' "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload restart policy is not bounded"
  [[ $(grep -Fxc 'RuntimeMaxSec=1800' "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload runtime is not bounded"
  canonical_upload="$tmp_dir/$market-shadow-upload-source.service"
  sed -e "/^Slice=$GATE_WORKER_SLICE$/d" \
      -e "s|^EnvironmentFile=$rendered_env$|EnvironmentFile=/etc/monday/binance-lob-archiver-rust-%i.env|" \
      -e "s|^ExecStart=$candidate_binary --upload-only$|ExecStart=/opt/monday/bin/binance-lob-archiver-shadow --upload-only|" \
      -e "s|^ReadWritePaths=$spool$|ReadWritePaths=/data/monday/spool/binance-lob-rust-shadow|" \
      "$rendered_upload" >"$canonical_upload"
  monday_validate_unit_allowlist "$canonical_upload" shadow_upload_run \
    || die "$market rendered shadow upload unit failed security/resource verification"
  [[ $(grep -Fxc "SPOOL_DIR=$spool" "$rendered_env" || true) -eq 1 ]] || die "$market Gate env spool is not exact"
  [[ $TEST_ONLY == true ]] || systemd-analyze verify "$rendered_unit" "$rendered_upload" || die "$market Gate unit failed systemd-analyze verify"
  unit[$market]="monday-rust-lob-gate-${run_id}-${market}.service"
  # systemd does not search the private evidence directory.  Install a
  # run-scoped copy under its runtime search path, then remove it in the EXIT
  # trap; the rendered bytes remain bound by candidate_asset_sha above.
  local search_unit="$GATE_SYSTEMD_ROOT/${unit[$market]}"
  local search_upload="$GATE_SYSTEMD_ROOT/monday-rust-lob-gate-${run_id}-${market}-upload.service"
  [[ ! -e $search_unit && ! -L $search_unit ]] \
    || die "$market run-scoped Gate unit already exists"
  [[ ! -e $search_upload && ! -L $search_upload ]] \
    || die "$market run-scoped upload unit already exists"
  install -m 0644 "$rendered_unit" "$search_unit"
  install -m 0644 "$rendered_upload" "$search_upload"
  upload_unit[$market]="monday-rust-lob-gate-${run_id}-${market}-upload.service"
  candidate_upload_unit_sha[$market]=$(sha256_file "$rendered_upload")
  candidate_asset_sha["binance-lob-archiver-rust@.service"]=$(sha256_file "$rendered_unit")
  candidate_asset_sha["binance-lob-archiver-rust-upload@.service"]=$(sha256_file "$rendered_upload")
  candidate_asset_sha["binance-lob-archiver-rust-${market}.env"]=$(sha256_file "$rendered_env")
}
run_strict_verifier() {
  if [[ $TEST_ONLY == true ]]; then
    local expect_path=false argument
    for argument in "$@"; do
      if [[ $expect_path == true ]]; then
        [[ -f $argument && ! -L $argument ]] || return 1
        expect_path=false
      elif [[ $argument == --verify-segment ]]; then
        expect_path=true
      fi
    done
    [[ $expect_path == false ]]
    return
  fi
  strict_unit_seq=$((strict_unit_seq + 1))
  systemd-run --quiet --wait --collect \
    --unit="${GATE_UNIT_PREFIX}${run_id}-strict-${strict_unit_seq}.service" \
    --slice="$GATE_WORKER_SLICE" \
    --property=MemoryMax=1536M --property=MemoryHigh=1280M \
    --property=OOMScoreAdjust=500 --property=Restart=no --property=RuntimeMaxSec=1800 \
    --uid="$SERVICE_USER" -- "$candidate_binary" "$@"
}
run_strict_verifier_pair() { run_strict_verifier --require-lob-continuity "$@"; }
verify_adjacent_segments() {
  local -a args=(); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do
    path=$1; digest=$2; manifest_digest=$3; shift 3
    args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest")
  done
  run_strict_verifier_pair "${args[@]}"
}
verify_aggregate_trade_continuity() {
  local -a args=(--verify-aggregate-trade-continuity); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do path=$1; digest=$2; manifest_digest=$3; shift 3; args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest"); done
  run_strict_verifier "${args[@]}"
}
verify_raw_trade_continuity() {
  local -a args=(--verify-raw-trade-continuity); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do path=$1; digest=$2; manifest_digest=$3; shift 3; args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest"); done
  run_strict_verifier "${args[@]}"
}
systemctl_value() { systemctl_show "${unit[$1]}" "$2"; }
health_ok() {
  local market=$1 health="${spool_dir[$1]}/health.json"; [[ -f $health ]] || return 1
  if [[ $TEST_ONLY == true ]]; then jq -e --arg market "$market" '.market == $market and .status == "synced" and .sequence_gaps == 0' "$health" >/dev/null; return; fi
  local minimum_symbols=1000
  [[ $market == usdm ]] && minimum_symbols=100
  jq -e --arg market "$market" --arg dataset "${dataset[$market]}" \
    --arg symbols "${symbols[$market]}" --argjson minimum "$minimum_symbols" \
    --argjson started "${market_gate_started_ns[$market]}" \
    '.market == $market
      and .dataset == $dataset
      and (.updated_at_ns | type) == "number"
      and .updated_at_ns >= $started
      and .status == "synced"
      and .sequence_gaps == 0
      and (.symbol_count | type) == "number"
      and .symbol_count >= $minimum
      and .snapshot_ready_count == .symbol_count
      and .bridged_count == .symbol_count
      and .stream_coverage_verified_count == .symbol_count
      and .snapshot_only_symbols == []
      and .all_symbols_bridged == true
      and .all_stream_coverage_verified == true
      and (.full_stream_coverage_verified == null or .full_stream_coverage_verified == true)
      and .queue_saturated == false
      and .disk_warning == false
      and .upload_warning == false
      and .upload_failure_count == 0
      and (.session_id | type) == "string"
      and (.session_id | length) > 0
      and (if $market == "usdm" then (.symbols | keys | sort) == ($symbols | split(",") | sort) else true end)' \
    "$health" >/dev/null
}

health_catalog_sha256() {
  local market=$1
  jq -c '.symbols | keys | sort' "${spool_dir[$market]}/health.json" \
    | sha256sum | awk '{print $1}'
}

validate_observation_sample() {
  local market=$1 health="${spool_dir[$1]}/health.json" session symbols_now catalog upload_failures updated_ns current_mono
  health_ok "$market" || die "$market health failed during observation"
  session=$(jq -er '.session_id' "$health")
  [[ $session == "${phase_session[$market]}" ]] || die "$market collector session changed during observation"
  symbols_now=$(jq -er '.symbol_count' "$health")
  [[ $symbols_now == "${frozen_symbol_count[$market]}" ]] || die "$market symbol catalog changed during observation"
  catalog=$(health_catalog_sha256 "$market")
  [[ $catalog == "${frozen_catalog_sha256[$market]}" ]] || die "$market catalog membership changed during observation"
  upload_failures=$(jq -er '.upload_failure_count' "$health")
  [[ $upload_failures == "${initial_upload_failure_count[$market]}" ]] || die "$market upload failures changed during observation"
  updated_ns=$(jq -er '.updated_at_ns' "$health")
  current_mono=$(monotonic_seconds)
  if [[ $TEST_ONLY == true ]]; then
    last_health_updated_ns[$market]=$updated_ns
    last_health_advance_mono[$market]=$current_mono
    return 0
  fi
  local next_updated next_mono next_gap increment
  read -r next_updated next_mono next_gap increment < <(
    monday_observe_health_freshness \
      "${last_health_updated_ns[$market]}" \
      "${last_health_advance_mono[$market]}" \
      "${max_health_silence_seconds[$market]}" \
      "$updated_ns" "$current_mono" "$MAX_HEALTH_SILENCE_SECONDS"
  ) || die "$market health timestamp regressed or stopped advancing"
  last_health_updated_ns[$market]=$next_updated
  last_health_advance_mono[$market]=$next_mono
  max_health_silence_seconds[$market]=$next_gap
  health_samples[$market]=$((health_samples[$market] + increment))
}
verify_segments() {
  local market=$1 dir="${spool_dir[$1]}" path file digest manifest_digest success_digest expected_success count=0 previous_end=0 start end segment_json
  local -a segment_records=()
  phase_segments_json[$market]='[]'
  while IFS= read -r path; do
    file=${path##*/}; digest=$(sha256_file "$path"); expected_success="$tmp_dir/$market-$file.expected-success"; printf '%s\n' "$digest" >"$expected_success"; cmp -s "$path._SUCCESS" "$expected_success" || die "$market _SUCCESS digest mismatch"
    success_digest=$(sha256_file "$path._SUCCESS")
    manifest_digest=$(sha256_file "$path.manifest.json")
    jq -e --arg market "$market" --arg digest "$digest" --arg session "${phase_session[$market]}" \
      '.schema == "binance.market_tape.v2" and .market == $market and .sha256 == $digest
       and .has_replay_safe_checkpoint == true and .lob_continuity.sequence_gaps == 0
       and .lob_continuity.reconnect_boundary == false
       and .lob_continuity.capture_session_id == $session' \
      "$path.manifest.json" >/dev/null || die "$market manifest failed strict checks"
    start=$(jq -er '.start_received_at_ns' "$path.manifest.json"); end=$(jq -er '.end_received_at_ns' "$path.manifest.json"); ((previous_end == 0 || start >= previous_end)) || die "$market segments overlap"; ((previous_end == 0 || start-previous_end <= MAX_SEGMENT_GAP_NS)) || die "$market segment gap is too large"; previous_end=$end; count=$((count+1))
    segment_json=$(jq -cn --arg file "$file" --arg path "$path" --arg data_sha "$digest" \
      --arg manifest_sha "$manifest_digest" --arg success_sha "$success_digest" \
      --argjson start "$start" --argjson end "$end" --arg session "${phase_session[$market]}" \
      '{file:$file,path:$path,data_sha256:$data_sha,manifest_sha256:$manifest_sha,
        success_sha256:$success_sha,start_received_at_ns:$start,end_received_at_ns:$end,
        session_id:$session}')
    phase_segments_json[$market]=$(jq -cn --argjson values "${phase_segments_json[$market]}" \
      --argjson value "$segment_json" '$values + [$value]')
    segment_records+=("$path" "$digest" "$manifest_digest")
  done < <(find "$dir" -maxdepth 1 -type f -name '*.jsonl.zst' | sort)
  ((count >= 2)) || die "$market has fewer than two complete segments"
  verify_adjacent_segments "${segment_records[@]}" || die "$market strict LOB continuity verifier failed"
  if [[ $market == spot ]]; then
    verify_aggregate_trade_continuity "${segment_records[@]}" || die "$market strict aggregate-trade continuity verifier failed"
    verify_raw_trade_continuity "${segment_records[@]}" || die "$market strict raw-trade continuity verifier failed"
    phase_strict_aggregate[$market]=true
    phase_strict_raw[$market]=true
  else
    phase_strict_aggregate[$market]=false
    phase_strict_raw[$market]=false
  fi
  phase_strict_lob[$market]=true
  phase_segments["$market"]=$count
}
run_market_gate_phase() {
  local market=$1 settle observation pid started_ns
  calibrate_psi "shadow-$market"; resource_monitor_start "shadow-$market" "$STRICT_VERIFIER_MEMORY_MAX_BYTES"; fixture_seed_market "$market"; systemctl reset-failed "${unit[$market]}" >/dev/null 2>&1 || true
  started_ns=$(date +%s%N); market_gate_started_ns[$market]=$started_ns
  systemctl start "${unit[$market]}"; systemctl_active "${unit[$market]}" || die "$market shadow did not start"; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market shadow restarted"; pid=$(systemctl_value "$market" MainPID); [[ $pid =~ ^[1-9][0-9]*$ ]] || die "$market MainPID unavailable"; phase_pid["$market"]=$pid; phase_exe_sha["$market"]=$candidate_payload
  if [[ $TEST_ONLY == true && ( ${MONDAY_GATE_FIXTURE_SIGKILL:-0} == 1 || ${MONDAY_GATE_HARD_CRASH_AFTER_SHADOW_START:-0} == 1 ) && $market == spot ]]; then
    kill -KILL "$$"
  fi
  if [[ $TEST_ONLY != true ]]; then
    exe_path=$(readlink -f -- "$PROC_ROOT/$pid/exe") || die "$market process executable is unavailable"
    [[ $(sha256_file "$exe_path") == "$candidate_payload" ]] || die "$market process executable identity differs from P1"
  fi
  settle=$(( $(monotonic_seconds) + HEALTH_SETTLE_DURATION_SECONDS )); while ! health_ok "$market"; do (( $(monotonic_seconds) < settle )) || die "$market health did not settle"; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market restarted while settling"; [[ $(systemctl_value "$market" MainPID) == "$pid" ]] || die "$market MainPID changed while settling"; assert_host_memory_reserve; sleep 1; done
  phase_session["$market"]=$(jq -er '.session_id' "${spool_dir[$market]}/health.json"); frozen_symbol_count[$market]=$(jq -er '.symbol_count' "${spool_dir[$market]}/health.json"); frozen_catalog_sha256[$market]=$(health_catalog_sha256 "$market"); initial_upload_failure_count[$market]=$(jq -er '.upload_failure_count' "${spool_dir[$market]}/health.json"); last_health_updated_ns[$market]=$(jq -er '.updated_at_ns' "${spool_dir[$market]}/health.json"); last_health_advance_mono[$market]=$(monotonic_seconds); max_health_silence_seconds[$market]=0; health_samples[$market]=1; market_observation_started_ns[$market]=$(date +%s%N)
  phase_health_json[$market]=$(jq -cn --arg sha "$(sha256_file "${spool_dir[$market]}/health.json")" \
    --arg session "${phase_session[$market]}" --argjson symbols "${frozen_symbol_count[$market]}" \
    --arg catalog "${frozen_catalog_sha256[$market]}" --argjson silence "${max_health_silence_seconds[$market]}" \
    --argjson samples "${health_samples[$market]}" \
    '{sha256:$sha,session_id:$session,frozen_symbol_count:$symbols,
      frozen_catalog_sha256:$catalog,max_health_silence_seconds:$silence,samples:$samples}')
  observation=$(( $(monotonic_seconds) + GATE_DURATION_SECONDS )); while (( $(monotonic_seconds) < observation )); do validate_observation_sample "$market"; assert_host_memory_reserve; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market restarted"; [[ $(systemctl_value "$market" MainPID) == "$pid" ]] || die "$market MainPID changed"; [[ $TEST_ONLY == true ]] && break; sleep 15; done
  phase_runtime["$market"]=$GATE_DURATION_SECONDS; systemctl stop "${unit[$market]}"; systemctl_active "${unit[$market]}" && die "$market shadow remained active"; resource_monitor_stop; calibrate_psi "shadow-$market-tail"; resource_monitor_start "strict-verifier-$market" "$STRICT_VERIFIER_MEMORY_MAX_BYTES"; verify_segments "$market"; resource_monitor_stop
}
DRAIN_ENV_KEYS=(
  MARKET DATASET SHARD_ID SYMBOLS DEPTH_MODE WS_SHARD_SIZE SNAPSHOT_LIMIT
  SNAPSHOT_REQUESTS_PER_SECOND SNAPSHOT_RETRY_ATTEMPTS
  SYNC_TIMEOUT_SECONDS STALL_TIMEOUT_SECONDS PROCESS_WATCHDOG_SECONDS
  TASK_CANCEL_TIMEOUT_SECONDS
  MAX_BUFFERED_DIFFS MAX_PENDING_DIFFS_TOTAL MIN_FREE_GB ZSTD_TIMEOUT_SECONDS
  OSS_COPY_TIMEOUT_SECONDS SEGMENT_SECONDS OSS_BUCKET OSS_ENDPOINT OSS_REGION
  ALIYUN_PROFILE BINANCE_REST_BASE LOG_LEVEL
)
declare -a candidate_drain_env_args=()
load_candidate_drain_env() {
  local market=$1 file=${market_env[$1]} spool=${spool_dir[$1]} key value
  candidate_drain_env_args=()
  [[ -n $spool && $spool == "$run_spool/$market" ]] || die "$market drain spool is not run-scoped"
  for key in "${DRAIN_ENV_KEYS[@]}"; do
    value=$(env_value "$file" "$key") || die "$market candidate env has no unique $key"
    candidate_drain_env_args+=("$key=$value")
  done
  if grep -q '^SNAPSHOT_PRODUCERS=' "$file"; then
    value=$(env_value "$file" SNAPSHOT_PRODUCERS) || die "$market candidate env has no unique SNAPSHOT_PRODUCERS"
    candidate_drain_env_args+=("SNAPSHOT_PRODUCERS=$value")
  fi
  # SPOOL_DIR is the one deliberate projection: the candidate env is used as
  # the source of every identity/timeout value, while the drain is forced into
  # this Gate's isolated spool instead of inheriting production's default.
  candidate_drain_env_args+=("SPOOL_DIR=$spool")
}
run_candidate_drain() {
  local market=$1
  resource_monitor_start "upload-drain-$market" "$UPLOAD_DRAIN_MEMORY_MAX_BYTES"
  # Build and validate the exact environment even in fixtures.  Returning
  # before this step previously masked a production-only default-spool bug.
  load_candidate_drain_env "$market"
  if [[ $TEST_ONLY == true ]]; then
    printf '%s\n' "${candidate_drain_env_args[@]}" >"$tmp_dir/$market-drain-env"
    jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" \
      --arg session "${phase_session[$market]}" \
      '{market:$market,dataset:$dataset,last_error:null,session_id:$session,uploaded_triplets:1}' \
      >"${spool_dir[$market]}/upload-status.json"
    resource_monitor_stop
    return 0
  fi
  systemctl reset-failed "${upload_unit[$market]}" >/dev/null 2>&1 || true
  systemctl start "${upload_unit[$market]}" \
    || { resource_monitor_stop; die "$market run-scoped upload drain failed"; }
  [[ $(systemctl show "${upload_unit[$market]}" --property=Result --value) == success ]] \
    || { resource_monitor_stop; die "$market run-scoped upload drain did not complete successfully"; }
  resource_monitor_stop
}
assert_spool_drained() {
  local market=$1 remaining
  remaining=$(find "${spool_dir[$market]}" \( -type f -o -type l \) \( \
    -name '*.jsonl.part' -o -name '*.zst.tmp' -o -name '*.part.corrupt' -o \
    -name '*.uploaded-cleanup.json' -o -name '*.uploaded-cleanup.json.tmp' \
  \) -print -quit)
  [[ -z $remaining ]] || die "$market spool contains an unsealed or pending artifact: $remaining"
}
run_oss() {
  local market=$1; shift
  if [[ $TEST_ONLY == true ]]; then
    OSS_FIXTURE_MARKET=$market aliyun ossutil "$@" --profile "${aliyun_profile[$market]}" --endpoint fixture --region ap-northeast-1
  else
    [[ -x /usr/local/bin/aliyun ]] || die 'trusted OSS CLI is missing: /usr/local/bin/aliyun'
    oss_unit_seq=$((oss_unit_seq + 1))
    # --pipe is required: OSS listing/readback is streamed through this
    # helper and its exit status must remain the systemd-run status.
    systemd-run --quiet --pipe --wait --collect \
      --unit="${GATE_UNIT_PREFIX}${run_id}-oss-${market}-${oss_unit_seq}.service" \
      --slice="$GATE_WORKER_SLICE" \
      --property=MemoryMax=1536M --property=MemoryHigh=1280M \
      --property=OOMScoreAdjust=500 --property=Restart=no --property=RuntimeMaxSec=1800 \
      -- runuser --user "$SERVICE_USER" -- env -i HOME="$SERVICE_HOME" PATH="$SAFE_PATH" \
      ALIYUN_PROFILE="${aliyun_profile[$market]}" /usr/local/bin/aliyun ossutil "$@" \
      --profile "${aliyun_profile[$market]}" --endpoint "${oss_endpoint[$market]}" \
      --region "${oss_region[$market]}"
  fi
}
verify_oss_roundtrips() {
  local market=$1 readback="$tmp_dir/oss-$1" listing="$tmp_dir/oss-$1.list" uri manifest final_manifest data success expected_success file digest manifest_digest success_digest line token replay_safe triplet_json observed_at observed_cutoff_ns
  local expected_bucket manifest_name object_prefix data_uri success_uri expected_file
  local candidates_file unsafe_file
  resource_monitor_start "oss-readback-$market" "$STRICT_VERIFIER_MEMORY_MAX_BYTES"
  observed_cutoff_ns=$(date +%s%N)
  [[ $observed_cutoff_ns =~ ^[0-9]+$ ]] || die "$market OSS observation clock is unavailable"
  local start end previous_end=0 count=0; local -a roundtrip_records=(); mkdir -p "$readback"
  phase_triplets_json[$market]='[]'
  candidates_file="$readback/replay-safe.tsv"; unsafe_file="$readback/replay-unsafe.tsv"
  : >"$candidates_file"; : >"$unsafe_file"
  local prefix
  if [[ $TEST_ONLY == true ]]; then
    prefix="oss://fixture/lake/raw/venue=binance/market=$market/dataset=${dataset[$market]}/shard=all/"
  else
    prefix="oss://${oss_bucket[$market]}/lake/raw/venue=binance/market=$market/dataset=${dataset[$market]}/shard=$(env_value "${market_env[$market]}" SHARD_ID)/"
  fi
  if [[ $TEST_ONLY == true ]]; then
    expected_oss_prefix[$market]=${prefix#oss://fixture/}
  else
    expected_oss_prefix[$market]=${prefix#oss://"${oss_bucket[$market]}"/}
  fi
  expected_oss_prefix[$market]=${expected_oss_prefix[$market]%/}
  monday_validate_oss_prefix "$market" "${dataset[$market]}" "${expected_oss_prefix[$market]}" \
    || die "$market OSS base prefix is invalid"
  expected_bucket=${oss_bucket[$market]}
  [[ $TEST_ONLY == true ]] && expected_bucket=fixture
  run_oss "$market" ls "$prefix" --recursive --short-format >"$listing"
  : >"$readback/manifest-uris"
  while IFS= read -r line; do
    line=${line%$'\r'}
    if [[ $line =~ (oss://[^[:space:]]+\.manifest\.json) ]]; then
      printf '%s\n' "${BASH_REMATCH[1]}" >>"$readback/manifest-uris"
      continue
    fi
    token=${line##*[$' \t']}; token=${token#/}
    [[ $token == *.manifest.json ]] && printf 'oss://%s/%s\n' "${oss_bucket[$market]}" "$token" >>"$readback/manifest-uris"
  done <"$listing"
  sort -u -o "$readback/manifest-uris" "$readback/manifest-uris"
  while IFS= read -r uri; do
    [[ -n $uri ]] || continue
    manifest_name=${uri##*/}
    monday_validate_lob_object_uri "$market" "${dataset[$market]}" "$expected_bucket" "$uri" manifest \
      || die "$market OSS manifest URI failed strict validation: $uri"
    object_prefix=${uri#"oss://$expected_bucket/"}
    object_prefix=${object_prefix%/"$manifest_name"}
    [[ $object_prefix == "${expected_oss_prefix[$market]}"/* ]] \
      || die "$market OSS manifest URI is outside the configured base prefix: $uri"
    manifest="$readback/discovered-$count.json"; run_oss "$market" cp "$uri" "$manifest" --force --no-progress >/dev/null
    jq -e --arg market "$market" \
      '.market == $market
       and (.start_received_at_ns | type == "number" and floor == . and . >= 0)
       and (.end_received_at_ns | type == "number" and floor == . and . >= 0)
       and .end_received_at_ns >= .start_received_at_ns
       and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))' \
      "$manifest" >/dev/null || die "$market OSS manifest failed strict verification"
    start=$(jq -er '.start_received_at_ns' "$manifest"); end=$(jq -er '.end_received_at_ns' "$manifest")
    [[ $start =~ ^[0-9]+$ && $end =~ ^[0-9]+$ && $end -le $observed_cutoff_ns ]] \
      || die "$market OSS manifest is future-dated"
    if [[ $TEST_ONLY != true ]] && ((end <= market_observation_started_ns[$market])); then
      continue
    fi
    jq -e --arg session "${phase_session[$market]}" \
      '.schema == "binance.market_tape.v2"
       and (.has_replay_safe_checkpoint | type == "boolean") and .lob_continuity.sequence_gaps == 0
       and .lob_continuity.reconnect_boundary == false
       and .lob_continuity.capture_session_id == $session
       and (.file | type == "string" and test("^part-[0-9]+\\.jsonl\\.zst$"))' \
      "$manifest" >/dev/null || die "$market OSS manifest failed strict verification"
    replay_safe=$(jq -er '.has_replay_safe_checkpoint' "$manifest")
    if [[ $replay_safe != true ]]; then
      printf '%s\t%s\t%s\n' "$start" "$end" "$uri" >>"$unsafe_file"
      continue
    fi
    printf '%s\t%s\t%s\n' "$start" "$end" "$uri" >>"$candidates_file"
    ((previous_end == 0 || start >= previous_end)) || die "$market OSS segments overlap"; ((previous_end == 0 || start - previous_end <= MAX_SEGMENT_GAP_NS)) || die "$market OSS continuity gap exceeded"; previous_end=$end
    expected_file=${manifest_name%.manifest.json}
    file=$(jq -er '.file' "$manifest"); [[ $file == "$expected_file" ]] \
      || die "$market OSS manifest filename does not match its object URI"
    data_uri="oss://$expected_bucket/$object_prefix/$file"
    success_uri="oss://$expected_bucket/$object_prefix/$file._SUCCESS"
    monday_validate_lob_object_uri "$market" "${dataset[$market]}" "$expected_bucket" "$data_uri" data \
      || die "$market OSS data URI failed strict validation: $data_uri"
    monday_validate_lob_object_uri "$market" "${dataset[$market]}" "$expected_bucket" "$success_uri" success \
      || die "$market OSS success URI failed strict validation: $success_uri"
    digest=$(jq -er '.sha256' "$manifest"); manifest_digest=$(sha256_file "$manifest")
    data="$readback/$file"; success="$data._SUCCESS"; final_manifest="$data.manifest.json"
    run_oss "$market" cp "$uri" "$final_manifest" --force --no-progress >/dev/null
    [[ $(sha256_file "$final_manifest") == "$manifest_digest" ]] || die "$market OSS manifest changed between reads"
    run_oss "$market" cp "$data_uri" "$data" --force --no-progress >/dev/null; run_oss "$market" cp "$success_uri" "$success" --force --no-progress >/dev/null
    expected_success="$readback/$file.expected-success"; printf '%s\n' "$digest" >"$expected_success"; [[ $(sha256_file "$data") == "$digest" ]] || die "$market OSS data digest mismatch"; cmp -s "$success" "$expected_success" || die "$market OSS success marker mismatch"
    success_digest=$(sha256_file "$success")
    observed_at=$(monday_epoch_ns_rfc3339 "$observed_cutoff_ns")
    triplet_json=$(jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" \
      --arg data_uri "$data_uri" --arg manifest_uri "$uri" \
      --arg success_uri "$success_uri" --arg data_sha "$digest" \
      --arg manifest_sha "$manifest_digest" --arg success_sha "$success_digest" \
      --arg prefix "$object_prefix" --arg observed "$observed_at" \
      --arg session "${phase_session[$market]}" --arg catalog "${frozen_catalog_sha256[$market]}" \
      --argjson start "$start" --argjson end "$end" \
      --argjson observed_ns "$observed_cutoff_ns" \
      '{market:$market,dataset:$dataset,data_uri:$data_uri,manifest_uri:$manifest_uri,success_uri:$success_uri,
        data_sha256:$data_sha,manifest_sha256:$manifest_sha,success_sha256:$success_sha,
        success_content:($data_sha + "\n"),object_prefix:$prefix,observed_at:$observed,
        observed_at_ns:$observed_ns,
        start_received_at_ns:$start,end_received_at_ns:$end,session_id:$session,
        catalog_sha256:$catalog}')
    phase_triplets_json[$market]=$(jq -cn --argjson values "${phase_triplets_json[$market]}" \
      --argjson value "$triplet_json" '$values + [$value]')
    roundtrip_records+=("$data" "$digest" "$manifest_digest")
    count=$((count + 1))
  done <"$readback/manifest-uris"
  ((count >= 2)) || die "$market OSS readback has fewer than two triplets"
  monday_validate_replay_safe_manifest_order "$market" "$candidates_file" "$unsafe_file" \
    || die "$market replay-safe manifest ordering failed"
  verify_adjacent_segments "${roundtrip_records[@]}" || die "$market OSS strict LOB continuity verifier failed"
  if [[ $market == spot ]]; then
    verify_aggregate_trade_continuity "${roundtrip_records[@]}" || die "$market OSS strict aggregate-trade continuity verifier failed"
    verify_raw_trade_continuity "${roundtrip_records[@]}" || die "$market OSS strict raw-trade continuity verifier failed"
  fi
  market_observed_at_ns[$market]=$observed_cutoff_ns
  phase_oss[$market]=$count
  resource_monitor_stop
}

for market in "${markets[@]}"; do render_shadow_unit "$market"; done
[[ $TEST_ONLY == true ]] || systemctl daemon-reload
for market in "${markets[@]}"; do run_market_gate_phase "$market"; run_candidate_drain "$market"; assert_spool_drained "$market"; done
for market in "${markets[@]}"; do verify_oss_roundtrips "$market"; done
for market in "${markets[@]}"; do systemctl stop "${unit[$market]}" >/dev/null 2>&1 || true; done
for asset in "${SHADOW_ASSETS[@]}"; do
  target=${installed_asset[$asset]}
  if [[ ${saved_state[$asset]} == projection ]]; then
    [[ -L $target && $(readlink -- "$target") == "${saved_target[$asset]}" ]] \
      || die "shadow asset changed during Gate: $asset"
    resolved=$(readlink -f -- "$target") || die "shadow projection disappeared: $asset"
    [[ $(sha256_file "$resolved") == "${saved_sha[$asset]}" ]] || die "shadow asset bytes changed: $asset"
  elif [[ ${saved_state[$asset]} == present ]]; then
    [[ -f $target && ! -L $target && $(sha256_file "$target") == "${saved_sha[$asset]}" ]] \
      || die "shadow asset changed during Gate: $asset"
  else
    [[ ! -e $target && ! -L $target ]] || die "shadow asset appeared during Gate: $asset"
  fi
done
if [[ $FROM_CONTROLLER != direct ]]; then
  [[ $(monday_active_controller_sha "$ROOT") == "$FROM_CONTROLLER" ]] || die 'active controller changed during Gate'
  monday_verify_controller_projections "$ROOT" "$FROM_CONTROLLER" || die 'before controller projections changed during Gate'
  [[ -L $PRODUCTION_BINARY && $(readlink -- "$PRODUCTION_BINARY") == "$CONTROLLER_ROOT/active/binance-lob-archiver" ]] || die 'production identity changed during Gate'
else
  [[ $(monday_active_controller_sha "$ROOT") == "$legacy_controller" ]] || die 'legacy active controller changed during Gate'
  monday_verify_legacy_controller_release "$ROOT" "$legacy_controller" "$PRODUCTION_BINARY" \
    || die 'legacy controller identity changed during Gate'
  [[ $TEST_ONLY == true || $(readlink -f -- "$PRODUCTION_BINARY") == "$candidate_binary" ]] || die 'direct production identity changed during Gate'
fi
if [[ $old_shadow_present == true ]]; then [[ $(readlink -- "$SHADOW_BINARY") == "$old_shadow_target" ]] || die 'shadow link was not restored'; else [[ ! -e $SHADOW_BINARY && ! -L $SHADOW_BINARY ]] || die 'shadow link was not removed'; fi
final_payload=$(monday_manifest_field "$candidate_manifest" artifact_sha256) || die 'candidate payload identity disappeared during Gate'
final_runtime=$(monday_manifest_field "$candidate_manifest" runtime_contract_sha256) || die 'candidate runtime identity disappeared during Gate'
[[ $final_payload == "$candidate_payload" && $final_runtime == "$candidate_runtime" ]] \
  || die 'candidate C/P/R changed during Gate'
monday_verify_controller_release "$ROOT" "$CANDIDATE_CONTROLLER" \
  || die 'candidate controller failed final identity verification'
[[ $(sha256_file "$candidate_binary") == "$candidate_payload" ]] \
  || die 'candidate payload changed during Gate'
[[ $(sha256_file "$candidate_release/deployment.sha256") == "$candidate_control_bytes_sha" ]] \
  || die 'candidate control bytes changed during Gate'
if [[ $old_shadow_present == true ]]; then
  restored_target=$(readlink -f -- "$SHADOW_BINARY") || die 'restored shadow link is unresolved'
  [[ $(sha256_file "$restored_target") == "$old_shadow_target_sha256" ]] \
    || die 'restored shadow binary bytes changed during Gate'
fi
refresh_production_snapshot || die 'production cgroup identity changed during Gate'
# A direct bootstrap lease is only a Gate-time guard.  Restore the original
# runtime limit before publishing the receipt; Cutover will install the
# permanent signed slice as part of the atomic 8 -> 9 migration.
restore_bootstrap_slice_lease || die 'temporary production slice lease could not be restored'
production_memory_json=$(jq -c \
  --arg mode "$bootstrap_slice_lease_mode" \
  --arg before_high "${bootstrap_slice_lease_before_high:-}" \
  --arg before_max "${bootstrap_slice_lease_before_max:-}" \
  --argjson applied "${bootstrap_slice_lease_applied:-false}" \
  --argjson restored "${bootstrap_slice_lease_restored:-false}" \
  '.slice_lease={mode:$mode,before_memory_high:$before_high,before_memory_max:$before_max,
    requested_memory_high:"3072M",requested_memory_max:"3584M",applied:$applied,restored:$restored}' \
  <<<"$production_memory_json")

checks=$(jq -cn '{before_pair_unchanged:true,production_runtime_verified:true,shadow_staging_verified:true,shadow_assets_restored:true,resource_preflight:true,oss_triplets:true,strict_segment_verifier:true,final_identity:true,controller_control_bytes:true,shadow_link_restored:true,health_freshness:true}')
before_assets_json='{}'; staged_assets_json='{}'; restored_assets_json='{}'
for asset in "${SHADOW_ASSETS[@]}"; do
  restored_asset_sha[$asset]="${saved_sha[$asset]:-}"
  before_assets_json=$(jq -cn --argjson values "$before_assets_json" --arg asset "$asset" \
    --arg state "${saved_state[$asset]}" --arg sha "${saved_sha[$asset]:-}" \
    --arg target "${saved_target[$asset]:-}" \
    '$values + {($asset):{state:$state,sha256:(if $sha == "" then null else $sha end),target:(if $state == "projection" then $target else null end)}}')
  staged_assets_json=$(jq -cn --argjson values "$staged_assets_json" --arg asset "$asset" \
    --arg sha "${candidate_asset_sha[$asset]:-}" \
    '$values + {($asset):$sha}')
  restored_assets_json=$(jq -cn --argjson values "$restored_assets_json" --arg asset "$asset" \
    --arg state "${saved_state[$asset]}" --arg sha "${restored_asset_sha[$asset]:-}" \
    --arg target "${saved_target[$asset]:-}" \
    '$values + {($asset):{state:$state,sha256:(if $sha == "" then null else $sha end),target:(if $state == "projection" then $target else null end)}}')
done
markets_json='{}'
for market in "${markets[@]}"; do
  health_sha=$(sha256_file "${spool_dir[$market]}/health.json")
  receipt_bucket=${oss_bucket[$market]}
  [[ $TEST_ONLY == true ]] && receipt_bucket=fixture
  phase_health_json[$market]=$(jq -cn --argjson value "${phase_health_json[$market]}" \
    --argjson silence "${max_health_silence_seconds[$market]}" \
    --argjson samples "${health_samples[$market]}" \
    '$value | .max_health_silence_seconds=$silence | .samples=$samples')
  market_json=$(jq -cn --arg market "$market" --arg unit "${unit[$market]}" \
    --arg dataset "${dataset[$market]}" --arg session "${phase_session[$market]}" \
    --arg bucket "$receipt_bucket" --arg configured_bucket "${oss_bucket[$market]}" \
    --arg endpoint "${oss_endpoint[$market]}" --arg region "${oss_region[$market]}" \
    --arg profile "${aliyun_profile[$market]}" --arg shard all \
    --arg spool "${spool_dir[$market]}" --arg prefix "${expected_oss_prefix[$market]}" \
    --argjson pid "${phase_pid[$market]}" --arg exe "${phase_exe_sha[$market]}" \
    --argjson runtime "${phase_runtime[$market]}" --argjson segments "${phase_segments[$market]}" \
    --argjson oss "${phase_oss[$market]}" --arg health "$health_sha" \
    --argjson segment_evidence "${phase_segments_json[$market]}" \
    --argjson triplet_evidence "${phase_triplets_json[$market]}" \
    --argjson health_evidence "${phase_health_json[$market]}" \
    --argjson strict_lob "${phase_strict_lob[$market]:-false}" \
    --argjson strict_aggregate "${phase_strict_aggregate[$market]:-false}" \
    --argjson strict_raw "${phase_strict_raw[$market]:-false}" \
    --argjson observed_at_ns "${market_observed_at_ns[$market]}" \
    '{market:$market,unit:$unit,dataset:$dataset,session_id:$session,main_pid:$pid,
      process_exe_sha256:$exe,n_restarts:0,observed_runtime_seconds:$runtime,
      spool_dir:$spool,shard_id:$shard,oss_bucket:$configured_bucket,oss_endpoint:$endpoint,
      oss_region:$region,aliyun_profile:$profile,segment_count:$segments,oss_triplet_count:$oss,health_sha256:$health,
      expected_oss_bucket:$bucket,expected_oss_prefix:$prefix,observed_at_ns:$observed_at_ns,segments:$segment_evidence,
      triplets:$triplet_evidence,health:$health_evidence,
      process_identity_verified:true,installed_shadow_assets_verified:true,
      strict_lob_continuity_readback:$strict_lob,
      strict_aggregate_trade_continuity_readback:$strict_aggregate,
      strict_raw_trade_continuity_readback:$strict_raw}')
  markets_json=$(jq -cn --argjson values "$markets_json" --arg market "$market" --argjson value "$market_json" '$values + {($market):$value}')
done
run_units_json=$(jq -cn --arg spot "${unit[spot]}" --arg usdm "${unit[usdm]}" \
  '{spot:$spot,usdm:$usdm}')
run_upload_units_json=$(jq -cn \
  --arg spot "${upload_unit[spot]}" --arg usdm "${upload_unit[usdm]}" \
  --arg spot_sha "${candidate_upload_unit_sha[spot]}" \
  --arg usdm_sha "${candidate_upload_unit_sha[usdm]}" \
  '{spot:{unit:$spot,sha256:$spot_sha},usdm:{unit:$usdm,sha256:$usdm_sha}}')
gate_finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ); production_eligible=true; [[ $TEST_ONLY == true ]] && production_eligible=false
jq -cn --arg schema monday.rust_lob_shadow_gate.v6 --arg from "$before_controller" --arg source_mode "$source_mode" --arg after "$CANDIDATE_CONTROLLER" --arg candidate "$CANDIDATE_CONTROLLER" --arg payload "$candidate_payload" --arg runtime "$candidate_runtime" --arg bundle "$candidate_bundle" --arg source "$candidate_source" --arg before_payload "$before_payload" --arg before_runtime "$before_runtime" --arg before_bundle "$before_bundle" --arg before_source "$before_source" --arg before_projection "$before_production_projection" --arg control_sha "$candidate_control_bytes_sha" --argjson control_assets "$candidate_control_assets" --argjson production_runtime "$candidate_production_runtime" --arg run "$run_id" --arg spool "$run_spool" --arg run_unit_root "$gate_unit_dir" --arg worker_slice "$GATE_WORKER_SLICE" --arg worker_slice_sha "$gate_worker_slice_sha256" --arg worker_slice_cgroup "$gate_worker_slice_control_group" --argjson worker_high "$GATE_WORKER_MEMORY_HIGH_BYTES" --argjson worker_max "$GATE_WORKER_MEMORY_MAX_BYTES" --argjson units "$run_units_json" --argjson upload_units "$run_upload_units_json" --arg started "$gate_started_at" --arg finished "$gate_finished_at" --argjson host_total "$host_memory_total" --argjson host_swap "$host_swap_total" --argjson production_memory "$production_memory_json" --argjson production_process "$production_process_json" --argjson production_assets "$production_asset_json" --argjson resources "$resource_samples" --argjson psi "$psi_windows" --argjson checks "$checks" --argjson markets "$markets_json" --argjson eligible "$production_eligible" --argjson test_only "$TEST_ONLY" --argjson before_assets "$before_assets_json" --argjson staged_assets "$staged_assets_json" --argjson restored_assets "$restored_assets_json" --arg shadow_binary "$SHADOW_BINARY" --arg candidate_binary "$candidate_binary" --arg old_shadow_target "$old_shadow_target" --arg old_shadow_target_sha "$old_shadow_target_sha256" --argjson old_shadow_present "$old_shadow_present" \
  '{schema:$schema,control_plane_version:2,passed:true,production_eligible:$eligible,test_only:$test_only,source_mode:$source_mode,from_controller_sha256:$from,transition:{before:$from,after:$after,topology:(if $source_mode == "direct" then "direct-bootstrap" else "stable" end)},candidate_controller_sha256:$candidate,candidate_payload_sha256:$payload,candidate_runtime_contract_sha256:$runtime,candidate_deployment_bundle_sha256:$bundle,candidate_deployment_source_revision:$source,candidate_control_bytes:{sha256:$control_sha,assets:$control_assets},production_runtime:$production_runtime,before:{controller:$from,payload_sha256:$before_payload,runtime_contract_sha256:$before_runtime,deployment_bundle_sha256:$before_bundle,deployment_source_revision:$before_source,production_projection:$before_projection,production_assets:$production_assets},run_id:$run,run_spool:$spool,started_at:$started,finished_at:$finished,required_duration_seconds:240,health_settle_seconds:240,segment_seconds:120,host_memory_total_bytes:$host_total,host_swap_total_bytes:$host_swap,production_memory:$production_memory,production_process:$production_process,production_assets:$production_assets,resource_admission:$resources,io_full_psi_windows:$psi,shadow_staging:{mode:"run-scoped",run_unit_root:$run_unit_root,spool_root:$spool,aggregate_slice:{name:$worker_slice,sha256:$worker_slice_sha,cgroup:$worker_slice_cgroup,memory_high_bytes:$worker_high,memory_max_bytes:$worker_max},units:$units,upload_units:$upload_units,candidate_assets:$staged_assets,restored_assets:$restored_assets,before_assets:$before_assets,binary:{path:$run_unit_root,candidate_target:$candidate_binary,restored_target:(if $old_shadow_present then $old_shadow_target else null end),restored_target_sha256:(if $old_shadow_present then $old_shadow_target_sha else null end),restored_present:$old_shadow_present}},checks:$checks,markets:$markets}' >"$gate_json.tmp"
chmod 0640 "$gate_json.tmp"; [[ ! -e $gate_json ]] || die 'gate receipt already exists'; mv -f -- "$gate_json.tmp" "$gate_json"
if ! jq -e -f "$POLICY_SOURCE" "$gate_json" >/dev/null; then
  die 'V2 Gate policy rejected the receipt'
fi
if [[ $production_eligible == true ]]; then gate_sha=$(sha256_file "$gate_json"); printf '%s  gate.json\n' "$gate_sha" >"$passed_marker.tmp"; chmod 0640 "$passed_marker.tmp"; mv -f -- "$passed_marker.tmp" "$passed_marker"; fi
gate_finished=true; printf 'V2 Gate receipt: %s\nSHA-256: %s\n' "$gate_json" "$(sha256_file "$gate_json")"; [[ $production_eligible == true ]] && printf 'production shadow gate passed\n' || printf 'fixture Gate completed; not eligible for cutover\n'
