#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
RESTORE="$SCRIPT_DIR/host-rust-lob-restore.sh"
INVOKE="$SCRIPT_DIR/invoke-rust-lob-operation.sh"
INSTALL_RELEASE="$SCRIPT_DIR/deploy-rust-lob-release.sh"

for command in awk cmp find grep head install jq mktemp sed sha256sum; do
  command -v "$command" >/dev/null 2>&1 \
    || { printf 'missing test dependency: %s\n' "$command" >&2; exit 2; }
done

tmp_dir=$(readlink -f "$(mktemp -d)")
cleanup() {
  chmod -R u+w "$tmp_dir" 2>/dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

# Structural contract checks.
for step in \
  validate-production-symlink \
  validate-shadow-gate \
  validate-production-quiescent \
  validate-canonical-spool \
  validate-segment-spool \
  validate-installed-production-assets; do
  grep -Fq "STEP=$step" "$RESTORE" \
    || { printf 'restore script missing fail-closed step: %s\n' "$step" >&2; exit 1; }
done
grep -Fq 'refusing to reuse recovery evidence directory' "$RESTORE"
grep -Fq 'monday.rust_lob_recovery.v1' "$RESTORE"
grep -Fq 'monday.rust_lob_recovery_verification.v1' "$RESTORE"
grep -Fq 'systemctl disable --now' "$RESTORE"
grep -Fq 'systemctl mask --runtime' "$RESTORE"
# Restore must never rewrite the production symlink or touch deployment assets.
# shellcheck disable=SC2016
if grep -Eq 'ln[[:space:]]+-[snf]|rm[[:space:]]+.*\$PRODUCTION_LINK|unlink[[:space:]]+\$PRODUCTION_LINK' "$RESTORE"; then
  printf 'restore script may not rewrite the production symlink\n' >&2
  exit 1
fi
# Restore may only start/stop/enable/disable the production units; upload and
# legacy units are masked fail-closed and must never be started or enabled.
if grep -En 'systemctl[[:space:]]+(start|stop|restart|enable|disable)' "$RESTORE" \
  | grep -Ev 'PRODUCTION_UNITS' >/dev/null; then
  printf 'restore may only start, stop, enable, or disable production units\n' >&2
  exit 1
fi
if sed -n '/^assets=(/,/^)/p' "$INSTALL_RELEASE" | grep -Fq 'host-rust-lob-restore.sh'; then
  :
else
  printf 'deployment bundle does not include host-rust-lob-restore.sh\n' >&2
  exit 1
fi
grep -Fq 'restore)' "$INVOKE"
grep -Fq 'monday-rust-lob-restore' "$INVOKE"

# shellcheck disable=SC1090
. "$RESTORE"

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

# Collapse the health-wait poll into an immediate deadline so the fail-closed
# health timeout is exercised without a five-second real sleep per poll.
sleep() {
  SECONDS=$((SECONDS + 1))
  return 0
}

setup_fixture() {
  local fixture=$1 gate_dir now_ns digest_dir staging usdm_symbols_config usdm_catalog
  configure_paths "$fixture"
  # Referenced by secure_regular_file in the sourced helper.
  # shellcheck disable=SC2034
  EXPECTED_ROOT_UID=$(id -u)
  MOCK_STATE="$fixture/mock-systemctl"
  mkdir -p \
    "$OPT_ROOT" "$BIN_DIR" "$RELEASE_ROOT" "$SYSTEMD_ROOT" "$CONFIG_ROOT" \
    "$DATA_ROOT/monday/spool/binance-lob/spot" \
    "$DATA_ROOT/monday/spool/binance-lob/usdm" \
    "$DATA_ROOT/monday/evidence" \
    "$PROC_ROOT/111" "$PROC_ROOT/222" \
    "$MOCK_STATE/active" "$MOCK_STATE/enabled" "$MOCK_STATE/masked"
  staging="$RELEASE_ROOT/.staging"
  mkdir -p "$staging"
  printf '#!/usr/bin/env bash\nprintf "restored-collector\\n"\n' \
    >"$staging/binance-lob-archiver"
  chmod 0755 "$staging/binance-lob-archiver"
  CANDIDATE_SHA256=$(sha256sum "$staging/binance-lob-archiver" | awk '{print $1}')
  digest_dir="$RELEASE_ROOT/$CANDIDATE_SHA256"
  mkdir -p "$digest_dir"
  mv "$staging/binance-lob-archiver" "$digest_dir/binance-lob-archiver"
  rmdir "$staging"
  CANDIDATE_RELEASE="$digest_dir"
  CANDIDATE_DEPLOYMENT="$CANDIDATE_RELEASE/deployment"
  mkdir -p "$CANDIDATE_DEPLOYMENT"
  CANDIDATE_BINARY="$CANDIDATE_RELEASE/binance-lob-archiver"
  DEPLOYMENT_BUNDLE_SHA256=$(printf 'b%.0s' {1..64})
  DEPLOYMENT_SOURCE_REVISION=$(printf 'c%.0s' {1..40})
  jq -n \
    --arg artifact "$CANDIDATE_SHA256" \
    --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg source "$DEPLOYMENT_SOURCE_REVISION" \
    '{artifact_sha256:$artifact,deployment_bundle_sha256:$bundle,
      deployment_source_revision:$source}' >"$CANDIDATE_RELEASE/release.json"
  for asset in \
    binance-lob-archiver-production@.service \
    binance-lob-archiver-production-spot.env \
    binance-lob-archiver-production-usdm.env; do
    install -m 0644 "$SCRIPT_DIR/$asset" "$CANDIDATE_DEPLOYMENT/$asset"
  done
  install -m 0644 "$SCRIPT_DIR/binance-lob-archiver-production@.service" \
    "$SYSTEMD_ROOT/binance-lob-archiver-production@.service"
  install -m 0640 "$SCRIPT_DIR/binance-lob-archiver-production-spot.env" \
    "$CONFIG_ROOT/binance-lob-archiver-production-spot.env"
  install -m 0640 "$SCRIPT_DIR/binance-lob-archiver-production-usdm.env" \
    "$CONFIG_ROOT/binance-lob-archiver-production-usdm.env"
  install -m 0644 "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" \
    "$CANDIDATE_DEPLOYMENT/rust-lob-shadow-gate-policy.jq"
  install -m 0644 "$SCRIPT_DIR/rust-lob-runtime-health-policy.jq" \
    "$CANDIDATE_DEPLOYMENT/rust-lob-runtime-health-policy.jq"
  ln -s "$CANDIDATE_BINARY" "$PRODUCTION_LINK"
  gate_dir="$GATE_ROOT/$CANDIDATE_SHA256/$DEPLOYMENT_BUNDLE_SHA256/runs/gate-1"
  mkdir -p "$gate_dir"
  market=$(jq -cn \
    '{symbol_count:1200,snapshot_ready_count:1200,bridged_count:1200,
      stream_coverage_verified_count:1200,all_stream_coverage_verified:true,sequence_gaps:0,
      upload_failure_count:0,health_samples:121,max_health_silence_seconds:30,
      symbols_config:"ALL",
      catalog_sha256:"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      configured_catalog_sha256:"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
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
  usdm_symbols_config=$(sed -n 's/^SYMBOLS=//p' \
    "$SCRIPT_DIR/binance-lob-archiver-production-usdm.env")
  usdm_catalog=$(jq -cn --arg symbols "$usdm_symbols_config" \
    '$symbols | split(",") | sort' | sha256sum | awk '{print $1}')
  usdm_market=$(jq -c \
    --arg symbols_config "$usdm_symbols_config" \
    --arg catalog_sha256 "$usdm_catalog" '
    .symbol_count = 100
    | .snapshot_ready_count = 100
    | .bridged_count = 100
    | .stream_coverage_verified_count = 100
    | .symbols_config = $symbols_config
    | .catalog_sha256 = $catalog_sha256
    | .configured_catalog_sha256 = $catalog_sha256
    | .oss_roundtrip_evidence |= map(
        .lob_declared_symbol_count = 100 | .lob_covered_symbol_count = 100
        | .stream_coverage_verified_count = 100)' \
    <<<"$market")
  jq -n \
    --arg artifact "$CANDIDATE_SHA256" \
    --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg source "$DEPLOYMENT_SOURCE_REVISION" \
    --arg run_id 20260820T000000Z-1 \
    --arg run_spool "/data/monday/spool/binance-lob-rust-shadow/runs/$CANDIDATE_SHA256/20260820T000000Z-1" \
    --argjson market "$market" \
    --argjson usdm_market "$usdm_market" \
    '{schema:"monday.rust_lob_shadow_gate.v3",candidate_sha256:$artifact,
      deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
      run_id:$run_id,run_spool:$run_spool,
      required_duration_seconds:240,requested_duration_seconds:240,
      health_settle_seconds:240,segment_seconds:120,test_only:false,
      passed:true,production_eligible:true,checks_passed:true,duration_seconds:240,
      markets:{spot:$market,usdm:$usdm_market}}' \
    >"$gate_dir/gate.json"
  (cd "$gate_dir" && sha256sum gate.json >PASSED.sha256)
  now_ns=$(($(date +%s) * 1000000000))
  jq -n --argjson now "$now_ns" \
    '{market:"spot",dataset:"spot_all",status:"synced",session_id:"pre-spot",
      updated_at_ns:$now,sequence_gaps:0,symbol_count:1357,snapshot_ready_count:1357,
      bridged_count:1357,stream_coverage_verified_count:1357,snapshot_only_symbols:[],
      all_symbols_bridged:true,all_stream_coverage_verified:true,
      pending_upload_segments:0,queue_saturated:false,disk_warning:false,
      upload_warning:false}' >"$CANONICAL_SPOOL/spot/health.json"
  jq -n --argjson now "$now_ns" \
    '{market:"usdm",dataset:"usdm_perpetual_all",status:"synced",session_id:"pre-usdm",
      updated_at_ns:$now,sequence_gaps:0,symbol_count:573,snapshot_ready_count:573,
      bridged_count:573,stream_coverage_verified_count:573,snapshot_only_symbols:[],
      all_symbols_bridged:true,all_stream_coverage_verified:true,
      pending_upload_segments:0,queue_saturated:false,disk_warning:false,
      upload_warning:false}' >"$CANONICAL_SPOOL/usdm/health.json"
  ln -s "$CANDIDATE_BINARY" "$PROC_ROOT/111/exe"
  ln -s "$CANDIDATE_BINARY" "$PROC_ROOT/222/exe"
}

