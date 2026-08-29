#!/bin/sh
#
# monday-collector-health.sh - durable read-only health monitor for the Monday
# public-data collector host (monday-trade-data-26, Aliyun Tokyo ap-northeast-1).
#
# Guards against a silent recurrence of the 2026-08-05/06 disk-full incident in
# which every governed collector stopped and uploads failed while the only
# on-host monitor (polymarket-market-tape-upload-watchdog.sh) self-healed
# without ever alerting a human. Reconnect evidence comes from the collector's
# typed health counters; this monitor does not scan the journal.
#
# Health contract: hard gates, each a breach that fails closed into the
# monitor-collector-host workflow issue, plus the raw-ops Gate containment
# contract:
#   1. upload-status.json exists and parses for the mandated lanes
#      (binance-lob spot/usdm and binance-fee); missing or malformed = breach.
#   2. last_success_at is present and fresh on every upload lane. Thresholds
#      sit just above each lane's upload cadence (see the *_SUCCESS_MAX_AGE
#      constants). A missing/unparseable last_success_at is a breach: the
#      delivery loop is unproven. The fee uploader gains last_success_at in a
#      parallel change; until that deploys, the fee lane breaches by design.
#      The polymarket lanes additionally breach once the oldest pending
#      rotated tape exceeds POLY_PENDING_STALE_MAX_AGE: an upload stall with
#      a live backlog must alert within 30 minutes, not after two full
#      rotations. (Keyed on pending age, not last_success_at: with hourly
#      rotation the last success is legitimately ~60 minutes old whenever a
#      fresh tape awaits the next timer run.)
#   3. Pending upload backlog is bounded: the pending count stays under the
#      lane limit AND the oldest pending artifact stays younger than the lane
#      age bound. Pending artifacts are discovered with the same rules the
#      collectors themselves use: LOB lanes count top-level *.manifest.json
#      (the archiver's own pending_upload_segments definition), the fee and
#      usdm-reference lanes count lake/raw/**/batch=* directories (removed
#      after each verified upload), the polymarket lanes count rotated
#      market-updates.<stamp>[.frac][.uuid].ndjson tapes, and the bybit lane
#      counts .ndjson segments carrying manifest+_SUCCESS markers but no
#      .uploaded.json readback marker.
#   4. failure_count must not grow between polls and last_error must be empty,
#      uniformly across all upload lanes.
#   5. /data disk watermarks: free at or below DISK_WARN_PERCENT is a warning;
#      free at or below DISK_CRIT_PERCENT (used >= 85%) is a breach. The
#      2026-08-17/18
#      incidents reached 100% twice, so the critical watermark pages a human
#      instead of only warning.
#   6. polymarket-market-tape-upload.timer and polymarket-reference-upload.timer
#      must be active (waiting) whenever their collector service is active; a
#      stopped timer with a running collector silently strands rotated tapes
#      until the disk fills.
#   7. /data must be mounted; otherwise healthy-looking spool paths may be
#      writing to the root filesystem instead of the governed data volume.
# The raw-ops Gate template has no [Install] section, so systemd reports it as
# static. Static is healthy only when no Gate instance, running lock, or
# residual EnvironmentFile remains on the host. State-persistence failures
# also stay breaches because gate 4 delta detection depends on the persisted
# state.
#
# Everything else is a WARNING: unit/timer active+enabled state, systemd
# Result, restart-rate deltas, and health.json freshness/sequence counters.
# Warnings are reported in the JSON warnings array (and as warning: lines in
# text mode) but never block ok:true.
#
# The script is READ-ONLY: it never starts, stops, enables, or disables a unit
# and never modifies tape files or upload-status.json. It emits one JSON
# snapshot (or a human ok:/breach: summary) and exits nonzero when any breach
# is present. It runs from the monday-collector-health.timer every five minutes
# and is also invoked on demand by the monitor-collector-host GitHub Actions
# workflow through Aliyun Cloud Assistant.
#
# Usage: monday-collector-health.sh [--json] [--dry-run]
#   --json     emit a single JSON object to stdout (nothing else on stdout)
#   --dry-run  do not read or write the persistent upload-failure/restart state
#
# Test/override environment:
#   MONDAY_COLLECTOR_SPOOL_ROOT  spool root (default /data/monday/spool)
#   MONDAY_COLLECTOR_STATE_DIR   state directory (default
#                                /var/lib/monday-collector-health)
#   MONDAY_COLLECTOR_HEALTH_TEST_MODE=1 and
#   MONDAY_COLLECTOR_HEALTH_TEST_ROOT  are reserved for the contract test
#                                      fixture under /tmp. Test mode may also
#   MONDAY_COLLECTOR_HEALTH_TEST_HFT_GID override the expected collector gid.
set -u

TAG=monday-collector-health
SPOOL_ROOT=${MONDAY_COLLECTOR_SPOOL_ROOT:-/data/monday/spool}
STATE_DIR=${MONDAY_COLLECTOR_STATE_DIR:-/var/lib/monday-collector-health}
STATE_FILE="$STATE_DIR/state.json"
RECOVERY_QUEUE_ROOT="$SPOOL_ROOT/binance-lob-recovery"

HEALTH_SILENCE_SECONDS=300
DISK_WARN_PERCENT=25
DISK_CRIT_PERCENT=15
RESTART_MAX_DELTA=1
# Gate 2: last_success_at freshness per lane, set just above the lane's upload
# cadence:
# - LOB segments rotate every SEGMENT_SECONDS (default 3600s) and the
#   in-process upload loop runs every 300s, so a healthy lane uploads at least
#   once per rotation; allow two full rotations.
# - fee snapshots publish every 60s and binance-fee-upload.timer retries every
#   60s; fee delivery is hard-gated by upload-status.json and the oneshot
#   Result observed below.
# - usdm-reference runs on a 5-minute upload timer over hourly reference
#   batches.
# - both polymarket lanes rotate tapes hourly
#   (record_market_updates_rotate_seconds = 3600; the reference writer rotates
#   at UTC-hour boundaries) and last_success_at only advances when a rotated
#   tape actually uploads, so the 5-minute upload timer is not a heartbeat;
#   allow two full rotations, same as LOB.
# - bybit options segments finalize on the hour and the upload timer sweeps
#   them at :23, so 90 minutes covers one full finalize+sweep cycle.
LOB_SUCCESS_MAX_AGE=7200
FEE_SUCCESS_MAX_AGE=600
REF_SUCCESS_MAX_AGE=1200
POLY_SUCCESS_MAX_AGE=7200
BYBIT_SUCCESS_MAX_AGE=5400
# Gate 2 polymarket addendum: with rotated tapes still pending, an upload
# stall must alert within 30 minutes rather than after two full rotations.
POLY_PENDING_STALE_MAX_AGE=1800
RECOVERY_QUEUE_READY_MAX_AGE=1800
RECOVERY_QUEUE_RUNNING_MAX_AGE=7200

# Gate 3: pending backlog bounds per lane (count limit, oldest-artifact age).
LOB_PENDING_MAX=4
LOB_PENDING_MAX_AGE=10800
FEE_PENDING_MAX=120
FEE_PENDING_MAX_AGE=7200
REF_PENDING_MAX=24
REF_PENDING_MAX_AGE=10800
POLY_PENDING_MAX=100
POLY_PENDING_MAX_AGE=86400
BYBIT_PENDING_MAX=48
BYBIT_PENDING_MAX_AGE=7200

