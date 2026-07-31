#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ADOPT="$SCRIPT_DIR/host-rust-lob-adopt-production-release.sh"
INSTALL_RELEASE="$SCRIPT_DIR/deploy-rust-lob-release.sh"

for command in awk grep install jq mktemp sed sha256sum; do
  command -v "$command" >/dev/null 2>&1 \
    || { printf 'missing test dependency: %s\n' "$command" >&2; exit 2; }
done

tmp_dir=$(readlink -f "$(mktemp -d)")
cleanup() {
  chmod -R u+w "$tmp_dir" 2>/dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

grep -Fq 'monday-rust-lob-release.lock' "$ADOPT"
grep -Fq 'legacy_binary_supports_upload_only:false' "$ADOPT"
grep -Fq 'purpose:"rollback-compatibility-only"' "$ADOPT"
grep -Fq 'originally_present:false' "$ADOPT"
grep -Fq 'enabled_or_started:false' "$ADOPT"
grep -Fq "AFTER_UNITS == \"\$BEFORE_UNITS\"" "$ADOPT"
grep -Fq "verify_health_continuity \"\$BEFORE_HEALTH\" \"\$AFTER_HEALTH\"" "$ADOPT"
if grep -Eq 'systemctl[[:space:]]+(start|stop|restart|enable|disable|kill)' "$ADOPT"; then
  printf 'adoption helper may not start, stop, restart, enable, or disable services\n' >&2
  exit 1
fi
if sed -n '/^assets=(/,/^)/p' "$INSTALL_RELEASE" | grep -Fq "${ADOPT##*/}"; then
  printf 'adoption helper changed the already-gated candidate deployment bundle\n' >&2
  exit 1
fi

# shellcheck disable=SC1090
. "$ADOPT"

# Translate the Ubuntu host calls used by the helper only on the macOS test
# host. GitHub Actions already provides GNU stat, so preserve its arguments
# there instead of interpreting them as BSD stat flags.
stat() {
  if [[ $(uname -s) != Darwin ]]; then
    /usr/bin/stat "$@"
    return
  fi
  if [[ ${1:-} == -c ]]; then
    local format=$2
    shift 2
    [[ ${1:-} != -- ]] || shift
    case "$format" in
      %u) /usr/bin/stat -f %u "$1" ;;
      %a) /usr/bin/stat -f %Lp "$1" ;;
      *) return 2 ;;
    esac
  else
    /usr/bin/stat "$@"
  fi
}

mv() {
  local -a args=()
  local arg
  for arg in "$@"; do
    [[ $arg == -T || $arg == -Tf ]] || args+=("$arg")
  done
  command mv -f "${args[@]}"
}