mock_write_health() {
  local market=$1 session symbol_count
  if [[ $market == spot ]]; then
    session=post-spot
  else
    session=post-usdm
  fi
  if [[ ${MOCK_FAULTY:-0} == 1 ]]; then
    symbol_count=5
  elif [[ $market == usdm ]]; then
    symbol_count=100
  else
    symbol_count=1357
  fi
  mkdir -p "$CANONICAL_SPOOL/$market"
  jq -n \
    --argjson now "$(date +%s%N)" \
    --arg market "$market" \
    --arg session "$session" \
    --argjson symbol_count "$symbol_count" \
    '{market:$market,dataset:(if $market=="spot" then "spot_all" else "usdm_perpetual_all" end),
      status:"synced",session_id:$session,updated_at_ns:$now,sequence_gaps:0,
      symbol_count:$symbol_count,snapshot_ready_count:$symbol_count,bridged_count:$symbol_count,
      stream_coverage_verified_count:$symbol_count,snapshot_only_symbols:[],
      all_symbols_bridged:true,all_stream_coverage_verified:true,
      pending_upload_segments:0,queue_saturated:false,disk_warning:false,
      upload_warning:false}' >"$CANONICAL_SPOOL/$market/health.json"
}

systemctl() {
  local command=$1 unit property arg
  case "$command" in
    is-active)
      unit=${!#}
      [[ -f $MOCK_STATE/active/$unit ]]
      ;;
    is-enabled)
      unit=${!#}
      if [[ -f $MOCK_STATE/masked/$unit ]]; then
        [[ ${2:-} == --quiet ]] || printf 'masked\n'
        return 1
      fi
      if [[ -f $MOCK_STATE/enabled/$unit ]]; then
        [[ ${2:-} == --quiet ]] || printf 'enabled\n'
        return 0
      fi
      [[ ${2:-} == --quiet ]] || printf 'disabled\n'
      return 1
      ;;
    show)
      unit=$2
      property=$3
      case "$property" in
        --property=MainPID) printf '111\n' ;;
        --property=NRestarts) printf '0\n' ;;
        *) return 2 ;;
      esac
      ;;
    reset-failed) return 0 ;;
    unmask) return 0 ;;
    start)
      mock_write_health spot
      mock_write_health usdm
      touch "$MOCK_STATE/active/binance-lob-archiver-production@spot.service"
      touch "$MOCK_STATE/active/binance-lob-archiver-production@usdm.service"
      ;;
    enable)
      for arg in "${@:2}"; do
        touch "$MOCK_STATE/enabled/$arg"
      done
      ;;
    disable)
      for arg in "${@:2}"; do
        [[ $arg == --now ]] && continue
        rm -f "$MOCK_STATE/enabled/$arg" "$MOCK_STATE/active/$arg"
      done
      ;;
    mask)
      for arg in "${@:2}"; do
        [[ $arg == --runtime ]] && continue
        touch "$MOCK_STATE/masked/$arg"
      done
      ;;
    *) return 2 ;;
  esac
}

