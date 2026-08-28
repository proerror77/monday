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

monday_root_join() {
  [[ $# -eq 2 ]] || return 2
  local root=${1:-/} suffix=${2#/}
  root=${root%/}
  [[ -n $root ]] || root=/
  if [[ $root == / ]]; then
    printf '/%s\n' "$suffix"
  else
    printf '%s/%s\n' "$root" "$suffix"
  fi
}

monday_iso_epoch() {
  [[ $# -eq 1 ]] || return 2
  local value=$1 normalized tz
  [[ $value =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})$ ]] || return 1
  if date -u -d "$value" +%s >/dev/null 2>&1; then
    date -u -d "$value" +%s
    return
  fi
  [[ $value =~ ^([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})(\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})$ ]] || return 1
  normalized=${BASH_REMATCH[1]}
  tz=${BASH_REMATCH[3]}
  [[ $tz == Z ]] && tz=+0000 || tz=${tz/:/}
  normalized+="$tz"
  date -u -j -f '%Y-%m-%dT%H:%M:%S%z' "$normalized" +%s
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

monday_runtime_asset_target() {
  [[ $# -eq 2 ]] || return 2
  local root=$1 asset=$2
  case "$asset" in
    binance-lob-archiver-production@.service|binance-lob-archiver-rust@.service|\
    binance-lob-archiver-upload@.service|binance-lob-archiver-rust-upload@.service)
      monday_root_join "$root" "etc/systemd/system/$asset" ;;
    binance-lob-archiver-production-spot.env|binance-lob-archiver-production-usdm.env|\
    binance-lob-archiver-rust-spot.env|binance-lob-archiver-rust-usdm.env)
      monday_root_join "$root" "etc/monday/$asset" ;;
    *) return 1 ;;
  esac
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

monday_rust_lob_live_runtime_contract_sha256() {
  [[ $# -eq 1 ]] || return 2
  local root=$1 scratch asset target resolved digest
  scratch=$(mktemp -d) || return 1
  while IFS= read -r asset; do
    target=$(monday_runtime_asset_target "$root" "$asset") || {
      rm -rf -- "$scratch"
      return 1
    }
    [[ -f $target ]] || {
      rm -rf -- "$scratch"
      return 1
    }
    resolved=$(readlink -f -- "$target") || {
      rm -rf -- "$scratch"
      return 1
    }
    [[ -f $resolved && ! -L $resolved ]] || {
      rm -rf -- "$scratch"
      return 1
    }
    cp -p -- "$resolved" "$scratch/$asset" || {
      rm -rf -- "$scratch"
      return 1
    }
  done < <(monday_runtime_assets)
  digest=$(monday_rust_lob_runtime_contract_sha256 "$scratch") || {
    rm -rf -- "$scratch"
    return 1
  }
  rm -rf -- "$scratch"
  printf '%s\n' "$digest"
}

monday_controller_release_sha256() {
  [[ $# -eq 1 ]] || return 2
  monday_sha256_file "$1/release.json"
}

# Resolve the immutable V2 deployment currently installed as the active pair.
# This helper is intentionally strict: it never guesses another release and
# never accepts an unsupported manifest.
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
  local root
  root=$(cd -- "${controller_root%/}/../../../.." 2>/dev/null && pwd -P) || return 1
  monday_verify_controller_release "$root" "$sha" 2>/dev/null || return 1
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
  local root=${1:-/} link controller_root
  controller_root=$(monday_root_join "$root" opt/monday/releases/binance-lob-controller) || return 1
  link="$controller_root/active"
  local target sha
  [[ -L $link ]] || return 1
  target=$(readlink -f -- "$link") || return 1
  sha=${target##*/}
  [[ $target == "$controller_root/$sha" \
    && $sha =~ ^[a-f0-9]{64}$ ]] || return 1
  printf '%s\n' "$sha"
}

monday_verify_controller_release() {
  [[ $# -eq 2 ]] || return 2
  local root=${1:-/} sha=$2 controller_root release
  controller_root=$(monday_root_join "$root" opt/monday/releases/binance-lob-controller) || return 1
  release="$controller_root/$sha"
  local manifest="$release/release.json" asset expected projection target payload
  monday_sha256_ok "$sha" || return 1
  monday_path_direct "$controller_root" || return 1
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
  target=$(monday_root_join "$root" "opt/monday/releases/binance-lob-archiver/$payload/binance-lob-archiver")
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
  local controller_asset_keys production_asset_keys shadow_asset_keys
  controller_asset_keys=$(monday_controller_assets | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  production_asset_keys=$(printf '%s\n' \
    binance-lob-archiver-production@.service \
    binance-lob-archiver-upload@.service \
    binance-lob-archiver-production-spot.env \
    binance-lob-archiver-production-usdm.env \
    | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  shadow_asset_keys=$(printf '%s\n' \
    binance-lob-archiver-rust@.service \
    binance-lob-archiver-rust-upload@.service \
    binance-lob-archiver-rust-spot.env \
    binance-lob-archiver-rust-usdm.env \
    | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  monday_file_direct "$gate" || return 1
  [[ $(monday_sha256_file "$gate") == "$gate_sha" ]] || return 1
  jq -e \
    --arg from "$from" --arg candidate "$candidate" --arg gate_sha "$gate_sha" \
    --argjson controller_asset_keys "$controller_asset_keys" \
    --argjson production_asset_keys "$production_asset_keys" \
    --argjson shadow_asset_keys "$shadow_asset_keys" '
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
      and (.candidate_control_bytes | type == "object"
        and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.assets | type == "object" and (keys | sort) == $controller_asset_keys
          and all(.[]; type == "string" and test("^[a-f0-9]{64}$"))))
      and (.before | type == "object"
        and .controller == $from
        and (.payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.production_projection | type == "string" and length > 0)
        and (.production_assets | type == "object" and (keys | sort) == $production_asset_keys
          and all(.[]; type == "string" and test("^[a-f0-9]{64}$"))))
      and (.production_assets | type == "object" and (keys | sort) == $production_asset_keys
        and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
      and (.production_process | type == "object")
      and (if .test_only then true else
        (.production_process | ((keys | sort) == ["spot", "usdm"]
          and all(.[]; .active == true
            and (.main_pid | type == "number" and . >= 1)
            and (.process_exe_sha256 | type == "string" and test("^[a-f0-9]{64}$"))))) end)
      and (.resource_admission | type == "array" and length >= 3
        and ((["preflight","shadow-spot","strict-verifier-spot","upload-drain-spot","shadow-usdm","strict-verifier-usdm","upload-drain-usdm","oss-readback-spot","oss-readback-usdm"]
          - (map(.phase) | unique)) | length == 0)
        and all(.[]; . as $r
          | (.phase | type == "string" and length > 0)
          and (.started_at | type == "string" and length > 0)
          and (.ended_at | type == "string" and length > 0)
          and (.samples | type == "number" and . >= 1)
          and (.host_memory_available_bytes | type == "number" and . >= 0)
          and (.max_memory_available_bytes | type == "number" and . >= 0)
          and (.current_memory_available_bytes | type == "number" and . >= 0)
          and (.breach | type == "boolean" and . == false)
          and ($r.required_bytes | type == "number" and . > 0 and . <= $r.host_memory_available_bytes)
          and (.phase_memory_max_bytes | type == "number" and . > 0)))
      and (.io_full_psi_windows | type == "array" and length >= 3
        and all(.[]; . as $p
          | (.phase | type == "string" and length > 0)
          and (.stage | type == "string" and length > 0)
          and (.hit | type == "boolean")
          and ($p.consecutive_hits | type == "number" and . >= 0)
          and (if $p.stage == "calibration"
               then ($p.delta_us | type == "number" and . >= 0)
                 and ($p.ratio | type == "number" and . >= 0)
               else true end)))
      and (.shadow_staging | type == "object"
        and (.candidate_assets | type == "object" and (keys | sort) == $shadow_asset_keys)
        and (.restored_assets | type == "object" and (keys | sort) == $shadow_asset_keys)
        and (.before_assets | type == "object" and (keys | sort) == $shadow_asset_keys)
        and (.restored_assets == .before_assets)
        and all([.restored_assets, .before_assets][] | .[];
          ((.state == "present"
            and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))
           or (.state == "absent" and .sha256 == null)
           or (.state == "projection"
             and (.target | type == "string" and length > 0)
             and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))))
        and (.binary | type == "object" and (.candidate_target | type == "string")
          and (.restored_present | type == "boolean")
          and ((.restored_target_sha256 == null)
            or (.restored_target_sha256 | type == "string" and test("^[a-f0-9]{64}$")))))
      and (.checks | type == "object"
        and .before_pair_unchanged == true
        and .shadow_staging_verified == true
        and .shadow_assets_restored == true
        and .resource_preflight == true
        and .oss_triplets == true
        and .strict_segment_verifier == true
        and .final_identity == true
        and .controller_control_bytes == true
        and .shadow_link_restored == true
        and .health_freshness == true)
      and (.markets | type == "object" and ((keys | sort) == ["spot", "usdm"])
        and (to_entries | all(.[]; .value.market == .key))
        and all(.[]; . as $m
          | (.market | type == "string")
          and (.dataset | type == "string" and length > 0)
          and (.session_id | type == "string" and length > 0)
          and (.expected_oss_bucket | type == "string" and length > 0)
          and (.expected_oss_prefix | type == "string" and length > 0)
          and ($m.segment_count | type == "number" and . >= 2 and . == ($m.segments | length))
          and ($m.oss_triplet_count | type == "number" and . >= 2 and . == ($m.triplets | length))
          and (.n_restarts | type == "number" and . == 0)
          and (.process_identity_verified == true)
          and (.installed_shadow_assets_verified == true)
          and (.strict_lob_continuity_readback == true)
          and (.strict_aggregate_trade_continuity_readback | type == "boolean")
          and (.strict_raw_trade_continuity_readback | type == "boolean")
          and (if .market == "spot" then
            .strict_aggregate_trade_continuity_readback == true
            and .strict_raw_trade_continuity_readback == true
          else true end)
          and (.segments | type == "array" and length >= 2
            and all(.[];
              (.file | type == "string" and test("^[A-Za-z0-9._-]+\\.jsonl\\.zst$"))
              and (.path | type == "string" and length > 0)
              and (.data_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.manifest_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.success_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.start_received_at_ns | type == "number" and . >= 0)
              and (.end_received_at_ns | type == "number")
              and (.end_received_at_ns >= .start_received_at_ns)
              and (.session_id | type == "string" and . == $m.session_id)))
          and (.triplets | type == "array" and length >= 2
            and all(.[];
              (.market | type == "string" and . == $m.market)
              and (.dataset | type == "string" and . == $m.dataset)
              and (.data_uri | type == "string"
                and startswith(("oss://" + $m.expected_oss_bucket + "/" + $m.expected_oss_prefix + "/"))
                and test("^oss://[^/]+/.+\\.jsonl\\.zst$"))
              and (.manifest_uri | type == "string")
              and (.manifest_uri == (.data_uri + ".manifest.json"))
              and (.success_uri | type == "string")
              and (.success_uri == (.data_uri + "._SUCCESS"))
              and (.data_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.manifest_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.success_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
              and (.success_content == (.data_sha256 + "\n"))
              and (.start_received_at_ns | type == "number" and . >= 0)
              and (.end_received_at_ns | type == "number")
              and (.end_received_at_ns >= .start_received_at_ns)
              and (.observed_at | type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
              and (.session_id | type == "string" and . == $m.session_id)
              and (.catalog_sha256 | type == "string" and . == $m.health.frozen_catalog_sha256)))
          and (.health | type == "object"
            and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
            and (.session_id | type == "string" and length > 0)
            and (.frozen_catalog_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
            and (.frozen_symbol_count | type == "number" and . >= 1)
            and (.max_health_silence_seconds | type == "number" and . >= 0 and . <= 120)
            and (.samples | type == "number" and . >= 1)
            and .session_id == $m.session_id)))' \
    "$gate" >/dev/null
}

# Validate the transition receipt and its exact V2 Gate evidence.  A cutover
# receipt is not authoritative by itself: the immutable Gate receipt must be
# present, hash-identical, and pass the full evidence validator above.
monday_validate_v2_transition() {
  [[ $# -eq 5 ]] || return 2
  local receipt=$1 from=$2 to=$3 gate=$4 gate_sha=$5
  local gate_evidence gate_payload gate_runtime pair_asset_keys
  # The stable pair contains exactly the eight runtime unit/env assets
  # (production + shadow).  Recovery/health helpers remain controller assets
  # and are addressed through the immutable active controller, never copied
  # into a second live state projection.
  pair_asset_keys=$(monday_runtime_assets | jq -Rsc 'split("\n") | map(select(length > 0)) | sort') || return 1
  monday_file_direct "$receipt" || return 1
  [[ $from == direct || $from =~ ^[a-f0-9]{64}$ ]] || return 1
  [[ $to =~ ^[a-f0-9]{64}$ && $gate_sha =~ ^[a-f0-9]{64}$ ]] || return 1
  monday_file_direct "$gate" || return 1
  monday_validate_v2_gate "$gate" "$from" "$to" "$gate_sha" || return 1
  gate_evidence=$(jq -ceS \
    '{candidate_control_bytes,resource_admission,io_full_psi_windows,shadow_staging,checks,markets}' \
    "$gate") || return 1
  gate_payload=$(jq -er '.candidate_payload_sha256' "$gate") || return 1
  gate_runtime=$(jq -er '.candidate_runtime_contract_sha256' "$gate") || return 1
  jq -e --arg from "$from" --arg to "$to" --arg gate "$gate" --arg gate_sha "$gate_sha" \
    --arg payload "$gate_payload" --arg runtime "$gate_runtime" \
    --argjson pair_asset_keys "$pair_asset_keys" '
    .schema == "monday.rust_lob_pair_transition.v2"
    and .control_plane_version == 2
    and .operation == "cutover"
    and .from_controller_sha256 == $from
    and .controller_sha256 == $to
    and .payload_sha256 == $payload
    and .runtime_contract_sha256 == $runtime
    and .gate_receipt == $gate
    and .gate_sha256 == $gate_sha
    and (.test_only | type == "boolean")
    and (if .test_only then .production_eligible == false else .production_eligible == true end)
    and .active_pair_committed == true
    and (.completed_at | type == "string" and length > 0)
    and (.stable_production_projection | type == "string"
      and . == "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver")
    and (.gate_evidence | type == "object"
      and (.markets | type == "object" and ((keys | sort) == ["spot", "usdm"]))
      and (.candidate_control_bytes | type == "object")
      and (.resource_admission | type == "array" and length >= 3)
      and (.io_full_psi_windows | type == "array" and length >= 3))
    and (.before | type == "object"
      and (.controller == $from)
      and (.payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.production_projection | type == "string"
        and . == "/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver")
      and (.assets | type == "object" and (keys | sort) == $pair_asset_keys
        and all(.[]; ((.state == "present"
          and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))
          or (.state == "absent" and .sha256 == null)
          or (.state == "projection"
            and (.target | type == "string" and length > 0)
            and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))))))
    and (.installed_assets | type == "object" and (keys | sort) == $pair_asset_keys
      and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
    and (.installed_projections | type == "object" and (keys | sort) == $pair_asset_keys
      and all(.[]; type == "string" and length > 0))' \
    "$receipt" >/dev/null
  jq -e --argjson expected "$gate_evidence" \
    '.gate_evidence == $expected' "$receipt" >/dev/null
}

# Verify one production upload-status triplet using an injected copy function.
# The caller owns the OSS credentials; this helper owns URI identity, triplet
# bytes, marker content, and manifest re-download consistency.
monday_verify_upload_triplet_readback() {
  [[ $# -eq 8 ]] || return 2
  local status=$1 market=$2 dataset=$3 expected_bucket=$4 expected_prefix=$5
  local tmp_root=$6 minimum_success_at=$7 copy_fn=$8 triplet data_uri manifest_uri success_uri
  local data_sha manifest_sha success_sha object_prefix first_manifest second_manifest
  local data_file manifest_file success_file expected_success_file expected_prefix_norm
  monday_file_direct "$status" || return 1
  jq -e '.last_error == null' "$status" >/dev/null || return 1
  [[ $market == spot || $market == usdm ]] || return 1
  [[ $dataset =~ ^[A-Za-z0-9_.-]+$ && $expected_bucket =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]] || return 1
  expected_prefix_norm=${expected_prefix%/}
  [[ -n $expected_prefix_norm && $expected_prefix_norm != /* ]] || return 1
  declare -F "$copy_fn" >/dev/null 2>&1 || return 2
  triplet=$(jq -cer '.last_uploaded_triplet | objects' "$status") || return 1
  data_sha=$(jq -er '.data_sha256' <<<"$triplet") || return 1
  manifest_sha=$(jq -er '.manifest_sha256' <<<"$triplet") || return 1
  success_sha=$(jq -er '.success_sha256' <<<"$triplet") || return 1
  monday_sha256_ok "$data_sha" && monday_sha256_ok "$manifest_sha" \
    && monday_sha256_ok "$success_sha" || return 1
  object_prefix=$(jq -er '.object_prefix' <<<"$triplet") || return 1
  # Producers record the directory prefix itself for a triplet, while some
  # upload-status writers append a shard/object component.  Both are valid as
  # long as the value cannot escape the exact expected prefix.
  [[ $object_prefix == "$expected_prefix_norm" || $object_prefix == "$expected_prefix_norm"/* ]] || return 1
  data_uri=$(jq -er '.data_uri // .object // empty' <<<"$triplet") || true
  if [[ -z $data_uri ]]; then data_uri=$(jq -er '.last_uploaded_object' "$status") || return 1; fi
  [[ $data_uri == "oss://$expected_bucket/$expected_prefix_norm/"*.jsonl.zst ]] || return 1
  manifest_uri=$(jq -er '.manifest_uri // empty' <<<"$triplet") || true
  [[ -n $manifest_uri ]] || manifest_uri="$data_uri.manifest.json"
  success_uri=$(jq -er '.success_uri // empty' <<<"$triplet") || true
  [[ -n $success_uri ]] || success_uri="$data_uri._SUCCESS"
  [[ $manifest_uri == "$data_uri.manifest.json" && $success_uri == "$data_uri._SUCCESS" ]] || return 1
  mkdir -p "$tmp_root" || return 1
  first_manifest="$tmp_root/$market.manifest.first"; second_manifest="$tmp_root/$market.manifest.second"
  data_file="$tmp_root/$market.data"; manifest_file="$tmp_root/$market.manifest"; success_file="$tmp_root/$market.success"
  expected_success_file="$tmp_root/$market.success.expected"
  "$copy_fn" "$manifest_uri" "$first_manifest" || return 1
  "$copy_fn" "$data_uri" "$data_file" || return 1
  "$copy_fn" "$success_uri" "$success_file" || return 1
  "$copy_fn" "$manifest_uri" "$second_manifest" || return 1
  monday_file_direct "$first_manifest" && monday_file_direct "$second_manifest" \
    && cmp -s "$first_manifest" "$second_manifest" || return 1
  cp -p -- "$first_manifest" "$manifest_file" || return 1
  [[ $(monday_sha256_file "$data_file") == "$data_sha" \
    && $(monday_sha256_file "$manifest_file") == "$manifest_sha" ]] || return 1
  printf '%s\n' "$data_sha" >"$expected_success_file"
  cmp -s "$success_file" "$expected_success_file" || return 1
  if [[ $success_sha != "$data_sha" ]]; then
    [[ $(monday_sha256_file "$success_file") == "$success_sha" ]] || return 1
  fi
  jq -e --arg market "$market" --arg dataset "$dataset" --arg data_sha "$data_sha" \
    --arg data_file "${data_uri##*/}" '
      type == "object" and .market == $market and .dataset == $dataset
      and .file == $data_file and .sha256 == $data_sha
      and (.shard_id | type == "string" and length > 0)
      and ((.session_id // .lob_continuity.capture_session_id)
        | type == "string" and length > 0)
      and (.catalog_sha256? // "" | type == "string")' \
    "$manifest_file" >/dev/null || return 1
  if [[ -n $minimum_success_at ]]; then
    success_at=$(jq -er '.last_success_at' "$status") || return 1
    minimum_epoch=$(monday_iso_epoch "$minimum_success_at") || return 1
    success_epoch=$(monday_iso_epoch "$success_at") || return 1
    ((success_epoch >= minimum_epoch)) || return 1
  fi
  jq -cn --arg market "$market" --arg data_uri "$data_uri" --arg manifest_uri "$manifest_uri" \
    --arg success_uri "$success_uri" --arg data_sha "$data_sha" --arg manifest_sha "$manifest_sha" \
    --arg success_sha "$success_sha" --arg object_prefix "$object_prefix" \
    --arg last_success_at "$(jq -er '.last_success_at' "$status")" \
    --arg session "$(jq -er '.session_id // .lob_continuity.capture_session_id' "$manifest_file")" \
    --arg catalog "$(jq -er '.catalog_sha256 // ""' "$manifest_file")" \
    '{market:$market,data_uri:$data_uri,manifest_uri:$manifest_uri,success_uri:$success_uri,
      data_sha256:$data_sha,manifest_sha256:$manifest_sha,success_sha256:$success_sha,
      success_content:($data_sha + "\n"),object_prefix:$object_prefix,last_success_at:$last_success_at,
      session_id:$session,catalog_sha256:$catalog}'
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
