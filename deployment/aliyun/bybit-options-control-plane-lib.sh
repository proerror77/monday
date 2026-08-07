#!/usr/bin/env bash

# Pure monotonic freshness transition used by the Bybit Options shadow gate,
# its cutover, and the test harness.  Output:
#   last_updated_ms last_advance_mono max_gap_seconds sample_increment
# Returns 1 when the timestamp regressed or stopped advancing past the allowed
# gap, and 2 on argument errors (fail closed).
bybit_options_observe_health_freshness() {
  [[ $# -eq 6 ]] || return 2
  local last_updated_ms=$1
  local last_advance_mono=$2
  local max_gap_seconds=$3
  local current_updated_ms=$4
  local current_mono=$5
  local allowed_gap_seconds=$6
  local gap_seconds sample_increment=0

  [[ $last_updated_ms =~ ^[0-9]+$ \
    && $last_advance_mono =~ ^[0-9]+$ \
    && $max_gap_seconds =~ ^[0-9]+$ \
    && $current_updated_ms =~ ^[0-9]+$ \
    && $current_mono =~ ^[0-9]+$ \
    && $allowed_gap_seconds =~ ^[1-9][0-9]*$ ]] || return 2
  ((current_updated_ms >= last_updated_ms)) || return 1
  ((current_mono >= last_advance_mono)) || return 1

  gap_seconds=$((current_mono - last_advance_mono))
  ((gap_seconds > max_gap_seconds)) && max_gap_seconds=$gap_seconds
  ((gap_seconds <= allowed_gap_seconds)) || return 1

  if ((current_updated_ms > last_updated_ms)); then
    last_updated_ms=$current_updated_ms
    last_advance_mono=$current_mono
    sample_increment=1
  fi
  printf '%s %s %s %s\n' \
    "$last_updated_ms" "$last_advance_mono" "$max_gap_seconds" "$sample_increment"
}