restore_evidence_dir() {
  find "$EVIDENCE_ROOT" -maxdepth 1 -type d -name "*-${CANDIDATE_SHA256:0:12}-*" \
    -print | head -n1
}

assert_failed_recovery() {
  local out=$1 evidence
  evidence=$(restore_evidence_dir)
  [[ -f $evidence/recovery.json ]] || { printf 'missing recovery.json\n' >&2; exit 1; }
  jq -e --arg step "$2" \
    '.result == "failed" and .last_step == $step and .candidate_sha256 == "'"$CANDIDATE_SHA256"'"' \
    "$evidence/recovery.json" >/dev/null \
    || { printf 'unexpected recovery evidence for step %s\n' "$2" >&2; exit 1; }
  grep -Fq "$3" "$out" \
    || { printf 'missing expected failure text: %s\n' "$3" >&2; exit 1; }
}

run_success_fixture() (
  fixture="$tmp_dir/success"
  setup_fixture "$fixture"
  restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1 \
    || { printf 'restore failed in success fixture\n' >&2; exit 1; }
  evidence=$(restore_evidence_dir)
  [[ -n $evidence ]] || { printf 'no recovery evidence directory\n' >&2; exit 1; }
  jq -e --arg sha "$CANDIDATE_SHA256" --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    '.result == "passed"
     and .candidate_sha256 == $sha
     and .deployment_bundle_sha256 == $bundle
     and .previous_session_spot == "pre-spot"
     and .previous_session_usdm == "pre-usdm"
     and .production_units_active.spot == true
     and .production_units_active.usdm == true' \
    "$evidence/recovery.json" >/dev/null
  jq -e '
    .production_units["binance-lob-archiver-production@spot.service"].active == true
    and .production_units["binance-lob-archiver-production@spot.service"].enabled == true
    and .production_units["binance-lob-archiver-production@usdm.service"].active == true
    and .production_units["binance-lob-archiver-production@usdm.service"].enabled == true
    and .production_units["binance-lob-archiver-production@spot.service"].runtime_max_sec == 21600
    and .verification.symlink_sha256 == true
    and .verification.gate_marker_verified == true' \
    "$evidence/verification.json" >/dev/null
  # Symlink must be untouched and still resolve to the candidate.
  [[ -L $PRODUCTION_LINK ]]
  [[ $(readlink -f "$PRODUCTION_LINK") == "$CANDIDATE_BINARY" ]]
  printf '%s  %s\n' "$CANDIDATE_SHA256" "$PRODUCTION_LINK" \
    | sha256sum --check --strict >/dev/null
  # Gate evidence copied into the immutable recovery record.
  [[ -f $evidence/shadow-gate/gate.json && -f $evidence/shadow-gate/PASSED.sha256 ]]
  (cd "$evidence/shadow-gate" && sha256sum --check --strict PASSED.sha256 >/dev/null)
  # Pre-restore health preserved and production health fresh.
  [[ -f $evidence/previous-spot-health.json && -f $evidence/previous-usdm-health.json ]]
  [[ -f $evidence/production-spot-health.json && -f $evidence/production-usdm-health.json ]]
  [[ -f $CANONICAL_SPOOL/spot/health.json && -f $CANONICAL_SPOOL/usdm/health.json ]]
  # Units started and enabled.
  [[ -f $MOCK_STATE/active/binance-lob-archiver-production@spot.service ]]
  [[ -f $MOCK_STATE/enabled/binance-lob-archiver-production@spot.service ]]
  [[ -f $MOCK_STATE/active/binance-lob-archiver-production@usdm.service ]]
  [[ -f $MOCK_STATE/enabled/binance-lob-archiver-production@usdm.service ]]
)

