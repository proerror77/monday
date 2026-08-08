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
# permanent journald-query breach).
DELAY_GATE_WINDOW='15 min ago'

# Governed units. Persistent services must be active + enabled + Result=success
# and are monitored for restart-rate deltas. Upload lanes are driven by timers
# (active + enabled) whose oneshot services must have a successful last Result.
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

read_prior() {
  # $1 = state key (e.g. "nrestarts|<unit>"); prints prior value or empty
  if [ -f "$STATE_FILE" ]; then
    grep "^$1=" "$STATE_FILE" 2>/dev/null | head -n1 | cut -d= -f2-
  fi
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
    record_breach "disk: cannot determine /data free space (df unavailable)"
  else
    disk_free_percent=$((disk_avail_kib * 100 / disk_total_kib))
    disk_free_gb=$((disk_avail_kib / 1048576))
    disk_critical=0
    disk_warning=0
    if [ "$disk_free_percent" -lt "$DISK_CRIT_PERCENT" ]; then
      disk_critical=1
      record_breach "disk: /data free ${disk_free_percent}% (${disk_free_gb}GiB) below critical ${DISK_CRIT_PERCENT}%"
    elif [ "$disk_free_percent" -lt "$DISK_WARN_PERCENT" ]; then
      disk_warning=1
      record_breach "disk: /data free ${disk_free_percent}% (${disk_free_gb}GiB) below warning ${DISK_WARN_PERCENT}%"
    fi
  fi
  disk_json=$(jq -n --argjson p "$disk_free_percent" --argjson g "$disk_free_gb" \
    --argjson w "$(bool_json "$disk_warning")" --argjson c "$(bool_json "$disk_critical")" \
    '{free_percent: $p, free_gb: $g, warning: $w, critical: $c}')
}

check_service() {
  # Persistent service: active AND enabled AND Result=success AND restart-rate delta.
  unit=$1
  label=$2
  active=$(unit_is_active "$unit")
  enabled=$(unit_is_enabled "$unit")
  result=$(unit_result "$unit")
  nrestarts=$(unit_nrestarts "$unit")
  case "$nrestarts" in (*[!0-9]*|'') nrestarts=0;; esac
  [ "$active" = "active" ] || record_breach "$label: not active (is-active='$active')"
  [ "$enabled" = "enabled" ] || record_breach "$label: not enabled (is-enabled='$enabled')"
  [ "$result" = "success" ] || record_breach "$label: last systemd Result='$result'"
  prior=$(read_prior "nrestarts|$unit")
  if [ "$DRY_RUN" -eq 0 ] && [ -n "$prior" ]; then
    case "$prior" in (*[!0-9]*|'') prior="" ;; esac
    if [ -n "$prior" ]; then
      delta=$((nrestarts - prior))
      if [ "$delta" -gt "$RESTART_MAX_DELTA" ]; then
        record_breach "$label: restart rate high (NRestarts $prior -> $nrestarts)"
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
  [ "$active" = "active" ] || record_breach "$label: timer not active (is-active='$active')"
  [ "$enabled" = "enabled" ] || record_breach "$label: timer not enabled (is-enabled='$enabled')"
  obj=$(jq -n --arg a "$active" --arg e "$enabled" \
    '{active: ($a == "active"), enabled: ($e == "enabled")}')
  units_json=$(jq -n --argjson base "$units_json" --arg k "$unit" --argjson v "$obj" \
    '$base + {($k): $v}')
}

check_oneshot_result() {
  # Oneshot upload/watchdog service: last Result must be success.
  unit=$1
  label=$2
  result=$(unit_result "$unit")
  if [ -n "$result" ] && [ "$result" != "success" ]; then
    record_breach "$label: last systemd Result='$result'"
  fi
  obj=$(jq -n --arg r "$result" '{result: $r}')
  units_json=$(jq -n --argjson base "$units_json" --arg k "$unit" --argjson v "$obj" \
    '$base + {($k): $v}')
}

check_disabled_unit() {
  # The incident fill source must remain DISABLED. Breach on any is-enabled
  # state that is not explicitly disabled/masked/not-found (enabled, static,
  # indirect, ... all mean the unit can be activated).
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
  # health.json at /data/monday/spool/binance-lob/<market>/: fresh + gap-free.
  label=$1
  spool_dir=$2
  health_file="$spool_dir/health.json"
  age=0
  gaps=0
  hwarn=false
  hstatus=unknown
  if [ ! -f "$health_file" ] || [ -L "$health_file" ]; then
    record_breach "$label: health.json missing or a symbolic link ($health_file)"
    age=999999
  elif ! updated_ns=$(jq -r '.updated_at_ns // 0' "$health_file" 2>/dev/null); then
    record_breach "$label: health.json unparseable ($health_file)"
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
      record_breach "$label: health.json stale (age ${age}s > ${HEALTH_SILENCE_SECONDS}s)"
    fi
    if [ "$gaps" -gt 0 ]; then
      record_breach "$label: sequence_gaps=$gaps"
    fi
  fi
  hobj=$(jq -n --argjson age "$age" --argjson gaps "$gaps" --arg hw "$hwarn" --arg s "$hstatus" \
    '{age_seconds: $age, sequence_gaps: $gaps, disk_warning: ($hw == "true"), status: $s}')
  health_json=$(jq -n --argjson base "$health_json" --arg k "$label" --argjson v "$hobj" \
    '$base + {($k): $v}')
}