# Governed units. Persistent services are observed for active + enabled +
# Result=success and restart-rate deltas (all warnings). Upload lanes are
# driven by timers whose oneshot services' last Result is observed (warning);
# their delivery is hard-gated through upload-status.json instead.
ARCHIVER_SPOT=binance-lob-archiver-production@spot.service
ARCHIVER_USDM=binance-lob-archiver-production@usdm.service
RECOVERY_SPOT_TIMER=binance-lob-archiver-recovery@spot.timer
RECOVERY_USDM_TIMER=binance-lob-archiver-recovery@usdm.timer
REFERENCE_COLLECTOR=binance-usdm-reference-collector.service
POLY_MARKET_UPLOAD_TIMER=polymarket-market-tape-upload.timer
POLY_MARKET_UPLOAD_SERVICE=polymarket-market-tape-upload.service
POLY_REF_UPLOAD_TIMER=polymarket-reference-upload.timer
POLY_REF_UPLOAD_SERVICE=polymarket-reference-upload.service
# Collectors that produce the tapes the two polymarket upload timers drain.
POLY_MARKET_COLLECTOR=polymarket-market-tape.service
POLY_REF_COLLECTOR=polymarket-reference-collector.service
WATCHDOG_TIMER=polymarket-market-tape-upload-watchdog.timer
WATCHDOG_SERVICE=polymarket-market-tape-upload-watchdog.service
# Governed production lanes since the 2026-08-08 cutovers.
BYBIT_ARCHIVER=bybit-options-archiver.service
BYBIT_UPLOAD_TIMER=bybit-options-upload.timer
BYBIT_UPLOAD_SERVICE=bybit-options-upload.service
USDM_REF_UPLOAD_TIMER=binance-usdm-reference-upload.timer
USDM_REF_UPLOAD_SERVICE=binance-usdm-reference-upload.service
FEE_SPOT_TIMER=binance-fee-snapshot-spot.timer
FEE_SPOT_SERVICE=binance-fee-snapshot-spot.service
FEE_USDM_TIMER=binance-fee-snapshot-usdm.timer
FEE_USDM_SERVICE=binance-fee-snapshot-usdm.service
FEE_UPLOAD_TIMER=binance-fee-upload.timer
FEE_UPLOAD_SERVICE=binance-fee-upload.service
POLY_RAW_OPS_GATE='polymarket-raw-ops-gate@.service'
POLY_RAW_OPS_GATE_RUN_ROOT=/run/monday/polymarket-raw-ops-gates
POLY_RAW_OPS_GATE_CONTROL_LOCK="$POLY_RAW_OPS_GATE_RUN_ROOT/control.lock"
if [ "${MONDAY_COLLECTOR_HEALTH_TEST_MODE:-0}" = 1 ]; then
  test_root=${MONDAY_COLLECTOR_HEALTH_TEST_ROOT:-}
  case "$test_root" in
    *..*)
      printf 'invalid collector-health test root\n' >&2
      exit 2
      ;;
    /tmp/monday-collector-health-test.*) ;;
    *)
      printf 'invalid collector-health test root\n' >&2
      exit 2
      ;;
  esac
  POLY_RAW_OPS_GATE_RUN_ROOT="$test_root/run/monday/polymarket-raw-ops-gates"
  POLY_RAW_OPS_GATE_CONTROL_LOCK="$POLY_RAW_OPS_GATE_RUN_ROOT/control.lock"
elif [ -n "${MONDAY_COLLECTOR_HEALTH_TEST_ROOT:-}" ]; then
  printf 'collector-health test root requires explicit test mode\n' >&2
  exit 2
fi

JSON_MODE=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --json) JSON_MODE=1 ;;
    --dry-run) DRY_RUN=1 ;;
    *)
      printf 'usage: %s [--json] [--dry-run]\n' "$0" >&2
      exit 2
      ;;
  esac
done

if ! command -v jq >/dev/null 2>&1; then
  logger -t "$TAG" -p daemon.err -- 'jq is required but not installed' 2>/dev/null || true
  printf 'ok:false\nbreach: jq is required but not installed\n' >&2
  exit 1
fi

breach_count=0
breaches=""
warnings=""
units_json='{}'
health_json='{}'
uploads_json='{}'
recovery_queue_json='{}'
recovery_queue_root_ok=1
recovery_root_owner_uid=0
recovery_hft_owner_uid=''
recovery_hft_group_gid=''
delay_gate_json='{}'
disk_json='{}'
mount_json='{}'
state_lines=""

log() {
  logger -t "$TAG" -p "daemon.$1" -- "$2" 2>/dev/null || true
}

bool_json() {
  [ "$1" = 1 ] && printf 'true' || printf 'false'
}

record_breach() {
  msg=$1
  breach_count=$((breach_count + 1))
  if [ -n "$breaches" ]; then
    breaches="$breaches
$msg"
  else
    breaches=$msg
  fi
  log err "$msg"
}

record_warning() {
  msg=$1
  if [ -n "$warnings" ]; then
    warnings="$warnings
$msg"
  else
    warnings=$msg
  fi
  log warning "$msg"
}

read_prior() {
  # $1 = state key (e.g. "nrestarts|<unit>"); prints prior value or empty
  if [ -f "$STATE_FILE" ]; then
    grep "^$1=" "$STATE_FILE" 2>/dev/null | head -n1 | cut -d= -f2-
  fi
}

preserve_sequence_prior() {
  # Invalid or missing health observations must never erase the last valid
  # session/counter baseline when write_state atomically replaces the file.
  if [ "$DRY_RUN" -eq 0 ] && [ -n "${prior_session:-}" ] \
    && [ -n "${prior_total:-}" ]; then
    sequence_gap_previous_total_json=$prior_total
    state_lines="$state_lines sequence_gap_session|$label=$prior_session sequence_gap_total|$label=$prior_total"
  fi
}

file_mtime() {
  # Portable mtime: GNU stat on the host, BSD stat under the macOS test stubs.
  stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null
}

file_uid() {
  # Portable owner uid: GNU stat on the host, BSD stat under the macOS tests.
  stat -c %u "$1" 2>/dev/null || stat -f %u "$1" 2>/dev/null
}

file_gid() {
  # Portable owner gid: GNU stat on the host, BSD stat under the macOS tests.
  stat -c %g "$1" 2>/dev/null || stat -f %g "$1" 2>/dev/null
}

file_group_world_not_writable() {
  mode=$(stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1" 2>/dev/null) || return 1
  case "$mode" in
    [0-7][0-7][0-7] | [0-7][0-7][0-7][0-7]) ;;
    *) return 1 ;;
  esac
  case "$mode" in
    *[2367]? | *[2367]) return 1 ;;
    *) return 0 ;;
  esac
}

owned_directory_not_writable() {
  [ -d "$1" ] && [ ! -L "$1" ] \
    && [ "$(file_uid "$1")" = "$2" ] \
    && file_group_world_not_writable "$1"
}

owned_regular_file_not_writable() {
  [ -f "$1" ] && [ ! -L "$1" ] \
    && [ "$(file_uid "$1")" = "$2" ] \
    && file_group_world_not_writable "$1"
}

owned_collector_traversable_directory() {
  [ -d "$1" ] && [ ! -L "$1" ] \
    && [ "$(file_uid "$1")" = "$2" ] \
    && [ "$(file_gid "$1")" = "$3" ] \
    && file_group_world_not_writable "$1" \
    && mode=$(stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1" 2>/dev/null) \
    && case "$mode" in
      [0-7][1357][0-7] | [0-7][0-7][1357][0-7]) true ;;
      *) false ;;
    esac
}

unit_is_active() {
  systemctl is-active "$1" 2>/dev/null || true
}
unit_is_enabled() {
  systemctl is-enabled "$1" 2>/dev/null || true
}
unit_result() {
  systemctl show -p Result --value "$1" 2>/dev/null || true
}
unit_nrestarts() {
  systemctl show -p NRestarts --value "$1" 2>/dev/null || true
}
unit_substate() {
  systemctl show -p SubState --value "$1" 2>/dev/null || true
}
unit_timer_next() {
  systemctl show -p NextElapseUSecMonotonic --value "$1" 2>/dev/null || true
}

check_mount() {
  data_mounted=0
  if command -v mountpoint >/dev/null 2>&1; then
    if mountpoint -q /data 2>/dev/null; then
      data_mounted=1
    fi
  elif grep -q '[[:space:]]/data[[:space:]]' /proc/mounts 2>/dev/null; then
    data_mounted=1
  fi
  if [ "$data_mounted" -eq 0 ]; then
    record_breach "mount: /data is not mounted"
  fi
  mount_json=$(jq -n --argjson m "$(bool_json "$data_mounted")" '{data_mounted: $m}')
}

check_disk() {
  disk_total_kib=0
  disk_avail_kib=0
  df_out=$(df -Pk /data 2>/dev/null || true)
  disk_total_kib=$(printf '%s\n' "$df_out" | awk 'NR==2 {print $2; exit}')
  disk_avail_kib=$(printf '%s\n' "$df_out" | awk 'NR==2 {print $4; exit}')
  case "$disk_total_kib" in (*[!0-9]*|'') disk_total_kib=0;; esac
  case "$disk_avail_kib" in (*[!0-9]*|'') disk_avail_kib=0;; esac
  if [ "$disk_total_kib" -eq 0 ]; then
    disk_free_percent=0
    disk_free_gb=0
    disk_critical=1
    disk_warning=1
    record_warning "disk: cannot determine /data free space (df unavailable)"
  else
    disk_free_percent=$((disk_avail_kib * 100 / disk_total_kib))
    disk_free_gb=$((disk_avail_kib / 1048576))
    disk_critical=0
    disk_warning=0
    if [ "$disk_free_percent" -le "$DISK_CRIT_PERCENT" ]; then
      disk_critical=1
      record_breach "disk: /data free ${disk_free_percent}% (${disk_free_gb}GiB) at or below critical ${DISK_CRIT_PERCENT}%"
    elif [ "$disk_free_percent" -le "$DISK_WARN_PERCENT" ]; then
      disk_warning=1
      record_warning "disk: /data free ${disk_free_percent}% (${disk_free_gb}GiB) at or below warning ${DISK_WARN_PERCENT}%"
    fi
  fi
  disk_json=$(jq -n --argjson p "$disk_free_percent" --argjson g "$disk_free_gb" \
    --argjson w "$(bool_json "$disk_warning")" --argjson c "$(bool_json "$disk_critical")" \
    '{free_percent: $p, free_gb: $g, warning: $w, critical: $c}')
}