run_missing_symlink_fixture() (
  fixture="$tmp_dir/missing-symlink"
  setup_fixture "$fixture"
  rm -f "$PRODUCTION_LINK"
  if restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1; then
    printf 'restore accepted a missing production symlink\n' >&2
    exit 1
  fi
  assert_failed_recovery "$fixture/out" validate-production-symlink \
    'production symlink is missing'
  # Symlink must remain absent; restore must never recreate it.
  [[ ! -e $PRODUCTION_LINK && ! -L $PRODUCTION_LINK ]]
)

run_symlink_mismatch_fixture() (
  fixture="$tmp_dir/symlink-mismatch"
  setup_fixture "$fixture"
  other_binary="$RELEASE_ROOT/other-binance-lob-archiver"
  printf '#!/usr/bin/env bash\nexit 1\n' >"$other_binary"
  chmod 0755 "$other_binary"
  ln -sfn "$other_binary" "$PRODUCTION_LINK"
  if restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1; then
    printf 'restore accepted a symlink that does not resolve to the candidate\n' >&2
    exit 1
  fi
  assert_failed_recovery "$fixture/out" validate-production-symlink \
    'production symlink does not resolve to the candidate release'
  [[ $(readlink -f "$PRODUCTION_LINK") == "$other_binary" ]]
)

