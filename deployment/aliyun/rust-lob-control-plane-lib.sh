#!/usr/bin/env bash

# Pure monotonic freshness transition used by the host gate and its tests.
# Output: last_updated_ns last_advance_mono max_gap_seconds sample_increment
monday_observe_health_freshness() {
  [[ $# -eq 6 ]] || return 2
  local last_updated_ns=$1
  local last_advance_mono=$2
  local max_gap_seconds=$3
  local current_updated_ns=$4
  local current_mono=$5
  local allowed_gap_seconds=$6
  local gap_seconds sample_increment=0

  [[ $last_updated_ns =~ ^[0-9]+$ \
    && $last_advance_mono =~ ^[0-9]+$ \
    && $max_gap_seconds =~ ^[0-9]+$ \
    && $current_updated_ns =~ ^[0-9]+$ \
    && $current_mono =~ ^[0-9]+$ \
    && $allowed_gap_seconds =~ ^[1-9][0-9]*$ ]] || return 2
  ((current_updated_ns >= last_updated_ns)) || return 1
  ((current_mono >= last_advance_mono)) || return 1

  gap_seconds=$((current_mono - last_advance_mono))
  ((gap_seconds > max_gap_seconds)) && max_gap_seconds=$gap_seconds
  ((gap_seconds <= allowed_gap_seconds)) || return 1

  if ((current_updated_ns > last_updated_ns)); then
    last_updated_ns=$current_updated_ns
    last_advance_mono=$current_mono
    sample_increment=1
  fi
  printf '%s %s %s %s\n' \
    "$last_updated_ns" "$last_advance_mono" "$max_gap_seconds" "$sample_increment"
}