check_service() {
  # Persistent service: active AND enabled AND Result=success AND restart-rate
  # delta. All soft signals: the hard delivery gates run on upload-status.json.
  unit=$1
  label=$2
  active=$(unit_is_active "$unit")
  enabled=$(unit_is_enabled "$unit")
  result=$(unit_result "$unit")
  nrestarts=$(unit_nrestarts "$unit")
  case "$nrestarts" in (*[!0-9]*|'') nrestarts=0;; esac
  [ "$active" = "active" ] || record_warning "$label: not active (is-active='$active')"
  [ "$enabled" = "enabled" ] || record_warning "$label: not enabled (is-enabled='$enabled')"
  [ "$result" = "success" ] || record_warning "$label: last systemd Result='$result'"
  prior=$(read_prior "nrestarts|$unit")
  if [ "$DRY_RUN" -eq 0 ] && [ -n "$prior" ]; then
    case "$prior" in (*[!0-9]*|'') prior="" ;; esac
    if [ -n "$prior" ]; then
      delta=$((nrestarts - prior))
      if [ "$delta" -gt "$RESTART_MAX_DELTA" ]; then
        record_warning "$label: restart rate high (NRestarts $prior -> $nrestarts)"
      fi
    fi
  fi
  state_lines="$state_lines nrestarts|$unit=$nrestarts"
  obj=$(jq -n --arg a "$active" --arg e "$enabled" --arg r "$result" --argjson n "$nrestarts" \
    '{active: ($a == "active"), enabled: ($e == "enabled"), result: $r, nrestarts: $n}')
  units_json=$(jq -n --argjson base "$units_json" --arg k "$unit" --argjson v "$obj" \
    '$base + {($k): $v}')
}

check_timer() {
  # Timer: active AND enabled (drives a oneshot upload/watchdog service).
  unit=$1
  label=$2
  backing_service=${3:-}
  active=$(unit_is_active "$unit")
  enabled=$(unit_is_enabled "$unit")
  if [ -n "$backing_service" ]; then
    service_active=$(unit_is_active "$backing_service")
    service_enabled=$(unit_is_enabled "$backing_service")
    if [ "$service_active" = "active" ] && [ "$service_enabled" = "enabled" ]; then
      [ "$active" = "active" ] \
        || record_breach "$label: timer not active (is-active='$active') while service $backing_service is active and enabled"
      [ "$enabled" = "enabled" ] \
        || record_breach "$label: timer not enabled (is-enabled='$enabled') while service $backing_service is active and enabled"
    fi
  else
    [ "$active" = "active" ] || record_warning "$label: timer not active (is-active='$active')"
    [ "$enabled" = "enabled" ] || record_warning "$label: timer not enabled (is-enabled='$enabled')"
  fi
  obj=$(jq -n --arg a "$active" --arg e "$enabled" \
    '{active: ($a == "active"), enabled: ($e == "enabled")}')
  units_json=$(jq -n --argjson base "$units_json" --arg k "$unit" --argjson v "$obj" \
    '$base + {($k): $v}')
}

check_scheduled_timer() {
  unit=$1
  label=$2
  backing_service=${3:-}
  check_timer "$unit" "$label"
  substate=$(unit_substate "$unit")
  next_elapse=$(unit_timer_next "$unit")
  case "$substate" in
    waiting)
      if [ -z "$next_elapse" ] || [ "$next_elapse" = "n/a" ] \
          || [ "$next_elapse" = "infinity" ]; then
        service_state=
        [ -z "$backing_service" ] || service_state=$(unit_is_active "$backing_service")
        case "$service_state" in
          active | activating | deactivating) ;;
          *) record_breach "$label: waiting timer has no finite next elapse" ;;
        esac
      fi
      ;;
    running) ;;
    *) record_breach "$label: timer not waiting or running (SubState='$substate')" ;;
  esac
  obj=$(jq -n --argjson base "$units_json" --arg k "$unit" \
    --arg s "$substate" --arg n "$next_elapse" \
    '$base | .[$k] += {substate: $s,
      scheduled: ($n != "" and $n != "n/a" and $n != "infinity"),
      next_elapse_monotonic: $n}')
  units_json=$obj
}

check_upload_timer_backed() {
  # Gate 6: an upload timer must be active (waiting) whenever its collector
  # service is active. A stopped timer with a running collector silently
  # strands rotated tapes until the spool disk fills. The standalone
  # check_timer observation above stays a warning; only the
  # collector-active pairing is a breach.
  collector=$1
  timer=$2
  label=$3
  collector_active=$(unit_is_active "$collector")
  timer_active=$(unit_is_active "$timer")
  if [ "$collector_active" = "active" ] && [ "$timer_active" != "active" ]; then
    record_breach "$label: $timer not active (is-active='$timer_active') while collector $collector is active"
  fi
}

check_oneshot_result() {
  # Oneshot upload/watchdog service: last Result observed as a warning.
  unit=$1
  label=$2
  result=$(unit_result "$unit")
  if [ -n "$result" ] && [ "$result" != "success" ]; then
    record_warning "$label: last systemd Result='$result'"
  fi
  obj=$(jq -n --arg r "$result" '{result: $r}')
  units_json=$(jq -n --argjson base "$units_json" --arg k "$unit" --argjson v "$obj" \
    '$base + {($k): $v}')
}