run_missing_gate_fixture() (
  fixture="$tmp_dir/missing-gate"
  setup_fixture "$fixture"
  rm -rf "${GATE_ROOT:?}/$CANDIDATE_SHA256"
  if restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1; then
    printf 'restore accepted a release without an immutable passed shadow gate\n' >&2
    exit 1
  fi
  assert_failed_recovery "$fixture/out" validate-shadow-gate \
    'expected exactly one immutable passed shadow gate'
)

run_active_production_fixture() (
  fixture="$tmp_dir/active-production"
  setup_fixture "$fixture"
  touch "$MOCK_STATE/active/binance-lob-archiver-production@spot.service"
  touch "$MOCK_STATE/active/binance-lob-archiver-production@usdm.service"
  if restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1; then
    printf 'restore accepted live production units\n' >&2
    exit 1
  fi
  assert_failed_recovery "$fixture/out" validate-production-quiescent \
    'refusing live production'
)

run_spool_symlink_fixture() (
  fixture="$tmp_dir/spool-symlink"
  setup_fixture "$fixture"
  mkdir -p "$tmp_dir/outside"
  rm -rf "$CANONICAL_SPOOL"
  ln -s "$tmp_dir/outside" "$CANONICAL_SPOOL"
  if restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1; then
    printf 'restore accepted a canonical spool symlink\n' >&2
    exit 1
  fi
  assert_failed_recovery "$fixture/out" validate-canonical-spool \
    'canonical spool path contains a symlink'
)

