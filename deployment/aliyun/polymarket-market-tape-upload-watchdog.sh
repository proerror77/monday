#!/bin/sh
# Watchdog for the Polymarket market and reference tape upload pipelines
# (issue #655). Runs every two minutes from
# polymarket-market-tape-upload-watchdog.timer. It only ever starts units; it
# never stops, disables, or deletes anything and never modifies tape files.
#
# Cutover suppression: when $SUPPRESS_FILE exists, all remediation is skipped
# for both lanes so a governed cutover can stop the upload timers without the
# watchdog restarting them mid-cutover. The file lives under /run and does not
# survive a reboot.
set -eu

readonly TAG=polymarket-upload-watchdog
readonly ACTIVE_TAPE=market-updates.ndjson
readonly STALE_SECONDS=5400
readonly SUPPRESS_FILE=/run/monday/polymarket-upload-watchdog.suppress

readonly MARKET_LANE=market
readonly MARKET_SPOOL=/data/monday/spool/polymarket
readonly MARKET_TIMER=polymarket-market-tape-upload.timer
readonly MARKET_SERVICE=polymarket-market-tape-upload.service
readonly REFERENCE_LANE=reference
readonly REFERENCE_SPOOL=/data/monday/spool/polymarket-reference
readonly REFERENCE_TIMER=polymarket-reference-upload.timer
readonly REFERENCE_SERVICE=polymarket-reference-upload.service

log() {
  logger -t "$TAG" -p "daemon.$1" -- "$2"
}

unit_state() {
  # systemctl is-active exits non-zero for every non-active state; the state
  # name on stdout is the value we need, so never propagate the exit status.
  systemctl is-active "$1" 2>/dev/null || true
}

unit_substate() {
  systemctl show -p SubState --value "$1" 2>/dev/null || true
}

unit_timer_next() {
  systemctl show -p NextElapseUSecMonotonic --value "$1" 2>/dev/null || true
}

# Sets pending and oldest_age for the rotated tapes in spool dir $1. Both
# lanes rotate 'market-updates.<timestamp>.ndjson' next to the active
# 'market-updates.ndjson', which never matches the rotation glob but is
# excluded explicitly regardless.
lane_stats() {
  pending=0
  oldest_age=0
  [ -d "$1" ] || return 0
  for tape in "$1"/market-updates.*.ndjson; do
    [ -f "$tape" ] || continue
    [ "${tape##*/}" != "$ACTIVE_TAPE" ] || continue
    mtime=$(stat -c %Y -- "$tape") || continue
    age=$((now - mtime))
    [ "$age" -ge 0 ] || age=0
    pending=$((pending + 1))
    [ "$age" -le "$oldest_age" ] || oldest_age=$age
  done
}

# start_unit LANE UNIT [start arguments...]: starts a unit without letting a
# failure abort the run; logs an ERROR naming the lane and records the failure
# so the script exits nonzero after every lane has been checked.
start_unit() {
  lane=$1
  unit=$2
  shift 2
  if ! systemctl start "$@" "$unit"; then
    log err "lane $lane: failed to start $unit"
    remediation_failed=1
  fi
}

restart_unit() {
  lane=$1
  unit=$2
  shift 2
  if ! systemctl restart "$@" "$unit"; then
    log err "lane $lane: failed to restart $unit"
    remediation_failed=1
  fi
}

# Self-heal one upload lane: $1 lane label, $2 timer unit, $3 service unit,
# $4 spool dir, $5 pending rotated tapes, $6 oldest rotated tape age in
# seconds.
check_lane() {
  timer_enablement=$(systemctl is-enabled "$2" 2>/dev/null || true)
  case $timer_enablement in
    enabled | enabled-runtime) ;;
    disabled | masked | masked-runtime)
      log info "$2 is '$timer_enablement'; respecting administrative containment for lane $1"
      return 0
      ;;
    *)
      log err "lane $1: cannot verify enablement of $2 ('$timer_enablement')"
      remediation_failed=1
      return 0
      ;;
  esac
  service_state=$(unit_state "$3")
  timer_state=$(unit_state "$2")
  if [ "$timer_state" != "active" ]; then
    log warning "$2 was '$timer_state'; starting it (see issue #655)"
    start_unit "$1" "$2"
  else
    timer_substate=$(unit_substate "$2")
    timer_next=$(unit_timer_next "$2")
    case $timer_substate in
      running) ;;
      waiting)
        if [ -z "$timer_next" ] || [ "$timer_next" = "n/a" ] \
          || [ "$timer_next" = "infinity" ]; then
          case $service_state in
            active | activating | deactivating) ;;
            *)
              log warning "$2 waiting but NextElapseUSecMonotonic='$timer_next' while $3 is '$service_state'; restarting it to rearm the schedule"
              restart_unit "$1" "$2"
              ;;
          esac
        fi
        ;;
      *)
        log warning "$2 active but SubState='$timer_substate'; restarting it to rearm the schedule"
        restart_unit "$1" "$2"
        ;;
    esac
  fi
  case $service_state in
    active | activating | deactivating) ;;
    *)
      if [ "$5" -gt 0 ] && [ "$6" -gt "$STALE_SECONDS" ]; then
        log warning "$3 is '$service_state' with $5 rotated tape(s) pending in $4 and oldest age ${6}s > ${STALE_SECONDS}s; starting it with --no-block"
        start_unit "$1" "$3" --no-block
      fi
      ;;
  esac
}

now=$(date +%s)
lane_stats "$MARKET_SPOOL"
market_pending=$pending
market_oldest=$oldest_age
lane_stats "$REFERENCE_SPOOL"
reference_pending=$pending
reference_oldest=$oldest_age
data_free_gb=$(df -kP /data | awk 'NR==2 {printf "%d", $4 / 1048576}')

stats="market_pending_rotated_tapes=$market_pending market_oldest_tape_age_seconds=$market_oldest reference_pending_rotated_tapes=$reference_pending reference_oldest_tape_age_seconds=$reference_oldest data_free_gb=$data_free_gb"

if [ -e "$SUPPRESS_FILE" ]; then
  log info "suppressed: $SUPPRESS_FILE present; skipping all remediation ($stats)"
  exit 0
fi
log info "$stats"

remediation_failed=0
check_lane "$MARKET_LANE" "$MARKET_TIMER" "$MARKET_SERVICE" "$MARKET_SPOOL" \
  "$market_pending" "$market_oldest"
check_lane "$REFERENCE_LANE" "$REFERENCE_TIMER" "$REFERENCE_SERVICE" \
  "$REFERENCE_SPOOL" "$reference_pending" "$reference_oldest"

if [ "$remediation_failed" -ne 0 ]; then
  log err "one or more lanes failed remediation; see earlier ERROR lines"
  exit 1
fi