collect_raw_ops_gate_snapshot() {
  gate_list=$(systemctl list-units 'polymarket-raw-ops-gate@*.service' \
    --all --no-legend --no-pager 2>/dev/null)
  gate_list_rc=$?
  active_instances=$(printf '%s\n' "$gate_list" | awk '
    $1 ~ /^polymarket-raw-ops-gate@[^.]+[.]service$/ &&
    ($3 == "active" || $3 == "activating" || $3 == "deactivating" || $3 == "reloading") {
      print $1
    }
  ')
  if [ "$gate_list_rc" -ne 0 ]; then
    record_breach "$label: cannot inspect active Gate instances (systemctl list-units exit $gate_list_rc)"
  fi
  if [ -n "$active_instances" ]; then
    record_breach "$label: active Gate instance(s): $(printf '%s' "$active_instances" | tr '\n' ' ')"
  fi

  residual_env=""
  residual_env_check_failed=0
  if [ -e "$POLY_RAW_OPS_GATE_RUN_ROOT" ] || [ -L "$POLY_RAW_OPS_GATE_RUN_ROOT" ]; then
    if [ ! -d "$POLY_RAW_OPS_GATE_RUN_ROOT" ] || [ -L "$POLY_RAW_OPS_GATE_RUN_ROOT" ] \
      || [ ! -r "$POLY_RAW_OPS_GATE_RUN_ROOT" ] || [ ! -x "$POLY_RAW_OPS_GATE_RUN_ROOT" ]; then
      residual_env_check_failed=1
      record_breach "$label: Gate runtime root is not an inspectable directory ($POLY_RAW_OPS_GATE_RUN_ROOT)"
    fi
  fi
  if [ "$residual_env_check_failed" -eq 0 ] && [ -d "$POLY_RAW_OPS_GATE_RUN_ROOT" ]; then
    for env_file in "$POLY_RAW_OPS_GATE_RUN_ROOT"/*.env; do
      if [ -e "$env_file" ] || [ -L "$env_file" ]; then
        if [ -n "$residual_env" ]; then
          residual_env="$residual_env
$env_file"
        else
          residual_env=$env_file
        fi
      fi
    done
  fi
  if [ -n "$residual_env" ]; then
    record_breach "$label: residual Gate environment file(s): $(printf '%s' "$residual_env" | tr '\n' ' ')"
  fi
}

check_raw_ops_gate() {
  # The raw-ops gate template has no [Install] section, so static is the
  # expected installed state. Retain the old absence/masked states as clean,
  # but fail closed on an enabled/indirect template or an uninspectable state.
  unit=$1
  label=$2
  enabled=$(unit_is_enabled "$unit")
  case "$enabled" in
    static|disabled|masked|not-found) ;;
    *) record_breach "$label: unexpected is-enabled='$enabled'" ;;
  esac

  lock_held=0
  lock_check_failed=0
  lock_race_detected=0
  held_locks=""
  active_instances=""
  residual_env=""
  residual_env_check_failed=0
  lock_path=$POLY_RAW_OPS_GATE_CONTROL_LOCK
  lock_label='control lock'
  lock_path_present=0
  if [ -e "$lock_path" ] || [ -L "$lock_path" ]; then
    lock_path_present=1
  fi

  # A free control lock is held for the entire unit+EnvironmentFile snapshot.
  # The grouped redirection keeps an unreadable regular lock from terminating
  # this POSIX monitor before it can emit a fail-closed breach. A lock that is
  # absent at the first check is scanned optimistically and rechecked after the
  # snapshot; appearance during that window is also a breach.
  if [ "$lock_path_present" -eq 1 ] && { [ ! -f "$lock_path" ] || [ -L "$lock_path" ]; }; then
    lock_check_failed=1
    record_breach "$label: $lock_label path is not a regular file ($lock_path)"
    collect_raw_ops_gate_snapshot
  elif [ "$lock_path_present" -eq 1 ] && ! command -v flock >/dev/null 2>&1; then
    lock_check_failed=1
    record_breach "$label: cannot inspect $lock_label (flock unavailable)"
    collect_raw_ops_gate_snapshot
  elif [ "$lock_path_present" -eq 1 ]; then
    lock_group_ran=0
    snapshot_collected=0
    {
      lock_group_ran=1
      flock -n 9 2>/dev/null
      flock_rc=$?
      if [ "$flock_rc" -eq 0 ]; then
        collect_raw_ops_gate_snapshot
        snapshot_collected=1
      elif [ "$flock_rc" -eq 1 ]; then
        lock_held=1
        held_locks=$lock_path
        record_breach "$label: running $lock_label is held or unavailable ($lock_path)"
      else
        lock_check_failed=1
        record_breach "$label: cannot inspect $lock_label (flock exit $flock_rc)"
      fi
    } 9<"$lock_path" 2>/dev/null
    if [ "$lock_group_ran" -eq 0 ]; then
      lock_check_failed=1
      record_breach "$label: cannot inspect $lock_label ($lock_path)"
      collect_raw_ops_gate_snapshot
    elif [ "$snapshot_collected" -eq 0 ] && [ "$lock_held" -eq 0 ]; then
      collect_raw_ops_gate_snapshot
    fi
  else
    collect_raw_ops_gate_snapshot
    if [ -e "$lock_path" ] || [ -L "$lock_path" ]; then
      lock_race_detected=1
      record_breach "$label: $lock_path appeared during the containment snapshot"
    fi
  fi

  active_instances_json=$(printf '%s\n' "$active_instances" | jq -Rsc \
    'split("\n") | map(select(length > 0))')
  held_locks_json=$(printf '%s\n' "$held_locks" | jq -Rsc \
    'split("\n") | map(select(length > 0))')
  residual_env_json=$(printf '%s\n' "$residual_env" | jq -Rsc \
    'split("\n") | map(select(length > 0))')
  obj=$(jq -n --arg e "$enabled" --argjson i "$active_instances_json" \
    --argjson h "$held_locks_json" --argjson r "$residual_env_json" \
    --argjson l "$(bool_json "$lock_held")" --argjson u "$(bool_json "$lock_check_failed")" \
    --argjson f "$(bool_json "$residual_env_check_failed")" \
    --argjson x "$(bool_json "$lock_race_detected")" \
    '{is_enabled: $e, active_instances: $i, lock_held: $l, lock_check_failed: $u, lock_race_detected: $x, held_locks: $h, residual_env: $r, residual_env_check_failed: $f}')
  units_json=$(jq -n --argjson base "$units_json" --arg k "$unit" --argjson v "$obj" \
    '$base + {($k): $v}')
}

check_binance_health() {
  # health.json at /data/monday/spool/binance-lob/<market>/: freshness and the
  # collector's typed, session-scoped sequence-gap counter are soft signals
  # (warnings). The hard LOB gates run on upload-status.json and the pending
  # segment backlog. sequence_gap_total is cumulative only within session_id;
  # a new session or a counter regression establishes a new baseline instead
  # of fabricating a delta. Malformed counters retain the last valid baseline.
  label=$1
  spool_dir=$2
  health_file="$spool_dir/health.json"
  age=0
  gaps=0
  sequence_gap_total_json=null
  sequence_gap_delta_json=null
  sequence_gap_previous_total_json=null
  sequence_gap_session_json=null
  sequence_gap_observed=0
  sequence_gap_baseline=missing
  hwarn=false
  hstatus=unknown
  prior_session=''
  prior_total=''
  if [ "$DRY_RUN" -eq 0 ]; then
    prior_session=$(read_prior "sequence_gap_session|$label")
    prior_total=$(read_prior "sequence_gap_total|$label")
    case "$prior_total" in
      '' | *[!0-9]*) prior_total='' ;;
    esac
  fi
  if [ ! -f "$health_file" ] || [ -L "$health_file" ]; then
    record_warning "$label: health.json missing or a symbolic link ($health_file)"
    sequence_gap_baseline=missing
    preserve_sequence_prior
    age=999999
  elif ! updated_ns=$(jq -r '.updated_at_ns // 0' "$health_file" 2>/dev/null); then
    record_warning "$label: health.json unparseable ($health_file)"
    sequence_gap_baseline=malformed
    preserve_sequence_prior
    age=999999
  else
    gaps=$(jq -r '.sequence_gaps // 0' "$health_file" 2>/dev/null || printf '0')
    hwarn=$(jq -r '.disk_warning // false' "$health_file" 2>/dev/null || printf 'false')
    hstatus=$(jq -r '.status // "unknown"' "$health_file" 2>/dev/null || printf 'unknown')
    case "$gaps" in (*[!0-9]*|'') gaps=0 ;; esac
    if [ "$gaps" -gt 0 ]; then
      record_warning "$label: sequence_gaps=$gaps"
    fi
    updated_ns_valid=1
    case "$updated_ns" in
      (*[!0-9]*|'') updated_ns=0; updated_ns_valid=0 ;;
    esac
    updated_sec=$((updated_ns / 1000000000))
    age=$((NOW_SEC - updated_sec))
    [ "$age" -lt 0 ] && age=0
    if [ "$age" -gt "$HEALTH_SILENCE_SECONDS" ]; then
      record_warning "$label: health.json stale (age ${age}s > ${HEALTH_SILENCE_SECONDS}s)"
    fi

    # A session id is deliberately constrained to the collector's opaque,
    # single-token identity format before it can enter the line-oriented state
    # file. The counter must be a non-negative integer JSON number; strings,
    # fractions, negatives, and missing fields are malformed rather than zero.
    session_id=$(jq -r '
      if (.session_id? | type) == "string" and (.session_id | length) > 0
        then .session_id else empty end' "$health_file" 2>/dev/null || true)
    sequence_gap_total=$(jq -r '
      if (.sequence_gap_total? | type) == "number"
        and (.sequence_gap_total | floor) == .sequence_gap_total
        and .sequence_gap_total >= 0
        then (.sequence_gap_total | tostring) else empty end' \
      "$health_file" 2>/dev/null || true)
    session_valid=0
    case "$session_id" in
      '' | *[!A-Za-z0-9._:-]*) ;;
      *) session_valid=1 ;;
    esac
    total_valid=0
    case "$sequence_gap_total" in
      '' | *[!0-9]*) ;;
      *) total_valid=1 ;;
    esac

    if [ "$session_valid" -eq 1 ]; then
      sequence_gap_session_json=$(jq -Rn --arg s "$session_id" '$s')
    fi
    if [ "$total_valid" -eq 1 ]; then
      sequence_gap_total_json=$sequence_gap_total
    fi

    if [ "$updated_ns_valid" -eq 0 ]; then
      record_warning "$label: health.json updated_at_ns malformed"
      sequence_gap_baseline=malformed
      preserve_sequence_prior
    elif [ "$session_valid" -eq 1 ] && [ "$total_valid" -eq 1 ]; then
      sequence_gap_observed=1
      if [ "$DRY_RUN" -eq 1 ]; then
        sequence_gap_baseline=dry_run
      elif [ -z "$prior_session" ] || [ -z "$prior_total" ]; then
        sequence_gap_baseline=baseline
        sequence_gap_previous_total_json=null
      else
        sequence_gap_previous_total_json=$prior_total
        if [ "$session_id" != "$prior_session" ]; then
          sequence_gap_baseline=session_changed
          record_warning "$label: sequence_gap session changed ($prior_session -> $session_id); baseline reset at total=$sequence_gap_total"
        elif [ "$sequence_gap_total" -gt "$prior_total" ]; then
          sequence_gap_delta=$((sequence_gap_total - prior_total))
          sequence_gap_delta_json=$sequence_gap_delta
          sequence_gap_baseline=increased
          record_warning "$label: sequence_gap_total increased $prior_total -> $sequence_gap_total (delta=$sequence_gap_delta)"
        elif [ "$sequence_gap_total" -lt "$prior_total" ]; then
          sequence_gap_baseline=regressed
          record_warning "$label: sequence_gap_total regressed $prior_total -> $sequence_gap_total; baseline reset"
        else
          sequence_gap_baseline=stable
          sequence_gap_delta_json=0
        fi
      fi
      if [ "$DRY_RUN" -eq 0 ]; then
        state_lines="$state_lines sequence_gap_session|$label=$session_id sequence_gap_total|$label=$sequence_gap_total"
      fi
    else
      record_warning "$label: health.json sequence counter malformed (session_id/sequence_gap_total)"
      sequence_gap_baseline=malformed
      # Preserve the prior valid baseline instead of replacing it with a
      # fabricated zero. This lets the next valid poll still detect a delta.
      preserve_sequence_prior
    fi
  fi
  hobj=$(jq -n --argjson age "$age" --argjson gaps "$gaps" \
    --argjson total "$sequence_gap_total_json" \
    --argjson delta "$sequence_gap_delta_json" \
    --argjson previous "$sequence_gap_previous_total_json" \
    --argjson session "$sequence_gap_session_json" \
    --arg baseline "$sequence_gap_baseline" \
    --argjson observed "$sequence_gap_observed" \
    --arg hw "$hwarn" --arg s "$hstatus" \
    '{age_seconds: $age, sequence_gaps: $gaps,
      sequence_gap_total: $total, sequence_gap_delta: $delta,
      sequence_gap_previous_total: $previous, session_id: $session,
      sequence_gap_observed: ($observed == 1),
      sequence_gap_baseline: $baseline,
      disk_warning: ($hw == "true"), status: $s}')
  health_json=$(jq -n --argjson base "$health_json" --arg k "$label" --argjson v "$hobj" \
    '$base + {($k): $v}')
}

queue_entry_count() {
  printf '%s\n' "$1" | awk 'NF { c += 1 } END { print c + 0 }'
}

queue_oldest_age() {
  queue_oldest_age_result=null
  [ -n "$1" ] || return 0
  oldest=0
  for entry in $1; do
    mtime=$(file_mtime "$entry")
    case "$mtime" in (*[!0-9]*|'') continue ;; esac
    if [ "$oldest" -eq 0 ] || [ "$mtime" -lt "$oldest" ]; then
      oldest=$mtime
    fi
  done
  if [ "$oldest" -gt 0 ]; then
    age=$((NOW_SEC - oldest))
    [ "$age" -lt 0 ] && age=0
    queue_oldest_age_result=$age
  fi
}

recovery_job_receipt_valid() {
  entry=$1
  market=$2
  name=${entry##*/}
  job_id=${3:-${name%.*}}
  receipt="$entry/job.json"
  owned_directory_not_writable "$entry" "$recovery_hft_owner_uid" \
    && owned_regular_file_not_writable "$receipt" "$recovery_root_owner_uid" \
    && jq -e \
      --arg schema monday.rust_lob_recovery_queue.v1 \
      --arg market "$market" \
      --arg job_id "$job_id" \
      --arg canonical_spool "$SPOOL_ROOT/binance-lob/$market" \
      --arg recovery_unit "binance-lob-archiver-recovery@$market.service" \
      '.schema == $schema
        and .market == $market
        and .job_id == $job_id
        and .canonical_spool == $canonical_spool
        and .recovery_unit == $recovery_unit
        and (.queued_at | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
        and (.release_sha256 | test("^[a-f0-9]{64}$"))
        and (.deployment_bundle_sha256 | test("^[a-f0-9]{64}$"))
        and (.deployment_source_revision | test("^[a-f0-9]{40,64}$"))
        and (.env_sha256 | test("^[a-f0-9]{64}$"))
        and .release_env == "recovery.env"' \
      "$receipt" >/dev/null 2>&1
}

recovery_isolation_marker_valid() {
  marker=$1
  market=$2
  queue_dir=$3
  owned_regular_file_not_writable "$marker" "$recovery_root_owner_uid" || return 1
  job_id=$(jq -er '.job_id | select(type == "string")' "$marker" 2>/dev/null) || return 1
  receipt_sha256=$(jq -er '.receipt_sha256 | select(type == "string")' "$marker" 2>/dev/null) || return 1
  printf '%s\n' "$job_id" \
    | grep -Eq "^[0-9]{8}T[0-9]{6}Z-${market}-[a-f0-9]{12}-[0-9]+$" \
    || return 1
  printf '%s\n' "$receipt_sha256" | grep -Eq '^[a-f0-9]{64}$' || return 1
  canonical_spool="$SPOOL_ROOT/binance-lob/$market"
  ready_dir="$queue_dir/$job_id.ready"
  jq -e \
    --arg schema monday.rust_lob_recovery_isolation.v1 \
    --arg job_id "$job_id" \
    --arg market "$market" \
    --arg canonical_spool "$canonical_spool" \
    --arg ready_dir "$ready_dir" \
    --arg receipt_sha256 "$receipt_sha256" \
    '.schema == $schema
      and .job_id == $job_id
      and .market == $market
      and .canonical_spool == $canonical_spool
      and .ready_dir == $ready_dir
      and .receipt_sha256 == $receipt_sha256' \
    "$marker" >/dev/null 2>&1 || return 1
  if [ -e "$ready_dir" ] || [ -L "$ready_dir" ]; then
    receipt_dir=$ready_dir
  else
    receipt_dir=$canonical_spool
  fi
  recovery_job_receipt_valid "$receipt_dir" "$market" "$job_id" || return 1
  actual_receipt_sha256=$(sha256sum "$receipt_dir/job.json" 2>/dev/null | awk '{print $1}')
  [ "$actual_receipt_sha256" = "$receipt_sha256" ]
}

check_recovery_queue_market() {
  market=$1
  queue_dir="$RECOVERY_QUEUE_ROOT/$market"
  label="binance-lob-recovery[$market]"
  ready_scan_failed=0
  running_scan_failed=0
  failed_scan_failed=0
  malformed_scan_failed=0
  legacy_scan_failed=0
  ready_entries=""
  running_entries=""
  failed_entries=""
  status_entries=""
  legacy_entries=""
  ready_count=0
  running_count=0
  failed_count=0
  malformed_count=0
  legacy_unreceipted_count=0
  isolation_active=0
  isolation_valid=0
  isolation_age=null
  ready_oldest_age=null
  running_oldest_age=null
  failed_oldest_age=null

  if [ "$recovery_queue_root_ok" -eq 1 ] && { [ -e "$queue_dir" ] || [ -L "$queue_dir" ]; }; then
    if ! owned_collector_traversable_directory "$queue_dir" "$recovery_root_owner_uid" "$recovery_hft_group_gid" \
      || [ ! -r "$queue_dir" ] || [ ! -x "$queue_dir" ]; then
      record_breach "$label: recovery queue root is not an inspectable directory ($queue_dir)"
    else
      isolation_marker="$queue_dir/isolation.json"
      if [ -e "$isolation_marker" ] || [ -L "$isolation_marker" ]; then
        isolation_active=1
        marker_mtime=$(file_mtime "$isolation_marker")
        case "$marker_mtime" in
          *[!0-9]* | '') ;;
          *)
            isolation_age=$((NOW_SEC - marker_mtime))
            [ "$isolation_age" -lt 0 ] && isolation_age=0
            ;;
        esac
        if recovery_isolation_marker_valid "$isolation_marker" "$market" "$queue_dir"; then
          isolation_valid=1
          record_breach "$label: unfinished isolation transaction present (age ${isolation_age}s)"
        else
          record_breach "$label: malformed isolation transaction present (age ${isolation_age}s)"
        fi
      fi
      ready_entries=$(find "$queue_dir" -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print 2>/dev/null) \
        || ready_scan_failed=1
      running_entries=$(find "$queue_dir" -mindepth 1 -maxdepth 1 -type d -name '*.running' -print 2>/dev/null) \
        || running_scan_failed=1
      failed_entries=$(find "$queue_dir" -mindepth 1 -maxdepth 1 -type d -name '*.failed' -print 2>/dev/null) \
        || failed_scan_failed=1
      status_entries=$(find "$queue_dir" -mindepth 1 -maxdepth 1 \
        \( -name '*.ready' -o -name '*.running' -o -name '*.failed' \) -print 2>/dev/null) \
        || malformed_scan_failed=1
      if [ "${ready_scan_failed:-0}" -eq 1 ] \
        || [ "${running_scan_failed:-0}" -eq 1 ] \
        || [ "${failed_scan_failed:-0}" -eq 1 ] \
        || [ "${malformed_scan_failed:-0}" -eq 1 ]; then
        record_breach "$label: recovery queue scan failed ($queue_dir)"
      else
        ready_count=$(queue_entry_count "$ready_entries")
        running_count=$(queue_entry_count "$running_entries")
        failed_count=$(queue_entry_count "$failed_entries")
        queue_oldest_age "$ready_entries"
        ready_oldest_age=$queue_oldest_age_result
        queue_oldest_age "$running_entries"
        running_oldest_age=$queue_oldest_age_result
        queue_oldest_age "$failed_entries"
        failed_oldest_age=$queue_oldest_age_result
        for entry in $status_entries; do
          if ! recovery_job_receipt_valid "$entry" "$market"; then
            malformed_count=$((malformed_count + 1))
          fi
        done
      fi

      legacy_dir="$queue_dir/legacy-unreceipted"
      if [ -e "$legacy_dir" ] || [ -L "$legacy_dir" ]; then
        if ! owned_directory_not_writable "$legacy_dir" "$recovery_root_owner_uid" \
          || [ ! -r "$legacy_dir" ] || [ ! -x "$legacy_dir" ]; then
          record_breach "$label: legacy-unreceipted is not an inspectable root-owned directory ($legacy_dir)"
        else
          legacy_entries=$(find "$legacy_dir" -mindepth 1 -maxdepth 1 -type d -print 2>/dev/null) \
            || legacy_scan_failed=1
          if [ "${legacy_scan_failed:-0}" -eq 1 ]; then
            record_breach "$label: legacy-unreceipted scan failed ($legacy_dir)"
          else
            legacy_unreceipted_count=$(queue_entry_count "$legacy_entries")
          fi
        fi
      fi
    fi
  fi

  if [ "$malformed_count" -gt 0 ]; then
    record_breach "$label: malformed recovery job receipt(s) present ($malformed_count)"
  fi
  if [ "$legacy_unreceipted_count" -gt 0 ]; then
    record_breach "$label: legacy unreceipted recovery job(s) present ($legacy_unreceipted_count)"
  fi
  if [ "$failed_count" -gt 0 ]; then
    record_breach "$label: failed recovery job(s) present ($failed_count)"
  fi
  if [ "$ready_oldest_age" != null ] && [ "$ready_oldest_age" -gt "$RECOVERY_QUEUE_READY_MAX_AGE" ]; then
    record_breach "$label: oldest ready recovery job age ${ready_oldest_age}s over ${RECOVERY_QUEUE_READY_MAX_AGE}s"
  fi
  if [ "$running_oldest_age" != null ] && [ "$running_oldest_age" -gt "$RECOVERY_QUEUE_RUNNING_MAX_AGE" ]; then
    record_breach "$label: oldest running recovery job age ${running_oldest_age}s over ${RECOVERY_QUEUE_RUNNING_MAX_AGE}s"
  fi

  qobj=$(jq -n \
    --argjson rc "$ready_count" \
    --argjson ra "$ready_oldest_age" \
    --argjson uc "$running_count" \
    --argjson ua "$running_oldest_age" \
    --argjson fc "$failed_count" \
    --argjson fa "$failed_oldest_age" \
    --argjson mc "$malformed_count" \
    --argjson lc "$legacy_unreceipted_count" \
    --argjson ia "$isolation_active" \
    --argjson iv "$isolation_valid" \
    --argjson ig "$isolation_age" \
    '{ready_count: $rc, ready_oldest_age_seconds: $ra,
      running_count: $uc, running_oldest_age_seconds: $ua,
      failed_count: $fc, failed_oldest_age_seconds: $fa,
      malformed_count: $mc, legacy_unreceipted_count: $lc,
      isolation_active: ($ia == 1), isolation_valid: ($iv == 1),
      isolation_age_seconds: $ig}')
  recovery_queue_json=$(jq -n --argjson base "$recovery_queue_json" --arg k "$market" --argjson v "$qobj" \
    '$base + {($k): $v}')
}

