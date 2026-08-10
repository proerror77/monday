#!/bin/sh
#
# monday-collector-health.sh - durable read-only health monitor for the Monday
# public-data collector host (monday-trade-data-26, Aliyun Tokyo ap-northeast-1).
#
# Guards against a silent recurrence of the 2026-08-05/06 disk-full incident in
# which every governed collector stopped, uploads failed, and delay-gate trips
# accumulated while the only on-host monitor (polymarket-market-tape-upload-
# watchdog.sh) self-healed without ever alerting a human.
#
# Health contract: exactly FOUR hard gates, each a breach that fails closed
# into the monitor-collector-host workflow issue, plus the expected-disabled
# installation gates:
#   1. upload-status.json exists and parses for the mandated lanes
#      (binance-lob spot/usdm and binance-fee); missing or malformed = breach.
#   2. last_success_at is present and fresh on every upload lane. Thresholds
#      sit just above each lane's upload cadence (see the *_SUCCESS_MAX_AGE
#      constants). A missing/unparseable last_success_at is a breach: the
#      delivery loop is unproven. The fee uploader gains last_success_at in a
#      parallel change; until that deploys, the fee lane breaches by design.
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
# polymarket-raw-ops-gate stays a breach when is-enabled is anything but
# disabled/masked/not-found (e.g. 'static'): it proves an uncleaned host
# installation. State-persistence failures also stay breaches because gate 4
# delta detection depends on the persisted state.
#
# Everything else is a WARNING: unit/timer active+enabled state, systemd
# Result, restart-rate deltas, health.json freshness/gaps, journald delay-gate
# trips, fee snapshot journal failures, disk space, and the /data mount.
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
set -u

TAG=monday-collector-health
SPOOL_ROOT=${MONDAY_COLLECTOR_SPOOL_ROOT:-/data/monday/spool}
STATE_DIR=${MONDAY_COLLECTOR_STATE_DIR:-/var/lib/monday-collector-health}
STATE_FILE="$STATE_DIR/state.json"

HEALTH_SILENCE_SECONDS=300
DISK_WARN_PERCENT=25
DISK_CRIT_PERCENT=10
RESTART_MAX_DELTA=1
# journalctl --since value; must be a timestamp journalctl can parse
# ("15min" is rejected with "Failed to parse timestamp" and would read as a
# permanent journald-query warning).
DELAY_GATE_WINDOW='15 min ago'
FEE_FAILURE_WINDOW='10 min ago'

# Gate 2: last_success_at freshness per lane, set just above the lane's upload
# cadence:
# - LOB segments rotate every SEGMENT_SECONDS (default 3600s) and the
#   in-process upload loop runs every 300s, so a healthy lane uploads at least
#   once per rotation; allow two full rotations.
# - fee snapshots publish every 60s and binance-fee-upload.timer retries every
#   60s (mirrors the FEE_FAILURE_WINDOW='10 min ago' journal window).
# - usdm-reference and both polymarket lanes run on 5-minute upload timers.
# - bybit options segments finalize on the hour and the upload timer sweeps
#   them at :23, so 90 minutes covers one full finalize+sweep cycle.
LOB_SUCCESS_MAX_AGE=7200
FEE_SUCCESS_MAX_AGE=600
REF_SUCCESS_MAX_AGE=1200
POLY_SUCCESS_MAX_AGE=1800
BYBIT_SUCCESS_MAX_AGE=5400

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
REFERENCE_COLLECTOR=binance-usdm-reference-collector.service
POLY_MARKET_UPLOAD_TIMER=polymarket-market-tape-upload.timer
POLY_MARKET_UPLOAD_SERVICE=polymarket-market-tape-upload.service
POLY_REF_UPLOAD_TIMER=polymarket-reference-upload.timer
POLY_REF_UPLOAD_SERVICE=polymarket-reference-upload.service
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

