#!/usr/bin/env bash

# Pure identity helpers shared by the five Control Plane V2 operations.
# Host scripts may add runtime checks, but no helper writes state.

monday_sha256_file() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]] || return 1
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | awk '{print $1}'
  else
    shasum -a 256 -- "$1" | awk '{print $1}'
  fi
}

monday_sha256_text() {
  [[ $# -eq 1 ]] || return 2
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$1" | sha256sum | awk '{print $1}'
  else
    printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
  fi
}

monday_sha256_ok() {
  [[ $# -eq 1 && $1 =~ ^[a-f0-9]{64}$ ]]
}

monday_path_direct() {
  [[ $# -eq 1 ]] || return 2
  local path=$1 resolved
  [[ -d $path && ! -L $path ]] || return 1
  resolved=$(readlink -f -- "$path") || return 1
  [[ $resolved == "$path" ]]
}

monday_file_direct() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]]
}

monday_runtime_assets() {
  printf '%s\n' \
    binance-lob-archiver-production@.service \
    binance-lob-archiver-rust@.service \
    binance-lob-archiver-upload@.service \
    binance-lob-archiver-rust-upload@.service \
    binance-lob-archiver-production-spot.env \
    binance-lob-archiver-production-usdm.env \
    binance-lob-archiver-rust-spot.env \
    binance-lob-archiver-rust-usdm.env
}

monday_controller_assets() {
  printf '%s\n' \
    binance-lob-archiver-recovery@.service \
    binance-lob-archiver-recovery@.timer \
    host-rust-lob-recovery-queue.sh \
    host-rust-lob-readback.sh \
    host-rust-lob-shadow-gate.sh \
    host-rust-lob-cutover.sh \
    host-rust-lob-restore.sh \
    host-rust-lob-controller-release.sh \
    monday-collector-health.sh \
    rust-lob-control-plane-lib.sh \
    rust-lob-runtime-health-policy.jq \
    rust-lob-shadow-gate-policy.jq
}

monday_rust_lob_runtime_contract_sha256() {
  [[ $# -eq 1 ]] || return 2
  local directory=${1%/} asset digest
  local -a assets=()
  mapfile -t assets < <(monday_runtime_assets)
  for asset in "${assets[@]}"; do
    monday_file_direct "$directory/$asset" || return 1
  done
  {
    for asset in "${assets[@]}"; do
      digest=$(monday_sha256_file "$directory/$asset") || return 1
      printf '%s  %s\n' "$digest" "$asset"
    done
  } | if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

monday_controller_release_sha256() {
  [[ $# -eq 1 ]] || return 2
  monday_sha256_file "$1/release.json"
}

# Resolve the immutable V2 deployment currently installed as the active pair.
# This helper is intentionally strict: it never guesses another release and
# never accepts a legacy manifest.
monday_rust_lob_active_controller_deployment() {
  [[ $# -eq 3 ]] || return 2
  local controller_root=$1 artifact_sha=$2 runtime_contract=$3
  local active="$controller_root/active" release sha manifest deployment
  [[ $artifact_sha =~ ^[a-f0-9]{64}$ && $runtime_contract =~ ^[a-f0-9]{64}$ ]] || return 1
  [[ -L $active ]] || return 1
  release=$(readlink -f -- "$active") || return 1
  [[ $release =~ ^${controller_root}/([a-f0-9]{64})$ ]] || return 1
  sha=${BASH_REMATCH[1]}
  manifest="$release/release.json"
  deployment="$release/deployment"
  [[ -d $release && ! -L $release && -d $deployment && ! -L $deployment ]] || return 1
  monday_verify_controller_release "${controller_root%/}/../../.." "$sha" 2>/dev/null || return 1
  jq -e --arg artifact "$artifact_sha" --arg runtime "$runtime_contract" \
    '.artifact_sha256 == $artifact and .runtime_contract_sha256 == $runtime' \
    "$manifest" >/dev/null || return 1
  printf '%s\n' "$deployment"
}

# Pure monotonic health freshness transition used by the host gate and tests.
# Output: last_updated_ns last_advance_mono max_gap_seconds sample_increment
monday_observe_health_freshness() {
  [[ $# -eq 6 ]] || return 2
  local last_updated_ns=$1 last_advance_mono=$2 max_gap_seconds=$3
  local current_updated_ns=$4 current_mono=$5 allowed_gap_seconds=$6
  local gap_seconds sample_increment=0
  [[ $last_updated_ns =~ ^[0-9]+$ && $last_advance_mono =~ ^[0-9]+$ \
    && $max_gap_seconds =~ ^[0-9]+$ && $current_updated_ns =~ ^[0-9]+$ \
    && $current_mono =~ ^[0-9]+$ && $allowed_gap_seconds =~ ^[1-9][0-9]*$ ]] || return 2
  ((current_updated_ns >= last_updated_ns && current_mono >= last_advance_mono)) || return 1
  gap_seconds=$((current_mono - last_advance_mono))
  ((gap_seconds > max_gap_seconds)) && max_gap_seconds=$gap_seconds
  ((gap_seconds <= allowed_gap_seconds)) || return 1
  if ((current_updated_ns > last_updated_ns)); then
    last_updated_ns=$current_updated_ns
    last_advance_mono=$current_mono
    sample_increment=1
  fi
  printf '%s %s %s %s\n' "$last_updated_ns" "$last_advance_mono" \
    "$max_gap_seconds" "$sample_increment"
}

# Return the required bytes and accept only when the host has that headroom.
monday_shadow_memory_admission() {
  (($# >= 3)) || return 2
  local available=$1 total=0 value
  for value in "$@"; do
    [[ $value =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    (( value <= 9223372036854775807 )) || return 2
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

# Reserve measured production peak plus bounded growth, capped by unit limit.
monday_production_memory_growth_headroom() {
  [[ $# -eq 4 ]] || return 2
  local current=$1 peak=$2 maximum=$3 margin=$4 target
  local value
  for value in "$current" "$peak" "$maximum" "$margin"; do
    [[ $value =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    (( value <= 9223372036854775807 )) || return 2
  done
  ((current <= peak && peak <= maximum)) || return 2
  if ((margin >= maximum - peak)); then target=$maximum; else target=$((peak + margin)); fi
  printf '%s\n' "$((target - current))"
}

# Read cumulative I/O-full stall time from a Linux PSI source.
monday_io_full_psi_total_us() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]] || return 2
  awk '$1 == "full" { rows += 1; for (i = 2; i <= NF; i++) {
    if ($i ~ /^total=[0-9]+$/) { totals += 1; value = substr($i, 7) }
  }} END { if (rows != 1 || totals != 1 || value !~ /^(0|[1-9][0-9]*)$/) exit 1; print value }' "$1"
}

# Output: delta_us ratio hit consecutive_hits. Threshold is normalized to the
# reference window and any non-hit resets the consecutive count.
monday_io_full_psi_window() {
  [[ $# -eq 6 ]] || return 2
  local previous=$1 current=$2 window_us=$3 reference_window_us=$4
  local threshold_us=$5 consecutive=$6 value delta hit next ratio
  for value in "$previous" "$current" "$window_us" "$reference_window_us" "$threshold_us" "$consecutive"; do
    [[ $value =~ ^(0|[1-9][0-9]{0,18})$ ]] || return 2
    (( value <= 9223372036854775807 )) || return 2
  done
  ((current >= previous && window_us > 0 && reference_window_us > 0 && threshold_us > 0)) || return 2
  delta=$((current - previous))
  if awk -v delta="$delta" -v window="$window_us" -v threshold="$threshold_us" \
    -v reference="$reference_window_us" 'BEGIN { exit !((delta / window) >= (threshold / reference)) }'; then
    hit=true; next=$((consecutive + 1))
  else hit=false; next=0; fi
  ratio=$(awk -v delta="$delta" -v window="$window_us" 'BEGIN { printf "%.9f", delta / window }')
  printf '%s %s %s %s\n' "$delta" "$ratio" "$hit" "$next"
}

# Replay-unsafe manifests may only trail the safe observation window.
monday_validate_replay_safe_manifest_order() {
  [[ $# -eq 3 ]] || return 2
  local market=$1 candidates=$2 unsafe_candidates=$3
  local unsafe_start unsafe_end unsafe_uri
  [[ -f $candidates && -f $unsafe_candidates ]] || return 2
  [[ -s $unsafe_candidates ]] || return 0
  while IFS=$'\t' read -r unsafe_start unsafe_end unsafe_uri; do
    if awk -F '\t' -v start="$unsafe_start" -v end="$unsafe_end" \
      '$1 < end && start < $2 { overlap=1 } END { exit(overlap ? 0 : 1) }' "$candidates"; then
      printf '%s replay-unsafe manifest overlaps a replay-safe segment: %s\n' "$market" "$unsafe_uri" >&2
      return 1
    fi
    if awk -F '\t' -v start="$unsafe_start" '$1 > start { found=1 } END { exit(found ? 0 : 1) }' "$candidates"; then
      printf '%s has a replay-unsafe manifest before a later replay-safe manifest: %s\n' "$market" "$unsafe_uri" >&2
      return 1
    fi
  done < <(sort -n -k1,1 "$unsafe_candidates")
}

monday_validate_v2_manifest() {
  [[ $# -eq 1 && -f $1 && ! -L $1 ]] || return 2
  jq -e '
    type == "object"
    and (keys | sort) == [
      "artifact_sha256", "artifact_uri", "control_plane_version",
      "deployment_bundle_sha256", "deployment_bundle_uri",
      "deployment_source_revision", "runtime_contract_sha256", "schema",
      "topology"
    ]
    and .schema == "monday.rust_lob_controller_release.v2"
    and .control_plane_version == 2
    and (.artifact_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.deployment_bundle_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
    and (.artifact_uri | type == "string" and test("^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$"))
    and (.deployment_bundle_uri | type == "string" and test("^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$"))
    and (.topology | . == "stable")' \
    "$1" >/dev/null
}

monday_manifest_field() {
  [[ $# -eq 2 ]] || return 2
  jq -er --arg key "$2" '.[$key]' "$1"
}

monday_active_controller_sha() {
  [[ $# -eq 1 ]] || return 2
  local root=${1%/} link
  link="$root/opt/monday/releases/binance-lob-controller/active"
  local target sha
  [[ -L $link ]] || return 1
  target=$(readlink -f -- "$link") || return 1
  sha=${target##*/}
  [[ $target == "$root/opt/monday/releases/binance-lob-controller/$sha" \
    && $sha =~ ^[a-f0-9]{64}$ ]] || return 1
  printf '%s\n' "$sha"
}

monday_verify_controller_release() {
  [[ $# -eq 2 ]] || return 2
  local root=${1%/} sha=$2
  local release="$root/opt/monday/releases/binance-lob-controller/$sha"
  local manifest="$release/release.json" asset expected projection target payload
  monday_sha256_ok "$sha" || return 1
  monday_path_direct "$root/opt/monday/releases/binance-lob-controller" || return 1
  monday_path_direct "$release" || return 1
  monday_path_direct "$release/deployment" || return 1
  monday_file_direct "$manifest" || return 1
  monday_file_direct "$release/release.json.sha256" || return 1
  monday_file_direct "$release/deployment.sha256" || return 1
  [[ $(monday_sha256_file "$manifest") == "$sha" ]] || return 1
  (cd "$release" && sha256sum --check --strict release.json.sha256 >/dev/null \
    && sha256sum --check --strict deployment.sha256 >/dev/null) || return 1
  monday_validate_v2_manifest "$manifest" || return 1
  expected=$(cd "$release" && sha256sum deployment/* | sort -k2)
  cmp -s <(printf '%s\n' "$expected") "$release/deployment.sha256" || return 1
  while IFS= read -r asset; do
    [[ -n $asset ]] || continue
    monday_file_direct "$release/deployment/$asset" || return 1
  done < <(monday_controller_assets)
  payload=$(monday_manifest_field "$manifest" artifact_sha256) || return 1
  projection="$release/binance-lob-archiver"
  target="$root/opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver"
  [[ -L $projection && $(readlink -- "$projection") == "$target" ]] || return 1
  [[ $(readlink -f -- "$projection") == "$target" ]] || return 1
  monday_file_direct "$target" || return 1
  [[ $(monday_sha256_file "$target") == "$payload" ]] || return 1
  [[ $(monday_rust_lob_runtime_contract_sha256 "$release/deployment") \
    == "$(monday_manifest_field "$manifest" runtime_contract_sha256)" ]] || return 1
}

monday_validate_v2_gate() {
  [[ $# -eq 4 ]] || return 2
  local gate=$1 from=$2 candidate=$3 gate_sha=$4
  monday_file_direct "$gate" || return 1
  [[ $(monday_sha256_file "$gate") == "$gate_sha" ]] || return 1
  jq -e \
    --arg from "$from" --arg candidate "$candidate" --arg gate_sha "$gate_sha" '
      .schema == "monday.rust_lob_shadow_gate.v5"
      and .control_plane_version == 2
      and .passed == true
      and (.production_eligible | type == "boolean")
      and (.test_only | type == "boolean")
      and (if .test_only then .production_eligible == false else .production_eligible == true end)
      and .transition.before == $from
      and .transition.after == $candidate
      and (.transition.topology == "stable" or .transition.topology == "direct-bootstrap")
      and (.candidate_controller_sha256 == $candidate)
      and (.candidate_payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.candidate_runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.candidate_deployment_bundle_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.candidate_deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
      and (.before | type == "object")
      and (.production_assets | type == "object" and length == 4)
      and (.production_process | type == "object")
      and (.shadow_staging | type == "object"
        and (.candidate_assets | type == "object" and length == 4)
        and (.restored_assets | type == "object" and length == 4)
        and (.before_assets | type == "object" and length == 4)
        and (.binary | type == "object" and (.candidate_target | type == "string")
          and (.restored_present | type == "boolean")))
      and (.checks | type == "object"
        and .before_pair_unchanged == true
        and .shadow_staging_verified == true
        and .shadow_assets_restored == true
        and .resource_preflight == true
        and .oss_triplets == true
        and .strict_segment_verifier == true
        and .final_identity == true)
      and (.markets | type == "object" and ((keys | sort) == ["spot", "usdm"])
        and all(.[].segment_count; . >= 2)
        and all(.[].oss_triplet_count; . >= 2)
        and all(.[].n_restarts; . == 0)
        and all(.[].process_identity_verified; . == true)
        and all(.[].installed_shadow_assets_verified; . == true)
        and all(.[].strict_lob_continuity_readback; . == true))' \
    "$gate" >/dev/null
}

monday_atomic_symlink() {
  [[ $# -eq 2 ]] || return 2
  local target=$1 link=$2 temporary resolved
  resolved=$(readlink -f -- "$target") || return 1
  temporary="$link.new.$$"
  rm -f -- "$temporary"
  ln -s "$target" "$temporary"
  if [[ $(uname -s) == Darwin ]]; then
    # macOS mv follows a directory symlink; remove the link while the
    # operation lock is held, then rename the staged link into place.
    rm -f -- "$link"
    mv -f -- "$temporary" "$link"
  else
    mv -Tf -- "$temporary" "$link"
  fi
  [[ -L $link && $(readlink -f -- "$link") == "$resolved" ]]
}