check_recovery_queue_root() {
  recovery_queue_root_ok=1
  if [ -e "$RECOVERY_QUEUE_ROOT" ] || [ -L "$RECOVERY_QUEUE_ROOT" ]; then
    if [ "${MONDAY_COLLECTOR_HEALTH_TEST_MODE:-0}" = 1 ]; then
      recovery_root_owner_uid=$(id -u)
      recovery_hft_owner_uid=$recovery_root_owner_uid
      recovery_hft_group_gid=${MONDAY_COLLECTOR_HEALTH_TEST_HFT_GID:-$(id -g)}
    elif ! recovery_hft_owner_uid=$(id -u hftcollector 2>/dev/null) \
      || ! recovery_hft_group_gid=$(id -g hftcollector 2>/dev/null); then
      recovery_queue_root_ok=0
      record_breach "binance-lob-recovery: hftcollector owner identity is unavailable"
      return
    fi
    if ! owned_collector_traversable_directory "$RECOVERY_QUEUE_ROOT" "$recovery_root_owner_uid" "$recovery_hft_group_gid" \
      || [ ! -r "$RECOVERY_QUEUE_ROOT" ] || [ ! -x "$RECOVERY_QUEUE_ROOT" ]; then
      recovery_queue_root_ok=0
      record_breach "binance-lob-recovery: recovery queue root is not an inspectable directory ($RECOVERY_QUEUE_ROOT)"
    fi
  fi
}