file_mtime() {
  # Portable mtime: GNU stat on the host, BSD stat under the macOS test stubs.
  stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null
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
    record_warning "mount: /data is not mounted"
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
    if [ "$disk_free_percent" -lt "$DISK_CRIT_PERCENT" ]; then
      disk_critical=1
      record_warning "disk: /data free ${disk_free_percent}% (${disk_free_gb}GiB) below critical ${DISK_CRIT_PERCENT}%"
    elif [ "$disk_free_percent" -lt "$DISK_WARN_PERCENT" ]; then
      disk_warning=1
      record_warning "disk: /data free ${disk_free_percent}% (${disk_free_gb}GiB) below warning ${DISK_WARN_PERCENT}%"
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
  active=$(unit_is_active "$unit")
  enabled=$(unit_is_enabled "$unit")
  [ "$active" = "active" ] || record_warning "$label: timer not active (is-active='$active')"
  [ "$enabled" = "enabled" ] || record_warning "$label: timer not enabled (is-enabled='$enabled')"
  obj=$(jq -n --arg a "$active" --arg e "$enabled" \
    '{active: ($a == "active"), enabled: ($e == "enabled")}')
  units_json=$(jq -n --argjson base "$units_json" --arg k "$unit" --argjson v "$obj" \
    '$base + {($k): $v}')
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

check_disabled_unit() {
  # The raw-ops gate template must remain DISABLED. Breach on any is-enabled
  # state that is not explicitly disabled/masked/not-found (enabled, static,
  # indirect, ... all mean the unit can be activated and the host still carries
  # an uncleaned installation).
  unit=$1
  label=$2
  enabled=$(unit_is_enabled "$unit")
  case "$enabled" in
    disabled|masked|not-found|'') ;;
    *) record_breach "$label: expected disabled but is-enabled='$enabled'" ;;
  esac
  obj=$(jq -n --arg e "$enabled" '{is_enabled: $e}')
  units_json=$(jq -n --argjson base "$units_json" --arg k "$unit" --argjson v "$obj" \
    '$base + {($k): $v}')
}