check_upload() {
  # upload-status.json: breach on a present last_error_at/last_error, or on a
  # failure_count delta since the previous poll (failure_count is cumulative
  # and never reset, so a delta means an upload failed between polls).
  label=$1
  spool_dir=$2
  upload_file="$spool_dir/upload-status.json"
  err_at=null
  err_msg=null
  failure_count=0
  failure_delta=0
  if [ -f "$upload_file" ]; then
    err_at=$(jq -r '(.last_error_at // null)' "$upload_file" 2>/dev/null || printf 'null')
    err_msg=$(jq -r '(.last_error // null)' "$upload_file" 2>/dev/null || printf 'null')
    failure_count=$(jq -r '(.failure_count // 0)' "$upload_file" 2>/dev/null || printf '0')
  fi
  case "$failure_count" in (*[!0-9]*|'') failure_count=0 ;; esac
  if [ -n "$err_at" ] && [ "$err_at" != "null" ]; then
    record_breach "$label: upload last_error_at=$err_at"
  fi
  if [ -n "$err_msg" ] && [ "$err_msg" != "null" ]; then
    record_breach "$label: upload last_error=$err_msg"
  fi
  prior=$(read_prior "failure_count|$label")
  if [ "$DRY_RUN" -eq 0 ] && [ -n "$prior" ]; then
    case "$prior" in (*[!0-9]*|'') prior="" ;; esac
    if [ -n "$prior" ] && [ "$failure_count" -gt "$prior" ]; then
      failure_delta=1
      record_breach "$label: upload failure_count increased $prior -> $failure_count"
    fi
  fi
  state_lines="$state_lines failure_count|$label=$failure_count"
  uobj=$(jq -n --arg e "$err_at" --arg m "$err_msg" --argjson f "$failure_count" \
    --argjson d "$failure_delta" \
    '{last_error_at: $e, last_error: $m, failure_count: $f, failure_delta: ($d == 1)}')
  uploads_json=$(jq -n --argjson base "$uploads_json" --arg k "$label" --argjson v "$uobj" \
    '$base + {($k): $v}')
}

check_delay_gate() {
  # Journald delay-gate trips (the fail-closed reconnect path) in the last 15m.
  # Capture journalctl's own exit status separately: a successful no-match query
  # is trips=0, but a failed query means the delay-gate evidence could not be
  # inspected and must itself be reported as a breach so the monitor cannot
  # emit ok:true without inspectable delay-gate evidence.
  unit=$1
  label=$2
  journal_out=$(journalctl -u "$unit" --since "$DELAY_GATE_WINDOW" --no-pager 2>/dev/null)
  journal_rc=$?
  trips=$(printf '%s\n' "$journal_out" \
    | grep -c 'source-to-receive delay exceeds the governed limit' || true)
  case "$trips" in (*[!0-9]*|'') trips=0 ;; esac
  if [ "$journal_rc" -ne 0 ]; then
    record_breach "$label: journald query failed (exit $journal_rc)"
  elif [ "$trips" -gt 0 ]; then
    record_breach "$label: $trips delay-gate trip(s) in last 15 minutes"
  fi
  dobj=$(jq -n --argjson t "$trips" '{trips_15m: $t}')
  delay_gate_json=$(jq -n --argjson base "$delay_gate_json" --arg k "$unit" --argjson v "$dobj" \
    '$base + {($k): $v}')
}

write_state() {
  [ "$DRY_RUN" -eq 1 ] && return 0
  # A state-persistence failure means the next poll can lose restart/upload
  # deltas while the monitor reports healthy, so record it as a breach rather
  # than only logging.
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

check_disabled_unit "$POLY_RAW_OPS_GATE" "polymarket-raw-ops-gate"

check_binance_health "binance-lob-archiver-production@spot" "$SPOOL_ROOT/binance-lob/spot"
check_binance_health "binance-lob-archiver-production@usdm" "$SPOOL_ROOT/binance-lob/usdm"

check_upload "binance-lob-archiver-production@spot" "$SPOOL_ROOT/binance-lob/spot"
check_upload "binance-lob-archiver-production@usdm" "$SPOOL_ROOT/binance-lob/usdm"
check_upload "binance-usdm-reference-collector" "$SPOOL_ROOT/binance-usdm-reference"
check_upload "bybit-options-upload" "$SPOOL_ROOT/bybit-options"
check_upload "polymarket-market-tape-upload" "$SPOOL_ROOT/polymarket"
check_upload "polymarket-reference-upload" "$SPOOL_ROOT/polymarket-reference"

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
  checks_json=$(jq -n --argjson disk "$disk_json" --argjson mount "$mount_json" \
    --argjson units "$units_json" --argjson health "$health_json" \
    --argjson uploads "$uploads_json" --argjson delay "$delay_gate_json" \
    '{disk: $disk, mount: $mount, units: $units, health: $health, uploads: $uploads, delay_gate: $delay}')
  jq -n --argjson ok "$ok_str" --arg checked "$CHECKED_AT" \
    --argjson breaches "$breaches_json" --argjson checks "$checks_json" \
    '{ok: $ok, checked_at: $checked, breaches: $breaches, checks: $checks}'
else
  if [ "$breach_count" -gt 0 ]; then
    printf 'ok:false\n'
    printf '%s\n' "$breaches" | sed 's/^/breach: /'
  else
    printf 'ok:true\n'
  fi
fi

if [ "$breach_count" -gt 0 ]; then
  log err "$breach_count collector-health breach(es) detected"
  exit 1
fi
log info "all collector-health checks passed"
exit 0
