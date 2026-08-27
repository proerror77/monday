#!/usr/bin/env bash

# Hash only the bytes exercised by the Shadow Gate and later installed as the
# production collector runtime. Transition, recovery, and readback controllers
# remain covered by the full deployment bundle identity.
monday_rust_lob_runtime_contract_sha256() {
  [[ $# -eq 1 ]] || return 2
  local directory=$1 asset digest
  local -a assets=(
    binance-lob-archiver-production@.service
    binance-lob-archiver-rust@.service
    binance-lob-archiver-upload@.service
    binance-lob-archiver-rust-upload@.service
    binance-lob-archiver-production-spot.env
    binance-lob-archiver-production-usdm.env
    binance-lob-archiver-rust-spot.env
    binance-lob-archiver-rust-usdm.env
  )

  for asset in "${assets[@]}"; do
    [[ -f $directory/$asset && ! -L $directory/$asset ]] || return 1
  done
  {
    for asset in "${assets[@]}"; do
      if command -v sha256sum >/dev/null 2>&1; then
        digest=$(sha256sum "$directory/$asset" | awk '{print $1}')
      else
        digest=$(shasum -a 256 "$directory/$asset" | awk '{print $1}')
      fi
      printf '%s  %s\n' "$digest" "$asset"
    done
  } | if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

# Resolve the immutable controller deployment currently applied to one artifact.
monday_rust_lob_active_controller_deployment() {
  [[ $# -eq 3 ]] || return 2
  local controller_root=$1 artifact_sha=$2 runtime_contract=$3
  local active="$controller_root/active" release manifest manifest_sha deployment

  [[ $artifact_sha =~ ^[a-f0-9]{64}$ \
    && $runtime_contract =~ ^[a-f0-9]{64}$ \
    && -L $active ]] || return 1
  release=$(readlink -f -- "$active") || return 1
  [[ $release =~ ^${controller_root}/([a-f0-9]{64})$ \
    && -d $release && ! -L $release ]] || return 1
  manifest_sha=${BASH_REMATCH[1]}
  manifest="$release/release.json"
  deployment="$release/deployment"
  [[ -f $manifest && ! -L $manifest \
    && -d $deployment && ! -L $deployment \
    && -f $release/release.json.sha256 && ! -L $release/release.json.sha256 \
    && -f $release/deployment.sha256 && ! -L $release/deployment.sha256 ]] \
    || return 1
  [[ $(sha256sum "$manifest" | awk '{print $1}') == "$manifest_sha" ]] || return 1
  (cd "$release" \
    && sha256sum --check --strict release.json.sha256 >/dev/null \
    && sha256sum --check --strict deployment.sha256 >/dev/null) || return 1
  jq -e \
    --arg artifact "$artifact_sha" \
    --arg runtime "$runtime_contract" '
      .schema == "monday.rust_lob_controller_release.v1"
      and .artifact_sha256 == $artifact
      and .runtime_contract_sha256 == $runtime' \
    "$manifest" >/dev/null || return 1
  printf '%s\n' "$deployment"
}

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

# Print the required bytes and accept only when the host has that headroom.
monday_shadow_memory_admission() {
  (($# >= 3)) || return 2
  local input available=$1 total=0 value

  for input in "$@"; do
    [[ $input =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    # Intentional lexical comparison: arithmetic would overflow on invalid input.
    # shellcheck disable=SC2071
    (( ${#input} < 19 )) || [[ $input < 9223372036854775808 ]] || return 2
  done
  shift

  for value in "$@"; do
    ((value <= 9223372036854775807 - total)) || return 2
    total=$((total + value))
  done
  ((total > 0)) || return 2
  printf '%s\n' "$total"
  ((available >= total))
}

# Reserve a measured production peak plus a bounded growth margin, capped by
# the unit's hard limit. The live host-reserve check covers growth beyond it.
monday_production_memory_growth_headroom() {
  [[ $# -eq 4 ]] || return 2
  local current=$1 peak=$2 maximum=$3 margin=$4 target

  for value in "$current" "$peak" "$maximum" "$margin"; do
    [[ $value =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    # shellcheck disable=SC2071
    (( ${#value} < 19 )) || [[ $value < 9223372036854775808 ]] || return 2
  done
  ((current <= peak && peak <= maximum)) || return 2
  if ((margin >= maximum - peak)); then
    target=$maximum
  else
    target=$((peak + margin))
  fi
  printf '%s\n' "$((target - current))"
}

# Read the cumulative I/O-full stall time from a Linux PSI source.
monday_io_full_psi_total_us() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]] || return 2
  awk '
    $1 == "full" {
      rows += 1
      for (i = 2; i <= NF; i++) {
        if ($i ~ /^total=[0-9]+$/) {
          totals += 1
          value = substr($i, 7)
        }
      }
    }
    END {
      if (rows != 1 || totals != 1 || value !~ /^(0|[1-9][0-9]*)$/) exit 1
      print value
    }
  ' "$1"
}

# Output: delta_us ratio hit consecutive_hits. The threshold is normalized to
# its reference window so scheduler and validation time cannot inflate a hit.
# Any non-hit resets the consecutive counter.
monday_io_full_psi_window() {
  [[ $# -eq 6 ]] || return 2
  local previous=$1 current=$2 window_us=$3 reference_window_us=$4
  local threshold_us=$5 consecutive=$6
  local value delta hit next ratio

  for value in "$previous" "$current" "$window_us" "$reference_window_us" \
    "$threshold_us" "$consecutive"; do
    [[ $value =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    # shellcheck disable=SC2071
    (( ${#value} < 19 )) || [[ $value < 9223372036854775808 ]] || return 2
  done
  ((current >= previous && window_us > 0 && reference_window_us > 0 \
    && threshold_us > 0)) || return 2
  delta=$((current - previous))
  if awk -v delta="$delta" -v window="$window_us" \
    -v threshold="$threshold_us" -v reference="$reference_window_us" \
    'BEGIN { exit !((delta / window) >= (threshold / reference)) }'; then
    hit=true
    next=$((consecutive + 1))
  else
    hit=false
    next=0
  fi
  ratio=$(awk -v delta="$delta" -v window="$window_us" \
    'BEGIN { printf "%.9f", delta / window }')
  printf '%s %s %s %s\n' "$delta" "$ratio" "$hit" "$next"
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