check_binance_health() {
  # health.json at /data/monday/spool/binance-lob/<market>/: freshness and
  # sequence gaps are soft signals (warnings); the hard LOB gates run on
  # upload-status.json and the pending segment backlog.
  label=$1
  spool_dir=$2
  health_file="$spool_dir/health.json"
  age=0
  gaps=0
  hwarn=false
  hstatus=unknown
  if [ ! -f "$health_file" ] || [ -L "$health_file" ]; then
    record_warning "$label: health.json missing or a symbolic link ($health_file)"
    age=999999
  elif ! updated_ns=$(jq -r '.updated_at_ns // 0' "$health_file" 2>/dev/null); then
    record_warning "$label: health.json unparseable ($health_file)"
    age=999999
  else
    gaps=$(jq -r '.sequence_gaps // 0' "$health_file" 2>/dev/null || printf '0')
    hwarn=$(jq -r '.disk_warning // false' "$health_file" 2>/dev/null || printf 'false')
    hstatus=$(jq -r '.status // "unknown"' "$health_file" 2>/dev/null || printf 'unknown')
    case "$gaps" in (*[!0-9]*|'') gaps=0 ;; esac
    case "$updated_ns" in (*[!0-9]*|'') updated_ns=0 ;; esac
    updated_sec=$((updated_ns / 1000000000))
    age=$((NOW_SEC - updated_sec))
    [ "$age" -lt 0 ] && age=0
    if [ "$age" -gt "$HEALTH_SILENCE_SECONDS" ]; then
      record_warning "$label: health.json stale (age ${age}s > ${HEALTH_SILENCE_SECONDS}s)"
    fi
    if [ "$gaps" -gt 0 ]; then
      record_warning "$label: sequence_gaps=$gaps"
    fi
  fi
  hobj=$(jq -n --argjson age "$age" --argjson gaps "$gaps" --arg hw "$hwarn" --arg s "$hstatus" \
    '{age_seconds: $age, sequence_gaps: $gaps, disk_warning: ($hw == "true"), status: $s}')
  health_json=$(jq -n --argjson base "$health_json" --arg k "$label" --argjson v "$hobj" \
    '$base + {($k): $v}')
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
  # Single-pass scan inside a pipe subshell (POSIX sh has no process
  # substitution); the subshell prints "<count> <oldest-mtime>" once for the
  # parent to unpack.
  pending_stats=$(find "$1/lake/raw" -type d -name 'batch=*' 2>/dev/null | {
    count=0
    oldest=0
    while IFS= read -r dir; do
      count=$((count + 1))
      mtime=$(file_mtime "$dir")
      case "$mtime" in (*[!0-9]*|'') continue ;; esac
      if [ "$oldest" -eq 0 ] || [ "$mtime" -lt "$oldest" ]; then
        oldest=$mtime
      fi
    done
    printf '%s %s\n' "$count" "$oldest"
  })
  pending_count=${pending_stats%% *}
  pending_oldest=${pending_stats##* }
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
  upload_file="$spool_dir/upload-status.json"

  emit_lane_json() {
    uploads_json=$(jq -n --argjson base "$uploads_json" --arg k "$label" --argjson v "$1" \
      '$base + {($k): $v}')
  }

  if [ ! -f "$upload_file" ] || [ -L "$upload_file" ]; then
    if [ "$required" = 1 ]; then
      record_breach "$label: upload-status.json missing or a symbolic link"
    else
      record_warning "$label: upload-status.json missing or a symbolic link"
    fi
    emit_lane_json '{"last_success_at": null, "last_success_age_seconds": null, "last_error_at": null, "last_error": null, "failure_count": 0, "failure_delta": false, "pending_count": 0, "oldest_pending_age_seconds": 0}'
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
    emit_lane_json '{"last_success_at": null, "last_success_age_seconds": null, "last_error_at": null, "last_error": null, "failure_count": 0, "failure_delta": false, "pending_count": 0, "oldest_pending_age_seconds": 0}'
    return 0
  fi

  # Gate 2: last_success_at presence + freshness. RFC3339 strings on the
  # binance/polymarket lanes, epoch milliseconds on the bybit lane.
  success_raw=$(printf '%s' "$upload_json" | jq -c '(.last_success_at // null)')
  success_epoch=$(printf '%s' "$upload_json" | jq -r '
    (.last_success_at // null)
    | if . == null then empty
      elif type == "number" then (if . > 100000000000 then (. / 1000) else . end | floor)
      else (try fromdateiso8601 catch empty)
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

  # Gate 3: pending backlog count + oldest pending age.
  pending_count=0
  pending_oldest=0
  case "$pending_kind" in
    manifests) scan_pending_glob "$spool_dir/*.manifest.json" ;;
    lake) scan_pending_lake "$spool_dir" ;;
    tapes) scan_pending_glob "$spool_dir/market-updates.*.ndjson" ;;
    bybit-raw) scan_pending_bybit_raw "$spool_dir" ;;
  esac
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

  uobj=$(jq -n --argjson s "$success_raw" --argjson sa "$success_age_json" \
    --arg e "$err_at" --arg m "$err_msg" --argjson f "$failure_count" \
    --argjson d "$failure_delta" --argjson pc "$pending_count" --argjson pa "$pending_age" \
    '{last_success_at: $s, last_success_age_seconds: $sa, last_error_at: $e, last_error: $m, failure_count: $f, failure_delta: ($d == 1), pending_count: $pc, oldest_pending_age_seconds: $pa}')
  emit_lane_json "$uobj"
}

check_delay_gate() {
  # Journald delay-gate trips (the fail-closed reconnect path) in the last 15m,
  # observed as warnings. Capture journalctl's own exit status separately: a
  # successful no-match query is trips=0, while a failed query means the
  # delay-gate evidence could not be inspected.
  unit=$1
  label=$2
  journal_out=$(journalctl -u "$unit" --since "$DELAY_GATE_WINDOW" --no-pager 2>/dev/null)
  journal_rc=$?
  trips=$(printf '%s\n' "$journal_out" \
    | grep -c 'source-to-receive delay exceeds the governed limit' || true)
  case "$trips" in (*[!0-9]*|'') trips=0 ;; esac
  if [ "$journal_rc" -ne 0 ]; then
    record_warning "$label: journald query failed (exit $journal_rc)"
  elif [ "$trips" -gt 0 ]; then
    record_warning "$label: $trips delay-gate trip(s) in last 15 minutes"
  fi
  dobj=$(jq -n --argjson t "$trips" '{trips_15m: $t}')
  delay_gate_json=$(jq -n --argjson base "$delay_gate_json" --arg k "$unit" --argjson v "$dobj" \
    '$base + {($k): $v}')
}