sha256sum() {
  local -a args=()
  local arg check=0 checksum_file
  for arg in "$@"; do
    case "$arg" in
      --strict) ;;
      --check) check=1 ;;
      *) args+=("$arg") ;;
    esac
  done
  if (( check )); then
    if (( ${#args[@]} )); then
      command sha256sum -c "${args[@]}"
    else
      checksum_file=$(mktemp)
      command cat >"$checksum_file"
      command sha256sum -c "$checksum_file"
      command rm -f "$checksum_file"
    fi
  elif (( ${#args[@]} )); then
    command sha256sum "${args[@]}"
  else
    command sha256sum
  fi
}

sync() {
  return 0
}

setup_fixture() {
  local fixture=$1 old_body=$2 candidate_body=$3 now_ns gate_dir
  configure_paths "$fixture"
  # Referenced by secure_regular_file in the sourced helper.
  # shellcheck disable=SC2034
  EXPECTED_ROOT_UID=$(id -u)
  mkdir -p \
    "$RELEASE_ROOT" "${PRODUCTION_BINARY%/*}" "$SYSTEMD_ROOT" "$CONFIG_ROOT" \
    "$DATA_ROOT/monday/spool/binance-lob/spot" \
    "$DATA_ROOT/monday/spool/binance-lob/usdm" \
    "$DATA_ROOT/monday/evidence" "$PROC_ROOT/111" "$PROC_ROOT/222"
  printf '#!/usr/bin/env bash\n%s\n' "$old_body" >"$PRODUCTION_BINARY"
  chmod 0755 "$PRODUCTION_BINARY"
  CURRENT_SHA256=$(sha256sum "$PRODUCTION_BINARY" | awk '{print $1}')
  CANDIDATE_SHA256=$(printf '#!/usr/bin/env bash\n%s\n' "$candidate_body" | sha256sum | awk '{print $1}')
  CANDIDATE_RELEASE="$RELEASE_ROOT/$CANDIDATE_SHA256"
  CANDIDATE_DEPLOYMENT="$CANDIDATE_RELEASE/deployment"
  mkdir -p "$CANDIDATE_DEPLOYMENT"
  printf '#!/usr/bin/env bash\n%s\n' "$candidate_body" \
    >"$CANDIDATE_RELEASE/binance-lob-archiver"
  chmod 0755 "$CANDIDATE_RELEASE/binance-lob-archiver"
  install -m 0644 "$SCRIPT_DIR/binance-lob-archiver-upload@.service" \
    "$CANDIDATE_DEPLOYMENT/binance-lob-archiver-upload@.service"
  install -m 0644 "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" \
    "$CANDIDATE_DEPLOYMENT/rust-lob-shadow-gate-policy.jq"
  install -m 0644 "$SCRIPT_DIR/binance-lob-archiver-production@.service" \
    "$PRODUCTION_SERVICE"
  install -m 0640 "$SCRIPT_DIR/binance-lob-archiver-production-spot.env" "$SPOT_ENV"
  install -m 0640 "$SCRIPT_DIR/binance-lob-archiver-production-usdm.env" "$USDM_ENV"
  DEPLOYMENT_BUNDLE_SHA256=$(printf 'b%.0s' {1..64})
  DEPLOYMENT_SOURCE_REVISION=$(printf 'c%.0s' {1..40})
  jq -n \
    --arg artifact "$CANDIDATE_SHA256" \
    --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg source "$DEPLOYMENT_SOURCE_REVISION" \
    '{artifact_sha256:$artifact,deployment_bundle_sha256:$bundle,
      deployment_source_revision:$source}' >"$CANDIDATE_RELEASE/release.json"
  gate_dir="$GATE_ROOT/$CANDIDATE_SHA256/$DEPLOYMENT_BUNDLE_SHA256/runs/gate-1"
  mkdir -p "$gate_dir"
  market=$(jq -cn \
    '{symbol_count:1200,snapshot_ready_count:1200,bridged_count:1200,
      stream_coverage_verified_count:1200,all_stream_coverage_verified:true,sequence_gaps:0,
      upload_failure_count:0,health_samples:121,max_health_silence_seconds:30,
      catalog_sha256:"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      session_id:"shadow-session",oss_roundtrips:2,
      tape_schema:"binance.market_tape.v1",
      agg_trade_segments:2,agg_trade_count:2,
      strict_trade_summary_readback:true,strict_lob_continuity_readback:true,
      lob_reconnect_boundaries:0,
      min_lob_source_latency_ms:0,max_lob_source_latency_ms:0,
      min_lob_bid_levels:1,min_lob_ask_levels:1,max_segment_gap_ns:0,
      oss_roundtrip_evidence:[
        {success_uri:"oss://bucket/part-1.jsonl.zst._SUCCESS",
         sha256:"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
         manifest_sha256:"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
         gap_from_previous_ns:0,start_received_at_ns:100,end_received_at_ns:200,agg_trade_count:1,
         lob_capture_session_id:"shadow-session",lob_reconnect_boundary:false,
         lob_sequence_gaps:0,lob_source_time_rollbacks:0,
         lob_declared_symbol_count:1200,lob_covered_symbol_count:1200,
         stream_coverage_verified_count:1200,all_stream_coverage_verified:true,
         lob_min_source_latency_ms:0,lob_max_source_latency_ms:0,
         lob_min_bid_levels:1,lob_min_ask_levels:1},
        {success_uri:"oss://bucket/part-2.jsonl.zst._SUCCESS",
         sha256:"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
         manifest_sha256:"9999999999999999999999999999999999999999999999999999999999999999",
         gap_from_previous_ns:0,start_received_at_ns:200,end_received_at_ns:300,agg_trade_count:1,
         lob_capture_session_id:"shadow-session",lob_reconnect_boundary:false,
         lob_sequence_gaps:0,lob_source_time_rollbacks:0,
         lob_declared_symbol_count:1200,lob_covered_symbol_count:1200,
         stream_coverage_verified_count:1200,all_stream_coverage_verified:true,
         lob_min_source_latency_ms:0,lob_max_source_latency_ms:0,
         lob_min_bid_levels:1,lob_min_ask_levels:1}
      ]}')
  usdm_market=$(jq -c '
    .symbol_count = 500
    | .snapshot_ready_count = 500
    | .bridged_count = 500
    | .stream_coverage_verified_count = 500
    | .oss_roundtrip_evidence |= map(
        .lob_declared_symbol_count = 500 | .lob_covered_symbol_count = 500
        | .stream_coverage_verified_count = 500)' \
    <<<"$market")
  jq -n \
    --arg artifact "$CANDIDATE_SHA256" \
    --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg source "$DEPLOYMENT_SOURCE_REVISION" \
    --argjson market "$market" \
    --argjson usdm_market "$usdm_market" \
    '{schema:"monday.rust_lob_shadow_gate.v3",candidate_sha256:$artifact,
      deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
      passed:true,production_eligible:true,checks_passed:true,duration_seconds:3600,
      markets:{spot:$market,usdm:$usdm_market}}' \
    >"$gate_dir/gate.json"
  (cd "$gate_dir" && sha256sum gate.json >PASSED.sha256)
  now_ns=$(($(date +%s) * 1000000000))
  jq -n --argjson now "$now_ns" \
    '{market:"spot",dataset:"spot_all",status:"synced",session_id:"prod-spot",
      updated_at_ns:$now,sequence_gaps:0,symbol_count:1357,snapshot_ready_count:1357,
      bridged_count:1357,stream_coverage_verified_count:1357,snapshot_only_symbols:[],
      all_symbols_bridged:true,all_stream_coverage_verified:true,
      pending_upload_segments:0,queue_saturated:false,disk_warning:false,
      upload_warning:false}' >"$DATA_ROOT/monday/spool/binance-lob/spot/health.json"
  jq -n --argjson now "$now_ns" \
    '{market:"usdm",dataset:"usdm_perpetual_all",status:"synced",session_id:"prod-usdm",
      updated_at_ns:$now,sequence_gaps:1,symbol_count:573,snapshot_ready_count:573,
      bridged_count:573,stream_coverage_verified_count:573,snapshot_only_symbols:[],
      all_symbols_bridged:true,all_stream_coverage_verified:true,
      pending_upload_segments:0,queue_saturated:false,disk_warning:false,
      upload_warning:false}' >"$DATA_ROOT/monday/spool/binance-lob/usdm/health.json"
  ln -s "$PRODUCTION_BINARY" "$PROC_ROOT/111/exe"
  ln -s "$PRODUCTION_BINARY" "$PROC_ROOT/222/exe"
  MOCK_RELOADS="$fixture/reloads"
  printf '0\n' >"$MOCK_RELOADS"
}

systemctl() {
  local command=$1 unit property count
  case "$command" in
    is-active)
      unit=${!#}
      case "$unit" in
        binance-lob-archiver-production@spot.service|binance-lob-archiver-production@usdm.service)
          return 0 ;;
        *) return 3 ;;
      esac
      ;;
    is-enabled)
      unit=${!#}
      case "$unit" in
        binance-lob-archiver-production@spot.service|binance-lob-archiver-production@usdm.service)
          [[ ${2:-} == --quiet ]] || printf 'enabled\n'
          return 0 ;;
        *)
          if [[ -f $UPLOAD_SERVICE ]]; then printf 'static\n'; else printf 'not-found\n'; fi
          return 1
          ;;
      esac
      ;;
    show)
      unit=$2
      property=$3
      case "$property" in
        --property=MainPID)
          [[ $unit == *spot.service ]] && printf '111\n' || printf '222\n'
          ;;
        --property=NRestarts) printf '1\n' ;;
        *) return 2 ;;
      esac
      ;;
    daemon-reload)
      count=$(<"$MOCK_RELOADS")
      printf '%s\n' "$((count + 1))" >"$MOCK_RELOADS"
      ;;
    *) return 2 ;;
  esac
}

