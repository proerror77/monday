#!/usr/bin/env bash

# Missing status represents a new spool; malformed or indirect status never
# resets the cumulative counter to zero.
bybit_options_upload_failure_count() {
  local status=$1
  if [[ ! -e $status && ! -L $status ]]; then
    printf '0\n'
    return
  fi
  [[ -f $status && ! -L $status ]] || return 1
  jq -er '.failure_count | select(type == "number" and . >= 0 and . == floor)' "$status"
}

# The guarded interval must neither introduce failures nor erase history.
# A historical error can be retired only by the candidate drain itself.
bybit_options_upload_status_ready() {
  local status=$1 baseline=$2 count
  [[ $baseline =~ ^[0-9]+$ ]] || return 1
  count=$(bybit_options_upload_failure_count "$status") || return 1
  [[ $count == "$baseline" ]] || return 1
  [[ -e $status ]] || return 0
  jq -e 'has("last_error_at") and has("last_error")
    and .last_error_at == null and .last_error == null' "$status" >/dev/null
}

# Validate a complete rendered collector ExecStart, including immutable digest,
# and reject alternate/duplicate commands in unit fragments or drop-ins.
bybit_options_unit_exec_start_matches() {
  local binary=$1 unit_text=$2 exec_lines
  [[ $binary =~ ^/opt/monday/releases/bybit-options-archiver/[a-f0-9]{64}/bybit-options-archiver$ ]] \
    || return 1
  exec_lines=$(printf '%s\n' "$unit_text" | awk '
    /^[[:space:]]*ExecStart[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*/, "")
      sub(/[[:space:]]*$/, "")
      print
    }')
  [[ $exec_lines == "$binary" ]]
}

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