check_recent_snapshot_failures() {
  unit=$1
  label=$2
  journal_out=$(journalctl -u "$unit" --since "$FEE_FAILURE_WINDOW" --no-pager 2>/dev/null)
  journal_rc=$?
  failures=$(printf '%s\n' "$journal_out" | grep -c 'Failed with result' || true)
  case "$failures" in (*[!0-9]*|'') failures=0 ;; esac
  if [ "$journal_rc" -ne 0 ]; then
    record_warning "$label: snapshot failure journal query failed (exit $journal_rc)"
  elif [ "$failures" -gt 0 ]; then
    record_warning "$label: $failures recent snapshot failure(s)"
  fi
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
check_service "$REFERENCE_COLLECTOR" "binance-usdm-reference-collector"
check_service "$BYBIT_ARCHIVER" "bybit-options-archiver"

check_timer "$POLY_MARKET_UPLOAD_TIMER" "polymarket-market-tape-upload.timer"
check_oneshot_result "$POLY_MARKET_UPLOAD_SERVICE" "polymarket-market-tape-upload.service"
check_timer "$POLY_REF_UPLOAD_TIMER" "polymarket-reference-upload.timer"
check_oneshot_result "$POLY_REF_UPLOAD_SERVICE" "polymarket-reference-upload.service"
check_timer "$WATCHDOG_TIMER" "polymarket-market-tape-upload-watchdog.timer"
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
check_recent_snapshot_failures "$FEE_SPOT_SERVICE" "binance-fee-snapshot-spot.service"
check_recent_snapshot_failures "$FEE_USDM_SERVICE" "binance-fee-snapshot-usdm.service"

check_disabled_unit "$POLY_RAW_OPS_GATE" "polymarket-raw-ops-gate"

check_binance_health "binance-lob-archiver-production@spot" "$SPOOL_ROOT/binance-lob/spot"
check_binance_health "binance-lob-archiver-production@usdm" "$SPOOL_ROOT/binance-lob/usdm"

check_upload_lane "binance-lob-archiver-production@spot" "$SPOOL_ROOT/binance-lob/spot" \
  1 "$LOB_SUCCESS_MAX_AGE" "$LOB_PENDING_MAX" "$LOB_PENDING_MAX_AGE" manifests
check_upload_lane "binance-lob-archiver-production@usdm" "$SPOOL_ROOT/binance-lob/usdm" \
  1 "$LOB_SUCCESS_MAX_AGE" "$LOB_PENDING_MAX" "$LOB_PENDING_MAX_AGE" manifests
check_upload_lane "binance-usdm-reference-collector" "$SPOOL_ROOT/binance-usdm-reference" \
  0 "$REF_SUCCESS_MAX_AGE" "$REF_PENDING_MAX" "$REF_PENDING_MAX_AGE" lake
check_upload_lane "bybit-options-upload" "$SPOOL_ROOT/bybit-options" \
  0 "$BYBIT_SUCCESS_MAX_AGE" "$BYBIT_PENDING_MAX" "$BYBIT_PENDING_MAX_AGE" bybit-raw
check_upload_lane "polymarket-market-tape-upload" "$SPOOL_ROOT/polymarket" \
  0 "$POLY_SUCCESS_MAX_AGE" "$POLY_PENDING_MAX" "$POLY_PENDING_MAX_AGE" tapes
check_upload_lane "polymarket-reference-upload" "$SPOOL_ROOT/polymarket-reference" \
  0 "$POLY_SUCCESS_MAX_AGE" "$POLY_PENDING_MAX" "$POLY_PENDING_MAX_AGE" tapes
check_upload_lane "binance-fee-upload" "$SPOOL_ROOT/binance-fee" \
  1 "$FEE_SUCCESS_MAX_AGE" "$FEE_PENDING_MAX" "$FEE_PENDING_MAX_AGE" lake

check_delay_gate "$ARCHIVER_SPOT" "binance-lob-archiver-production@spot"
check_delay_gate "$ARCHIVER_USDM" "binance-lob-archiver-production@usdm"

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
    --argjson uploads "$uploads_json" --argjson delay "$delay_gate_json" \
    '{disk: $disk, mount: $mount, units: $units, health: $health, uploads: $uploads, delay_gate: $delay}')
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