# Literal fixture programs; parameter expansion happens when they run.
# shellcheck disable=SC2016
old_body='[[ ${1:-} == --help ]] && { printf "%s\\n" "legacy help"; exit 0; }; exit 0'
# shellcheck disable=SC2016
candidate_body='[[ ${1:-} == --help ]] && { printf "%s\\n" "--upload-only"; exit 0; }; exit 0'

run_success_fixture() (
  fixture="$tmp_dir/success"
  setup_fixture "$fixture" "$old_body" "$candidate_body"
  old_sha=$CURRENT_SHA256
  candidate_sha=$CANDIDATE_SHA256
  adopt_release "$old_sha" "$candidate_sha" >"$fixture/first.out"
  [[ -L $PRODUCTION_BINARY ]]
  [[ $(readlink -f "$PRODUCTION_BINARY") \
    == "$RELEASE_ROOT/$old_sha/binance-lob-archiver" ]]
  printf '%s  %s\n' "$old_sha" "$PRODUCTION_BINARY" \
    | sha256sum --check --strict >/dev/null
  cmp -s "$UPLOAD_SERVICE" \
    "$RELEASE_ROOT/$old_sha/deployment/binance-lob-archiver-upload@.service"
  [[ $(stat -c %a -- "$RELEASE_ROOT/$old_sha") == 755 ]]
  [[ -f $RELEASE_ROOT/$old_sha/adopted-release.json ]]
  [[ ! -e $RELEASE_ROOT/$old_sha/release.json ]]
  (cd "$RELEASE_ROOT/$old_sha" \
    && sha256sum --check --strict deployment.sha256 >/dev/null)
  evidence="$EVIDENCE_ROOT/$old_sha"
  [[ $(stat -c %a -- "$evidence") == 550 ]]
  (cd "$evidence" \
    && sha256sum --check --strict ADOPTED.sha256 >/dev/null \
    && sha256sum --check --strict MANIFEST.sha256 >/dev/null)
  jq -e '
    .result == "passed"
    and .legacy_binary_supports_upload_only == false
    and .synthetic_upload_unit.originally_present == false
    and .synthetic_upload_unit.enabled_or_started == false
    and .production_units.before == .production_units.after
    and .production_units.after.spot.n_restarts == 1
    and .production_units.after.usdm.n_restarts == 1' \
    "$evidence/adoption.json" >/dev/null
  [[ $(<"$MOCK_RELOADS") == 1 ]]
  adopt_release "$old_sha" "$candidate_sha" >"$fixture/second.out"
  grep -Fq 'already adopted' "$fixture/second.out"
  [[ $(<"$MOCK_RELOADS") == 1 ]]
  chmod 0777 "$RELEASE_ROOT/$old_sha/binance-lob-archiver"
  if adopt_release "$old_sha" "$candidate_sha" >"$fixture/binary-mode-drift.out" 2>&1; then
    printf 'idempotent adoption accepted a writable release binary\n' >&2
    exit 1
  fi
  grep -Fq 'required file is missing, indirect, writable, or not root-owned' \
    "$fixture/binary-mode-drift.out"
  chmod 0755 "$RELEASE_ROOT/$old_sha/binance-lob-archiver"
  printf 'tampered\n' >>"$UPLOAD_SERVICE"
  if adopt_release "$old_sha" "$candidate_sha" >"$fixture/drift.out" 2>&1; then
    printf 'idempotent adoption accepted a drifted upload unit\n' >&2
    exit 1
  fi
  grep -Fq 'installed upload unit drifted' "$fixture/drift.out"
  [[ -f $evidence/adoption.json && -f $evidence/ADOPTED.sha256 ]]
  [[ -d $RELEASE_ROOT/$old_sha ]]
)