# Pending-backlog scanners. Each sets pending_count and pending_oldest (epoch
# mtime of the oldest pending artifact, 0 when none), mirroring the collector's
# own pending definition for that lane.
scan_pending_glob() {
  # $1 = glob for pending artifacts (LOB manifests, polymarket rotated tapes)
  pending_count=0
  pending_oldest=0
  for entry in $1; do
    [ -f "$entry" ] || continue
    pending_count=$((pending_count + 1))
    mtime=$(file_mtime "$entry")
    case "$mtime" in (*[!0-9]*|'') continue ;; esac
    if [ "$pending_oldest" -eq 0 ] || [ "$mtime" -lt "$pending_oldest" ]; then
      pending_oldest=$mtime
    fi
  done
}

scan_pending_lake() {
  # $1 = output root; pending batches are lake/raw/**/batch=* directories. The
  # fee and usdm-reference uploaders remove each batch directory after a
  # verified upload, so a surviving batch directory is exactly the backlog.
  pending_count=0
  pending_oldest=0
  [ -d "$1/lake/raw" ] || return 0
  # Capture find's exit status: a traversal error (permissions, I/O) must set
  # pending_scan_failed so the caller can breach instead of reading a partial
  # scan as an empty backlog.
  scan_out=$(find "$1/lake/raw" -type d -name 'batch=*' 2>/dev/null) || pending_scan_failed=1
  # Batch directory names come from the governed lake layout (no whitespace).
  for dir in $scan_out; do
    pending_count=$((pending_count + 1))
    mtime=$(file_mtime "$dir")
    case "$mtime" in (*[!0-9]*|'') continue ;; esac
    if [ "$pending_oldest" -eq 0 ] || [ "$mtime" -lt "$pending_oldest" ]; then
      pending_oldest=$mtime
    fi
  done
}

