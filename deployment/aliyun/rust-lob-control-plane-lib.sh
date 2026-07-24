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

# Validate that replay-unsafe manifests are limited to the trailing incomplete
# portion of a gate observation. Safe segments remain the only input to strict
# readback; an unsafe segment followed by safe data is a fail-closed boundary.
monday_validate_replay_safe_manifest_order() {
  [[ $# -eq 3 ]] || return 2
  local market=$1
  local candidates=$2
  local unsafe_candidates=$3
  local unsafe_start unsafe_end unsafe_uri

  [[ -f $candidates && -f $unsafe_candidates ]] || return 2
  [[ -s $unsafe_candidates ]] || return 0

  while IFS=$'\t' read -r unsafe_start unsafe_end unsafe_uri; do
    if awk -F '\t' -v unsafe_start="$unsafe_start" -v unsafe_end="$unsafe_end" \
      '$1 < unsafe_end && unsafe_start < $2 { overlap=1 } END { exit(overlap ? 0 : 1) }' \
      "$candidates"; then
      printf '%s replay-unsafe manifest overlaps a replay-safe segment: %s\n' \
        "$market" "$unsafe_uri" >&2
      return 1
    fi
    if awk -F '\t' -v unsafe_start="$unsafe_start" \
      '$1 > unsafe_start { found=1 } END { exit(found ? 0 : 1) }' \
      "$candidates"; then
      printf '%s has a replay-unsafe manifest before a later replay-safe manifest: %s\n' \
        "$market" "$unsafe_uri" >&2
      return 1
    fi
  done < <(sort -n -k1,1 "$unsafe_candidates")
}