run_failure_fixture() (
  fixture="$tmp_dir/failure"
  setup_fixture "$fixture" "$old_body" "$candidate_body"
  old_sha=$CURRENT_SHA256
  candidate_sha=$CANDIDATE_SHA256
  # Overrides the sourced implementation to exercise post-symlink rollback.
  # shellcheck disable=SC2317,SC2329
  write_adoption_evidence() { return 1; }
  if adopt_release "$old_sha" "$candidate_sha" >"$fixture/failure.out" 2>&1; then
    printf 'adoption unexpectedly passed after evidence failure\n' >&2
    exit 1
  fi
  [[ -f $PRODUCTION_BINARY && ! -L $PRODUCTION_BINARY ]]
  printf '%s  %s\n' "$old_sha" "$PRODUCTION_BINARY" \
    | sha256sum --check --strict >/dev/null
  [[ ! -e $UPLOAD_SERVICE && ! -L $UPLOAD_SERVICE ]]
  [[ ! -e $RELEASE_ROOT/$old_sha && ! -L $RELEASE_ROOT/$old_sha ]]
  [[ ! -e $EVIDENCE_ROOT/$old_sha && ! -L $EVIDENCE_ROOT/$old_sha ]]
  [[ $(<"$MOCK_RELOADS") == 2 ]]
  [[ -z $(find "$fixture" \( -name '*.adopt.*' -o -name '*.new.*' \) -print) ]]
)