run_segment_spool_fixture() (
  fixture="$tmp_dir/segment-spool"
  setup_fixture "$fixture"
  touch "$CANONICAL_SPOOL/spot/part-0000.jsonl.zst"
  if restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1; then
    printf 'restore accepted segment artifacts in the canonical spool\n' >&2
    exit 1
  fi
  assert_failed_recovery "$fixture/out" validate-segment-spool \
    'canonical spool contains segment artifacts'
)

run_drifted_assets_fixture() (
  fixture="$tmp_dir/drifted-assets"
  setup_fixture "$fixture"
  printf '\n# tampered\n' >>"$SYSTEMD_ROOT/binance-lob-archiver-production@.service"
  if restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1; then
    printf 'restore accepted drifted installed production assets\n' >&2
    exit 1
  fi
  assert_failed_recovery "$fixture/out" validate-installed-production-assets \
    'installed production asset drifted from the gated deployment bundle'
)

run_health_failure_fixture() (
  fixture="$tmp_dir/health-failure"
  setup_fixture "$fixture"
  # Read by wait_for_release_health in the sourced restore helper; shellcheck
  # cannot see that cross-file reference.
  # shellcheck disable=SC2034
  HEALTH_TIMEOUT_SECONDS=1
  MOCK_FAULTY=1
  if restore_release "$CANDIDATE_SHA256" >"$fixture/out" 2>&1; then
    printf 'restore accepted production that never reached verified health\n' >&2
    exit 1
  fi
  grep -Fq 'restored production did not reach verified catalog health' "$fixture/out"
  evidence=$(restore_evidence_dir)
  [[ -f $evidence/recovery.json ]]
  jq -e '
    .result == "failed"
    and .rollback_result == "disabled"
    and .production_units_active.spot == false
    and .production_units_active.usdm == false' \
    "$evidence/recovery.json" >/dev/null
  # Fail-closed rollback: production stopped, disabled, masked.
  [[ ! -f $MOCK_STATE/active/binance-lob-archiver-production@spot.service ]]
  [[ ! -f $MOCK_STATE/enabled/binance-lob-archiver-production@spot.service ]]
  [[ -f $MOCK_STATE/masked/binance-lob-archiver-production@spot.service ]]
  [[ -f $MOCK_STATE/masked/binance-lob-archiver-production@usdm.service ]]
  [[ -f $MOCK_STATE/masked/binance-lob-archiver-upload@spot.service ]]
  [[ -f $MOCK_STATE/masked/binance-lob-archiver-upload@usdm.service ]]
  [[ -f $MOCK_STATE/masked/binance-lob-archiver@spot.service ]]
  [[ -f $MOCK_STATE/masked/binance-lob-archiver@usdm.service ]]
  # Symlink untouched.
  [[ $(readlink -f "$PRODUCTION_LINK") == "$CANDIDATE_BINARY" ]]
  # Post-restart health captured before rollback.
  [[ -f $evidence/rollback-spot-health.json && -f $evidence/rollback-usdm-health.json ]]
)

run_success_fixture
run_missing_symlink_fixture
run_symlink_mismatch_fixture
run_missing_gate_fixture
run_active_production_fixture
run_spool_symlink_fixture
run_segment_spool_fixture
run_drifted_assets_fixture
run_health_failure_fixture

printf 'Rust LOB governed restore tests passed\n'
