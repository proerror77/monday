#!/bin/sh
# Watchdog for the Polymarket market and reference tape upload pipelines
# (issue #655). Runs every two minutes from
# polymarket-market-tape-upload-watchdog.timer. It only ever starts units; it
# never stops, disables, or deletes anything and never modifies tape files.
set -eu

readonly TAG=polymarket-upload-watchdog
readonly ACTIVE_TAPE=market-updates.ndjson
readonly STALE_SECONDS=5400

readonly MARKET_SPOOL=/data/monday/spool/polymarket
readonly MARKET_TIMER=polymarket-market-tape-upload.timer
readonly MARKET_SERVICE=polymarket-market-tape-upload.service
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

# Self-heal one upload lane: $1 timer unit, $2 service unit, $3 spool dir,
# $4 pending rotated tapes, $5 oldest rotated tape age in seconds.
check_lane() {
  timer_state=$(unit_state "$1")
  if [ "$timer_state" != "active" ]; then
    log warning "$1 was '$timer_state'; starting it (see issue #655)"
    systemctl start "$1"
  fi
  service_state=$(unit_state "$2")
  case $service_state in
    active | activating | deactivating) ;;
    *)
      if [ "$4" -gt 0 ] && [ "$5" -gt "$STALE_SECONDS" ]; then
        log warning "$2 is '$service_state' with $4 rotated tape(s) pending in $3 and oldest age ${5}s > ${STALE_SECONDS}s; starting it with --no-block"
        systemctl start --no-block "$2"
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

log info "market_pending_rotated_tapes=$market_pending market_oldest_tape_age_seconds=$market_oldest reference_pending_rotated_tapes=$reference_pending reference_oldest_tape_age_seconds=$reference_oldest data_free_gb=$data_free_gb"

check_lane "$MARKET_TIMER" "$MARKET_SERVICE" "$MARKET_SPOOL" \
  "$market_pending" "$market_oldest"
check_lane "$REFERENCE_TIMER" "$REFERENCE_SERVICE" "$REFERENCE_SPOOL" \
  "$reference_pending" "$reference_oldest"