run_unhealthy_fixture() (
  fixture="$tmp_dir/unhealthy"
  setup_fixture "$fixture" "$old_body" "$candidate_body"
  old_sha=$CURRENT_SHA256
  candidate_sha=$CANDIDATE_SHA256
  jq '.status = "degraded"' \
    "$DATA_ROOT/monday/spool/binance-lob/spot/health.json" >"$fixture/health.tmp"
  mv "$fixture/health.tmp" "$DATA_ROOT/monday/spool/binance-lob/spot/health.json"
  if adopt_release "$old_sha" "$candidate_sha" >"$fixture/unhealthy.out" 2>&1; then
    printf 'adoption accepted unhealthy production state\n' >&2
    exit 1
  fi
  [[ -f $PRODUCTION_BINARY && ! -L $PRODUCTION_BINARY ]]
  [[ ! -e $UPLOAD_SERVICE && ! -L $UPLOAD_SERVICE ]]
  [[ ! -e $RELEASE_ROOT/$old_sha && ! -L $RELEASE_ROOT/$old_sha ]]
)

run_gate_marker_fixture() (
  fixture="$tmp_dir/gate-marker"
  setup_fixture "$fixture" "$old_body" "$candidate_body"
  old_sha=$CURRENT_SHA256
  candidate_sha=$CANDIDATE_SHA256
  gate_dir=$(find "$GATE_ROOT" -name PASSED.sha256 -exec dirname {} \;)
  printf 'extra\n' >"$gate_dir/extra"
  (cd "$gate_dir" && sha256sum extra >>PASSED.sha256)
  if adopt_release "$old_sha" "$candidate_sha" >"$fixture/gate-marker.out" 2>&1; then
    printf 'adoption accepted a multi-entry candidate gate marker\n' >&2
    exit 1
  fi
  grep -Fq 'marker must contain exactly one entry' "$fixture/gate-marker.out"
  [[ -f $PRODUCTION_BINARY && ! -L $PRODUCTION_BINARY ]]
  [[ ! -e $UPLOAD_SERVICE && ! -L $UPLOAD_SERVICE ]]
)

run_success_fixture
run_failure_fixture
run_unhealthy_fixture
run_gate_marker_fixture
printf 'Rust LOB release adoption tests passed\n'