scan_pending_bybit_raw() {
  # $1 = spool dir; pending = rotated .ndjson segments whose manifest+_SUCCESS
  # markers exist (rotation finished publishing) and whose .uploaded.json
  # readback marker is absent (mirrors upload_pending in
  # bybit-options-archiver.rs).
  pending_count=0
  pending_oldest=0
  for entry in "$1"/*.ndjson; do
    [ -f "$entry" ] || continue
    [ -f "$entry.manifest.json" ] || continue
    [ -f "$entry._SUCCESS" ] || continue
    [ ! -e "$entry.uploaded.json" ] || continue
    pending_count=$((pending_count + 1))
    mtime=$(file_mtime "$entry")
    case "$mtime" in (*[!0-9]*|'') continue ;; esac
    if [ "$pending_oldest" -eq 0 ] || [ "$mtime" -lt "$pending_oldest" ]; then
      pending_oldest=$mtime
    fi
  done
}

check_upload_lane() {
  # The four hard gates, applied per upload lane:
  #   gate 1 (mandated lanes only): upload-status.json exists and parses.
  #   gate 2: last_success_at present and younger than $success_max_age.
  #           When $8 (pending_stale_max_age) is set, a pending artifact older
  #           than that tighter stall bound is also a breach.
  #   gate 3: pending backlog count/age under $pending_max/$pending_max_age.
  #   gate 4: last_error empty and failure_count not growing (cumulative
  #           counter vs the previous poll; a first observation with a
  #           nonzero count is a breach because the failures were never
  #           acknowledged by this monitor).
  label=$1
  spool_dir=$2
  required=$3
  success_max_age=$4
  pending_max=$5
  pending_max_age=$6
  pending_kind=$7
  pending_stale_max_age=${8:-0}
  case "$pending_stale_max_age" in (*[!0-9]*|'') pending_stale_max_age=0 ;; esac
  upload_file="$spool_dir/upload-status.json"

  emit_lane_json() {
    uploads_json=$(jq -n --argjson base "$uploads_json" --arg k "$label" --argjson v "$1" \
      '$base + {($k): $v}')
  }

  # Gate 3 runs first so a missing status file on a non-mandated lane cannot
  # hide a real backlog: the pending artifacts live on disk independently of
  # the uploader's status reporting. A scan that cannot inspect the spool is
  # a breach (fail closed), never a silent zero backlog.
  pending_count=0
  pending_oldest=0
  pending_scan_failed=0
  if [ ! -d "$spool_dir" ] || [ ! -r "$spool_dir" ]; then
    pending_scan_failed=1
  else
    case "$pending_kind" in
      manifests) scan_pending_glob "$spool_dir/*.manifest.json" ;;
      lake) scan_pending_lake "$spool_dir" ;;
      tapes) scan_pending_glob "$spool_dir/market-updates.*.ndjson" ;;
      bybit-raw) scan_pending_bybit_raw "$spool_dir" ;;
    esac
  fi
  if [ "$pending_scan_failed" -eq 1 ]; then
    record_breach "$label: pending upload backlog scan failed ($spool_dir)"
  fi
  pending_age=0
  if [ "$pending_oldest" -gt 0 ]; then
    pending_age=$((NOW_SEC - pending_oldest))
    [ "$pending_age" -lt 0 ] && pending_age=0
  fi
  if [ "$pending_count" -gt "$pending_max" ]; then
    record_breach "$label: pending upload backlog $pending_count over limit $pending_max"
  fi
  if [ "$pending_age" -gt "$pending_max_age" ]; then
    record_breach "$label: oldest pending upload backlog age ${pending_age}s over ${pending_max_age}s"
  fi
  # Gate 2 addendum (lanes passing $8): a rotated tape sitting unuploaded
  # longer than the tighter stall bound means the uploader is stalled even
  # when the lane cadence bound has not elapsed. Keyed on the oldest pending
  # artifact's age rather than last_success_at: with hourly tape rotation the
  # last success is legitimately ~60 minutes old in the minutes between a
  # rotation and the next timer run, so a success-age condition would page on
  # a healthy lane.
  if [ "$pending_stale_max_age" -gt 0 ] && [ "$pending_count" -gt 0 ] \
    && [ "$pending_age" -gt "$pending_stale_max_age" ]; then
    record_breach "$label: $pending_count pending upload(s) stalled (oldest age ${pending_age}s > ${pending_stale_max_age}s)"
  fi

  if [ ! -f "$upload_file" ] || [ -L "$upload_file" ]; then
    if [ "$required" = 1 ]; then
      record_breach "$label: upload-status.json missing or a symbolic link"
    else
      record_warning "$label: upload-status.json missing or a symbolic link"
    fi
    uobj=$(jq -n --argjson pc "$pending_count" --argjson pa "$pending_age" \
      '{last_success_at: null, last_success_age_seconds: null, last_error_at: null, last_error: null, failure_count: 0, failure_delta: false, pending_count: $pc, oldest_pending_age_seconds: $pa}')
    emit_lane_json "$uobj"
    return 0
  fi

  if [ "$required" = 1 ]; then
    # Mandated lanes must carry a usable cumulative failure_count.
    upload_filter='if type == "object"
      and has("failure_count")
      and (.failure_count != null)
      and (.failure_count | type == "number")
      and (.failure_count | floor == .)
      and (.failure_count >= 0)
    then . else error("invalid upload status") end'
  else
    upload_filter='if type == "object" then . else error("invalid upload status") end'
  fi
  if ! upload_json=$(jq -ce "$upload_filter" "$upload_file" 2>/dev/null); then
    record_breach "$label: upload-status.json is malformed"
    uobj=$(jq -n --argjson pc "$pending_count" --argjson pa "$pending_age" \
      '{last_success_at: null, last_success_age_seconds: null, last_error_at: null, last_error: null, failure_count: 0, failure_delta: false, pending_count: $pc, oldest_pending_age_seconds: $pa}')
    emit_lane_json "$uobj"
    return 0
  fi

  # Gate 2: last_success_at presence + freshness. The real emitters produce
  # RFC3339 with six fractional digits and a Z suffix (polymarket_upload::
  # utc_now, reused by the fee and usdm-reference uploaders) or Chrono
  # to_rfc3339() with a +00:00 offset and optional fractional seconds (LOB);
  # the bybit lane writes epoch milliseconds. jq's fromdateiso8601 only
  # accepts the whole-second Z form, so normalize first; non-UTC offsets are
  # refused rather than silently reinterpreted.
  success_raw=$(printf '%s' "$upload_json" | jq -c '(.last_success_at // null)')
  success_epoch=$(printf '%s' "$upload_json" | jq -r '
    (.last_success_at // null)
    | if . == null then empty
      elif type == "number" then (if . > 100000000000 then (. / 1000) else . end | floor)
      elif type == "string" then
        if test("[+-](0[1-9]|[1-9][0-9]):[0-9]{2}$") then empty
        else ( sub("([Zz]|[+-]00:00)$"; "")
               | sub("\\.[0-9]+$"; "")
               | . + "Z"
               | try fromdateiso8601 catch empty )
        end
      else empty
      end' 2>/dev/null)
  case "$success_epoch" in (*[!0-9]*|'') success_epoch="" ;; esac
  success_age_json=null
  if [ -z "$success_epoch" ]; then
    record_breach "$label: upload-status.json missing a parseable last_success_at"
  else
    success_age=$((NOW_SEC - success_epoch))
    [ "$success_age" -lt 0 ] && success_age=0
    success_age_json=$success_age
    if [ "$success_age" -gt "$success_max_age" ]; then
      record_breach "$label: last upload success stale (age ${success_age}s > ${success_max_age}s)"
    fi
  fi

  # Gate 4: last_error must be empty and the cumulative failure_count must not
  # grow between polls.
  err_at=$(printf '%s' "$upload_json" | jq -r '(.last_error_at // null)')
  err_msg=$(printf '%s' "$upload_json" | jq -r '(.last_error // null)')
  failure_count=$(printf '%s' "$upload_json" | jq -r '(.failure_count // 0)')
  case "$failure_count" in (*[!0-9]*|'') failure_count=0 ;; esac
  failure_delta=0
  if [ -n "$err_at" ] && [ "$err_at" != "null" ]; then
    record_breach "$label: upload last_error_at=$err_at"
  fi
  if [ -n "$err_msg" ] && [ "$err_msg" != "null" ]; then
    record_breach "$label: upload last_error=$err_msg"
  fi
  prior=$(read_prior "failure_count|$label")
  if [ "$DRY_RUN" -eq 0 ]; then
    case "$prior" in (*[!0-9]*|'') prior="" ;; esac
    if [ -z "$prior" ] && [ "$failure_count" -gt 0 ]; then
      failure_delta=1
      record_breach "$label: initial upload failure_count=$failure_count"
    elif [ -n "$prior" ] && [ "$failure_count" -gt "$prior" ]; then
      failure_delta=1
      record_breach "$label: upload failure_count increased $prior -> $failure_count"
    fi
  fi
  state_lines="$state_lines failure_count|$label=$failure_count"

  uobj=$(jq -n --argjson s "$success_raw" --argjson sa "$success_age_json" \
    --arg e "$err_at" --arg m "$err_msg" --argjson f "$failure_count" \
    --argjson d "$failure_delta" --argjson pc "$pending_count" --argjson pa "$pending_age" \
    '{last_success_at: $s, last_success_age_seconds: $sa, last_error_at: $e, last_error: $m, failure_count: $f, failure_delta: ($d == 1), pending_count: $pc, oldest_pending_age_seconds: $pa}')
  emit_lane_json "$uobj"
}

mark_delay_gate_replaced() {
  # Keep the historical delay_gate projection for consumers that still parse
  # it, but make the replacement explicit. The typed health counters are the
  # sole source for sequence-gap/reconnect evidence; no journal scan occurs.
  for unit in "$ARCHIVER_SPOT" "$ARCHIVER_USDM"; do
    dobj=$(jq -n \
      '{trips_15m: null, observed: false,
        skipped_reason: "replaced_by_health_sequence_counters",
        replacement: "checks.health"}')
    delay_gate_json=$(jq -n --argjson base "$delay_gate_json" --arg k "$unit" \
      --argjson v "$dobj" '$base + {($k): $v}')
  done
}

write_state() {
  [ "$DRY_RUN" -eq 1 ] && return 0
  # A state-persistence failure means the next poll can lose restart/upload
  # deltas while the monitor reports healthy, so record it as a breach rather
  # than only logging: gate 4 delta detection depends on this state.
  if ! mkdir -p "$STATE_DIR" 2>/dev/null; then
    record_breach "state: state directory unavailable: $STATE_DIR"
    return 0
  fi
  tmp="$STATE_FILE.$$"
  if ! : > "$tmp" 2>/dev/null; then
    record_breach "state: cannot create state file $tmp"
    return 0
  fi
  for entry in $state_lines; do
    printf '%s\n' "$entry" >> "$tmp"
  done
  if ! mv "$tmp" "$STATE_FILE" 2>/dev/null; then
    rm -f "$tmp" 2>/dev/null
    record_breach "state: cannot persist state file $STATE_FILE"
  fi
}

NOW_SEC=$(date +%s)
CHECKED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)

check_mount
check_disk

check_service "$ARCHIVER_SPOT" "binance-lob-archiver-production@spot"
check_service "$ARCHIVER_USDM" "binance-lob-archiver-production@usdm"
check_timer "$RECOVERY_SPOT_TIMER" "binance-lob-archiver-recovery@spot.timer" "$ARCHIVER_SPOT"
check_timer "$RECOVERY_USDM_TIMER" "binance-lob-archiver-recovery@usdm.timer" "$ARCHIVER_USDM"
check_service "$REFERENCE_COLLECTOR" "binance-usdm-reference-collector"
check_service "$BYBIT_ARCHIVER" "bybit-options-archiver"

check_scheduled_timer "$POLY_MARKET_UPLOAD_TIMER" "polymarket-market-tape-upload.timer" "$POLY_MARKET_UPLOAD_SERVICE"
check_oneshot_result "$POLY_MARKET_UPLOAD_SERVICE" "polymarket-market-tape-upload.service"
check_scheduled_timer "$POLY_REF_UPLOAD_TIMER" "polymarket-reference-upload.timer" "$POLY_REF_UPLOAD_SERVICE"
check_oneshot_result "$POLY_REF_UPLOAD_SERVICE" "polymarket-reference-upload.service"
check_upload_timer_backed "$POLY_MARKET_COLLECTOR" "$POLY_MARKET_UPLOAD_TIMER" "polymarket-market-tape-upload.timer"
check_upload_timer_backed "$POLY_REF_COLLECTOR" "$POLY_REF_UPLOAD_TIMER" "polymarket-reference-upload.timer"
check_scheduled_timer "$WATCHDOG_TIMER" "polymarket-market-tape-upload-watchdog.timer"
check_oneshot_result "$WATCHDOG_SERVICE" "polymarket-market-tape-upload-watchdog.service"
check_timer "$BYBIT_UPLOAD_TIMER" "bybit-options-upload.timer"
check_oneshot_result "$BYBIT_UPLOAD_SERVICE" "bybit-options-upload.service"
check_timer "$USDM_REF_UPLOAD_TIMER" "binance-usdm-reference-upload.timer"
check_oneshot_result "$USDM_REF_UPLOAD_SERVICE" "binance-usdm-reference-upload.service"
check_timer "$FEE_SPOT_TIMER" "binance-fee-snapshot-spot.timer"
check_oneshot_result "$FEE_SPOT_SERVICE" "binance-fee-snapshot-spot.service"
check_timer "$FEE_USDM_TIMER" "binance-fee-snapshot-usdm.timer"
check_oneshot_result "$FEE_USDM_SERVICE" "binance-fee-snapshot-usdm.service"
check_timer "$FEE_UPLOAD_TIMER" "binance-fee-upload.timer"
check_oneshot_result "$FEE_UPLOAD_SERVICE" "binance-fee-upload.service"

check_raw_ops_gate "$POLY_RAW_OPS_GATE" "polymarket-raw-ops-gate"

check_binance_health "binance-lob-archiver-production@spot" "$SPOOL_ROOT/binance-lob/spot"
check_binance_health "binance-lob-archiver-production@usdm" "$SPOOL_ROOT/binance-lob/usdm"
mark_delay_gate_replaced
check_recovery_queue_root
check_recovery_queue_market spot
check_recovery_queue_market usdm

check_upload_lane "binance-lob-archiver-production@spot" "$SPOOL_ROOT/binance-lob/spot" \
  1 "$LOB_SUCCESS_MAX_AGE" "$LOB_PENDING_MAX" "$LOB_PENDING_MAX_AGE" manifests
check_upload_lane "binance-lob-archiver-production@usdm" "$SPOOL_ROOT/binance-lob/usdm" \
  1 "$LOB_SUCCESS_MAX_AGE" "$LOB_PENDING_MAX" "$LOB_PENDING_MAX_AGE" manifests
check_upload_lane "binance-usdm-reference-collector" "$SPOOL_ROOT/binance-usdm-reference" \
  0 "$REF_SUCCESS_MAX_AGE" "$REF_PENDING_MAX" "$REF_PENDING_MAX_AGE" lake
check_upload_lane "bybit-options-upload" "$SPOOL_ROOT/bybit-options" \
  0 "$BYBIT_SUCCESS_MAX_AGE" "$BYBIT_PENDING_MAX" "$BYBIT_PENDING_MAX_AGE" bybit-raw
check_upload_lane "polymarket-market-tape-upload" "$SPOOL_ROOT/polymarket" \
  0 "$POLY_SUCCESS_MAX_AGE" "$POLY_PENDING_MAX" "$POLY_PENDING_MAX_AGE" tapes "$POLY_PENDING_STALE_MAX_AGE"
check_upload_lane "polymarket-reference-upload" "$SPOOL_ROOT/polymarket-reference" \
  0 "$POLY_SUCCESS_MAX_AGE" "$POLY_PENDING_MAX" "$POLY_PENDING_MAX_AGE" tapes "$POLY_PENDING_STALE_MAX_AGE"
check_upload_lane "binance-fee-upload" "$SPOOL_ROOT/binance-fee" \
  1 "$FEE_SUCCESS_MAX_AGE" "$FEE_PENDING_MAX" "$FEE_PENDING_MAX_AGE" lake

write_state

if [ "$breach_count" -gt 0 ]; then
  ok_str=false
else
  ok_str=true
fi

if [ "$JSON_MODE" -eq 1 ]; then
  breaches_json=$(printf '%s' "$breaches" | jq -Rs 'split("\n") | map(select(length > 0))')
  warnings_json=$(printf '%s' "$warnings" | jq -Rs 'split("\n") | map(select(length > 0))')
  checks_json=$(jq -n --argjson disk "$disk_json" --argjson mount "$mount_json" \
    --argjson units "$units_json" --argjson health "$health_json" \
    --argjson uploads "$uploads_json" --argjson queue "$recovery_queue_json" \
    --argjson delay "$delay_gate_json" \
    '{disk: $disk, mount: $mount, units: $units, health: $health, uploads: $uploads, recovery_queue: $queue, delay_gate: $delay}')
  jq -n --argjson ok "$ok_str" --arg checked "$CHECKED_AT" \
    --argjson breaches "$breaches_json" --argjson warnings "$warnings_json" \
    --argjson checks "$checks_json" \
    '{ok: $ok, checked_at: $checked, breaches: $breaches, warnings: $warnings, checks: $checks}'
else
  if [ "$breach_count" -gt 0 ]; then
    printf 'ok:false\n'
    printf '%s\n' "$breaches" | sed 's/^/breach: /'
  else
    printf 'ok:true\n'
  fi
  if [ -n "$warnings" ]; then
    printf '%s\n' "$warnings" | sed 's/^/warning: /'
  fi
fi

if [ "$breach_count" -gt 0 ]; then
  log err "$breach_count collector-health breach(es) detected"
  exit 1
fi
log info "all collector-health checks passed"
exit 0
