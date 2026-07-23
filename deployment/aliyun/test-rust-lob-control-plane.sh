#!/usr/bin/env bash
# Dynamically sourced production functions consume fixture globals and mocks.
# shellcheck disable=SC2016,SC2034,SC2317,SC2329
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
CUTOVER="$SCRIPT_DIR/host-rust-lob-cutover.sh"
GATE="$SCRIPT_DIR/host-rust-lob-shadow-gate.sh"
INSTALL_RELEASE="$SCRIPT_DIR/deploy-rust-lob-release.sh"
INVOKE="$SCRIPT_DIR/invoke-rust-lob-operation.sh"
POLICY="$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq"
RUNTIME_POLICY="$SCRIPT_DIR/rust-lob-runtime-health-policy.jq"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

for command in awk base64 cmp cut grep install jq mktemp sed seq sha256sum; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'missing test dependency: %s\n' "$command" >&2
    exit 2
  }
done

grep -Fq '.trade_summary_contract == "binance.aggregate_trade_summary.v1"' "$GATE"
grep -Fq 'strict_verifier_args+=' "$GATE"
grep -Fq -- '--verify-segment' "$GATE"
grep -Fq -- '--segment-content-sha256' "$GATE"
grep -Fq -- '--segment-manifest-sha256' "$GATE"
grep -Fq -- '--require-lob-continuity' "$GATE"
grep -Fq '"$candidate_binary" "${strict_verifier_args[@]}"' "$GATE"
grep -Fq '.lob_continuity.contract == "binance.lob_continuity.v1"' "$GATE"
grep -Fq 'jq -e --arg session_id "${observed_session[$market]}"' "$GATE"
grep -Fq -- '--slurpfile manifest "$manifest_path"' "$GATE"
if grep -Fq -- '--argjson lob_continuity' "$GATE"; then
  printf 'shadow gate passes the full-catalog LOB summary through argv\n' >&2
  exit 1
fi
grep -Fq 'manifest changed between discovery and readback' "$GATE"
grep -Fq 'manifest_sha256:$manifest_sha256' "$GATE"
grep -Fq 'readonly MAX_HEALTH_SILENCE_SECONDS=120' "$GATE"

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

last_updated_ns=1
last_advance_mono=0
max_gap=0
health_sample_increments=0
for current_mono in $(seq 30 30 3600); do
  current_updated_ns=$((current_mono * 1000000000))
  read -r last_updated_ns last_advance_mono max_gap sample_increment < <(
    monday_observe_health_freshness \
      "$last_updated_ns" "$last_advance_mono" "$max_gap" \
      "$current_updated_ns" "$current_mono" 120
  )
  health_sample_increments=$((health_sample_increments + sample_increment))
done
((health_sample_increments == 120 && max_gap <= 120)) || {
  printf 'fresh one-hour health sequence did not pass the monotonic observer\n' >&2
  exit 1
}
read -r jitter_updated_ns jitter_advance_mono jitter_max_gap jitter_increment < <(
  monday_observe_health_freshness 1 0 0 2 91 120
)
[[ $jitter_updated_ns == 2 && $jitter_advance_mono == 91 \
  && $jitter_max_gap == 91 && $jitter_increment == 1 ]] || {
  printf 'monotonic observer rejected an advancing 91-second jitter sample\n' >&2
  exit 1
}
if monday_observe_health_freshness \
  "$jitter_updated_ns" "$jitter_advance_mono" "$jitter_max_gap" \
  "$jitter_updated_ns" "$((jitter_advance_mono + 121))" 120 >/dev/null; then
  printf 'monotonic observer accepted a 121-second health freeze\n' >&2
  exit 1
fi

artifact=$(printf 'a%.0s' {1..64})
bundle=$(printf 'b%.0s' {1..64})
source_revision=$(printf 'c%.0s' {1..40})
catalog=$(printf 'd%.0s' {1..64})

market_json=$(jq -cn \
  --arg catalog "$catalog" \
  '{symbol_count:1200,snapshot_ready_count:1200,sequence_gaps:0,
    upload_failure_count:0,health_samples:121,max_health_silence_seconds:30,
    catalog_sha256:$catalog,
    session_id:"session-1",oss_roundtrips:2,
    agg_trade_segments:2,agg_trade_count:2,
    strict_trade_summary_readback:true,
    strict_lob_continuity_readback:true,lob_reconnect_boundaries:1,
    min_lob_source_latency_ms:0,max_lob_source_latency_ms:0,
    min_lob_bid_levels:1,min_lob_ask_levels:1,
    max_segment_gap_ns:0,
    oss_roundtrip_evidence:[
      {success_uri:"oss://bucket/part-1.jsonl.zst._SUCCESS",sha256:$catalog,manifest_sha256:$catalog,
       gap_from_previous_ns:0,start_received_at_ns:100,end_received_at_ns:200,agg_trade_count:1,
       lob_capture_session_id:"session-1",lob_reconnect_boundary:true,lob_sequence_gaps:0,
       lob_source_time_rollbacks:0,lob_declared_symbol_count:1200,lob_covered_symbol_count:1200,
       lob_min_source_latency_ms:0,lob_max_source_latency_ms:0,
       lob_min_bid_levels:1,lob_min_ask_levels:1},
      {success_uri:"oss://bucket/part-2.jsonl.zst._SUCCESS",sha256:$catalog,manifest_sha256:$catalog,
       gap_from_previous_ns:0,start_received_at_ns:200,end_received_at_ns:300,agg_trade_count:1,
       lob_capture_session_id:"session-1",lob_reconnect_boundary:false,lob_sequence_gaps:0,
       lob_source_time_rollbacks:0,lob_declared_symbol_count:1200,lob_covered_symbol_count:1200,
       lob_min_source_latency_ms:0,lob_max_source_latency_ms:0,
       lob_min_bid_levels:1,lob_min_ask_levels:1}
    ]}')
usdm_market=$(jq -c '
  .symbol_count = 500
  | .snapshot_ready_count = 500
  | .oss_roundtrip_evidence |= map(
      .lob_declared_symbol_count = 500 | .lob_covered_symbol_count = 500)' \
  <<<"$market_json")
jq -n \
  --arg artifact "$artifact" \
  --arg bundle "$bundle" \
  --arg source "$source_revision" \
  --argjson market "$market_json" \
  --argjson usdm_market "$usdm_market" \
  '{schema:"monday.rust_lob_shadow_gate.v3",candidate_sha256:$artifact,
    deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
    passed:true,production_eligible:true,checks_passed:true,duration_seconds:3600,
    markets:{spot:$market,usdm:$usdm_market}}' \
  >"$tmp_dir/gate.json"

jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null

wrong_bundle=$(printf 'e%.0s' {1..64})
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$wrong_bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different deployment bundle\n' >&2
  exit 1
fi

jq '.markets.spot.oss_roundtrip_evidence[1].lob_capture_session_id = "session-2"' \
  "$tmp_dir/gate.json" >"$tmp_dir/mixed-lob-session.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/mixed-lob-session.json" >/dev/null; then
  printf 'gate policy accepted LOB evidence across a reconnect boundary\n' >&2
  exit 1
fi

jq '.markets.spot.oss_roundtrip_evidence[1] |=
      (.start_received_at_ns = 90000000300
       | .end_received_at_ns = 90000000400
       | .gap_from_previous_ns = 90000000100)
    | .markets.spot.max_segment_gap_ns = 90000000100' \
  "$tmp_dir/gate.json" >"$tmp_dir/excessive-segment-gap.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/excessive-segment-gap.json" >/dev/null; then
  printf 'gate policy accepted a segment gap over the continuity bound\n' >&2
  exit 1
fi

jq 'del(.markets.spot.oss_roundtrip_evidence[0].manifest_sha256)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-manifest-anchor.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-manifest-anchor.json" >/dev/null; then
  printf 'gate policy accepted evidence without a manifest SHA anchor\n' >&2
  exit 1
fi

jq '.markets.usdm.oss_roundtrip_evidence[1].start_received_at_ns = 199' \
  "$tmp_dir/gate.json" >"$tmp_dir/overlapping-agg-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/overlapping-agg-trades.json" >/dev/null; then
  printf 'gate policy accepted overlapping aggregate-trade segments\n' >&2
  exit 1
fi

wrong_artifact=$(printf 'f%.0s' {1..64})
if jq -e \
  --arg candidate_sha256 "$wrong_artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different binary artifact\n' >&2
  exit 1
fi

wrong_source=$(printf '9%.0s' {1..40})
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$wrong_source" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different source revision\n' >&2
  exit 1
fi

jq '.markets.spot.health_samples = 1' "$tmp_dir/gate.json" >"$tmp_dir/short-sampling.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/short-sampling.json" >/dev/null; then
  printf 'gate policy accepted insufficient continuous health samples\n' >&2
  exit 1
fi

for market in spot usdm; do
  jq --arg market "$market" \
    '.markets[$market].max_health_silence_seconds = 91' \
    "$tmp_dir/gate.json" >"$tmp_dir/rotation-jitter-health-$market.json"
  jq -e \
    --arg candidate_sha256 "$artifact" \
    --arg deployment_bundle_sha256 "$bundle" \
    --arg deployment_source_revision "$source_revision" \
    -f "$POLICY" "$tmp_dir/rotation-jitter-health-$market.json" >/dev/null || {
    printf 'gate policy rejected a 91-second %s rotation jitter gap\n' "$market" >&2
    exit 1
  }

  jq --arg market "$market" \
    '.markets[$market].max_health_silence_seconds = 121' \
    "$tmp_dir/gate.json" >"$tmp_dir/stale-health-$market.json"
  if jq -e \
    --arg candidate_sha256 "$artifact" \
    --arg deployment_bundle_sha256 "$bundle" \
    --arg deployment_source_revision "$source_revision" \
    -f "$POLICY" "$tmp_dir/stale-health-$market.json" >/dev/null; then
    printf 'gate policy accepted a %s health freshness gap over 120 seconds\n' "$market" >&2
    exit 1
  fi
done

jq '.markets.spot.agg_trade_count = 0' \
  "$tmp_dir/gate.json" >"$tmp_dir/zero-agg-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/zero-agg-trades.json" >/dev/null; then
  printf 'gate policy accepted zero aggregate trades\n' >&2
  exit 1
fi

jq 'del(.markets.spot.strict_trade_summary_readback)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-strict-trade-summary-readback.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-strict-trade-summary-readback.json" >/dev/null; then
  printf 'gate policy accepted evidence without strict trade-summary readback\n' >&2
  exit 1
fi

jq 'del(.markets.spot.oss_roundtrip_evidence[0].success_uri)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-success-marker.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-success-marker.json" >/dev/null; then
  printf 'gate policy accepted aggregate-trade evidence without a success marker\n' >&2
  exit 1
fi

jq '.markets.usdm.agg_trade_segments = 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/non-continuous-agg-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/non-continuous-agg-trades.json" >/dev/null; then
  printf 'gate policy accepted aggregate trades from fewer than two segments\n' >&2
  exit 1
fi

jq -n '{market:"spot",dataset:"spot_all",status:"synced",sequence_gaps:0,symbol_count:1200,
  snapshot_ready_count:1200,pending_upload_segments:0,queue_saturated:false,
  disk_warning:false,upload_warning:false,updated_at_ns:200,session_id:"new-session"}' \
  >"$tmp_dir/runtime-health.json"
runtime_policy_accepts() {
  local health=$1 old_session=$2 minimum_updated_ns=$3
  local expected_market=${4:-spot} expected_dataset=${5:-spot_all}
  jq -e \
    --arg expected_market "$expected_market" \
    --arg expected_dataset "$expected_dataset" \
    --arg old_session "$old_session" \
    --argjson minimum_symbols 1000 \
    --argjson minimum_updated_ns "$minimum_updated_ns" \
    -f "$RUNTIME_POLICY" "$health" >/dev/null
}
runtime_policy_accepts "$tmp_dir/runtime-health.json" old-session 100
if runtime_policy_accepts "$tmp_dir/runtime-health.json" old-session 200; then
  printf 'runtime policy accepted health that was not newer than restart\n' >&2
  exit 1
fi
if runtime_policy_accepts "$tmp_dir/runtime-health.json" new-session 100; then
  printf 'runtime policy accepted a stale session\n' >&2
  exit 1
fi
for field in symbol_count snapshot_ready_count; do
  jq --arg field "$field" '.[$field] = "1200"' \
    "$tmp_dir/runtime-health.json" >"$tmp_dir/quoted-count.json"
  if runtime_policy_accepts "$tmp_dir/quoted-count.json" old-session 100; then
    printf 'runtime policy accepted quoted %s\n' "$field" >&2
    exit 1
  fi
  jq --arg field "$field" '.[$field] = 1200.5' \
    "$tmp_dir/runtime-health.json" >"$tmp_dir/fractional-count.json"
  if runtime_policy_accepts "$tmp_dir/fractional-count.json" old-session 100; then
    printf 'runtime policy accepted fractional %s\n' "$field" >&2
    exit 1
  fi
done
jq '.market = "usdm"' "$tmp_dir/runtime-health.json" >"$tmp_dir/cross-market.json"
if runtime_policy_accepts "$tmp_dir/cross-market.json" old-session 100; then
  printf 'runtime policy accepted a cross-market health payload\n' >&2
  exit 1
fi
jq '.dataset = "usdm_perpetual_all"' \
  "$tmp_dir/runtime-health.json" >"$tmp_dir/cross-dataset.json"
if runtime_policy_accepts "$tmp_dir/cross-dataset.json" old-session 100; then
  printf 'runtime policy accepted a cross-dataset health payload\n' >&2
  exit 1
fi

rollback_body="$tmp_dir/rollback.sh"
sed -n '/^rollback_after_failure()/,/^}/p' "$CUTOVER" >"$rollback_body"
production_predicate_body="$tmp_dir/production-is-fail-closed.sh"
sed -n '/^production_is_fail_closed()/,/^}/p' "$CUTOVER" \
  >"$production_predicate_body"
start_line=$(grep -n 'systemctl start "${PRODUCTION_UNITS\[@\]}"' "$rollback_body" | tail -1 | cut -d: -f1)
clear_line=$(grep -n 'clear_health_before_restart' "$rollback_body" | cut -d: -f1)
health_line=$(grep -n 'wait_for_release_health' "$rollback_body" | cut -d: -f1)
enable_line=$(grep -n 'systemctl enable "${PRODUCTION_UNITS\[@\]}"' "$rollback_body" | cut -d: -f1)
((clear_line < start_line && start_line < health_line && health_line < enable_line)) || {
  printf 'rollback no longer follows clear stale health -> start -> verify -> enable\n' >&2
  exit 1
}
grep -Fq 'runtime_matches_release "$OLD_BINARY" true' "$rollback_body"
grep -Fq '"$rollback_started_ns"' "$rollback_body"
grep -Fq 'previous-release-health-unverified-disabled' "$rollback_body"
grep -Fq 'systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}"' "$rollback_body"
grep -Fq 'ROLLBACK_RESULT=new-host-containment-failed' "$rollback_body"
grep -Fq 'binance-lob-archiver@spot.service' "$CUTOVER"
grep -Fq 'binance-lob-archiver@usdm.service' "$CUTOVER"
grep -Fq 'legacy collector unit must be disabled before cutover' "$CUTOVER"
grep -Fq 'release_staging=$(mktemp -d "$release_root/.${artifact_sha256}.new.XXXXXX")' \
  "$INSTALL_RELEASE"
grep -Fq 'COPYFILE_DISABLE=1 tar -C "$SCRIPT_DIR" -cf "$BUNDLE_PATH" "${assets[@]}"' \
  "$INSTALL_RELEASE"
grep -Fq 'install -d -m 0755 /opt/monday/releases' "$INSTALL_RELEASE"
grep -Fq 'chmod 0755 "$release_staging"' "$INSTALL_RELEASE"
grep -Fq 'release directory must be traversable with mode 0755' "$INSTALL_RELEASE"
grep -Fq 'runuser -u hftcollector -- "$release_binary" --self-test' "$INSTALL_RELEASE"
grep -Fq 'existing release identity does not match requested artifact, bundle, and source' \
  "$INSTALL_RELEASE"
grep -Fq 'existing release deployment differs from the requested bundle' "$INSTALL_RELEASE"
grep -Fq 'bundle_evidence_dir="$binary_evidence_dir/$deployment_bundle_sha256"' "$GATE"
grep -Fq 'evidence_dir="$runs_dir/$gate_run_id"' "$GATE"
grep -Fq 'an immutable production-eligible gate already exists' "$GATE"
grep -Fq 'for candidate_unit in "${candidate_units[@]}"; do' "$GATE"
grep -Fq 'systemctl reset-failed "$candidate_unit" >/dev/null 2>&1 || true' "$GATE"
if grep -Fq 'rm -f "$gate_json"' "$GATE"; then
  printf 'shadow gate still deletes immutable gate evidence\n' >&2
  exit 1
fi
grep -Fq 'gate_markers=("$GATE_BUNDLE_DIR"/runs/*/PASSED.sha256)' "$CUTOVER"
grep -Fq 'rollback-deployment.sha256' "$CUTOVER"
grep -Fq 'ROLLBACK_DEPLOYMENT_MANIFEST_SHA256' "$rollback_body"
grep -Fq 'installed production asset drifted from the active immutable release' "$CUTOVER"
grep -Fq 'cmp -s -- "$source" "$installed_source"' "$CUTOVER"
grep -Fq 'mkdir -m 0750 -- "$EVIDENCE_DIR"' "$CUTOVER"
grep -Fq 'mkdir -m 0750 -- "$evidence_dir"' "$GATE"
grep -Fq '\( -type f -o -type l \)' "$GATE"
grep -Fxq 'TimeoutStartSec=0' "$SCRIPT_DIR/binance-lob-archiver-upload@.service"
grep -Fxq 'TimeoutStartSec=0' "$SCRIPT_DIR/binance-lob-archiver-rust-upload@.service"

candidate_start_body="$tmp_dir/candidate-start.sh"
sed -n '/^STEP=clear-stale-candidate-health/,/^STEP=write-cutover-evidence/p' \
  "$CUTOVER" >"$candidate_start_body"
candidate_clear_line=$(grep -n '^clear_health_before_restart' "$candidate_start_body" | cut -d: -f1)
candidate_timestamp_line=$(grep -n '^CANDIDATE_STARTED_NS=' "$candidate_start_body" | cut -d: -f1)
candidate_start_line=$(grep -n 'systemctl start "${PRODUCTION_UNITS\[@\]}"' \
  "$candidate_start_body" | cut -d: -f1)
candidate_health_line=$(grep -n '^wait_for_release_health' "$candidate_start_body" | cut -d: -f1)
candidate_enable_line=$(grep -n 'systemctl enable "${PRODUCTION_UNITS\[@\]}"' \
  "$candidate_start_body" | cut -d: -f1)
((candidate_clear_line < candidate_timestamp_line \
  && candidate_timestamp_line < candidate_start_line \
  && candidate_start_line < candidate_health_line \
  && candidate_health_line < candidate_enable_line)) || {
  printf 'candidate no longer follows clear stale health -> timestamp -> start -> verify -> enable\n' >&2
  exit 1
}
grep -Fq '"$CANDIDATE_STARTED_NS"' "$candidate_start_body"

# Execute the rollback snapshot logic against isolated fixture roots. This catches
# content drift and manifest-tamper regressions that static contract greps miss.
installed_root="$tmp_dir/installed"
release_deployment="$tmp_dir/old-release/deployment"
stage_body="$tmp_dir/stage-existing-deployment.sh"
mkdir -p "$installed_root/systemd" "$installed_root/monday" "$release_deployment"
sed -n '/^stage_existing_deployment_for_rollback()/,/^}/p' "$CUTOVER" \
  | sed \
      -e "s#/etc/systemd/system#$installed_root/systemd#g" \
      -e "s#/etc/monday#$installed_root/monday#g" \
  >"$stage_body"
deployment_assets=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
)
for asset in "${deployment_assets[@]}"; do
  case "$asset" in
    *.service) installed="$installed_root/systemd/$asset" ;;
    *.env) installed="$installed_root/monday/$asset" ;;
  esac
  printf 'fixture:%s\n' "$asset" >"$release_deployment/$asset"
  install -m 0644 "$release_deployment/$asset" "$installed"
done

run_stage_fixture() (
  DEPLOYMENT_ASSETS=("${deployment_assets[@]}")
  OLD_DEPLOYMENT="$release_deployment"
  EVIDENCE_DIR=$1
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=
  fail() { printf '%s\n' "$*" >&2; exit 1; }
  validate_deployment() { return 0; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  atomic_install() { install -m "$1" "$2" "$3"; }
  # shellcheck disable=SC1090
  . "$stage_body"
  stage_existing_deployment_for_rollback
)

snapshot_evidence="$tmp_dir/snapshot-evidence"
mkdir -p "$snapshot_evidence"
run_stage_fixture "$snapshot_evidence"
(
  cd "$snapshot_evidence/rollback-deployment"
  sha256sum --check --strict "$snapshot_evidence/rollback-deployment.sha256" >/dev/null
)
printf 'tampered\n' >> \
  "$snapshot_evidence/rollback-deployment/binance-lob-archiver-production@.service"
if (
  cd "$snapshot_evidence/rollback-deployment"
  sha256sum --check --strict "$snapshot_evidence/rollback-deployment.sha256" >/dev/null 2>&1
); then
  printf 'rollback manifest accepted a tampered snapshot\n' >&2
  exit 1
fi

printf 'drifted\n' >>"$installed_root/monday/binance-lob-archiver-production-spot.env"
drift_evidence="$tmp_dir/drift-evidence"
mkdir -p "$drift_evidence"
if run_stage_fixture "$drift_evidence" >"$tmp_dir/drift.out" 2>&1; then
  printf 'rollback snapshot accepted installed configuration drift\n' >&2
  exit 1
fi
grep -Fq 'installed production asset drifted from the active immutable release' \
  "$tmp_dir/drift.out"

run_new_host_rollback_fixture() (
  local active_unit=${1:-} unit
  PRODUCTION_UNITS=(production-spot production-usdm)
  UPLOAD_UNITS=(upload-spot upload-usdm)
  LEGACY_UNITS=(legacy-spot legacy-usdm)
  TRANSITION_MASK_UNITS=("${PRODUCTION_UNITS[@]}" "${UPLOAD_UNITS[@]}" "${LEGACY_UNITS[@]}")
  CANONICAL_SPOOL="$tmp_dir/nonexistent-spool"
  CANDIDATE_DEPLOYMENT="$tmp_dir/candidate-deployment"
  CANDIDATE_BINARY="$tmp_dir/candidate-binary"
  PRODUCTION_LINK="$tmp_dir/nonexistent-production-link"
  OLD_MODE=new-host
  ROLLBACK_RESULT=
  systemctl() {
    case "$1" in
      is-active)
        unit=${!#}
        [[ -n $active_unit && $unit == "$active_unit" ]]
        ;;
      is-enabled)
        unit=${!#}
        if [[ ${2:-} == --quiet ]]; then
          return 1
        fi
        printf 'masked-runtime\n'
        return 1
        ;;
      *) return 0 ;;
    esac
  }
  copy_health_evidence() { return 0; }
  run_candidate_drain() { return 0; }
  # shellcheck disable=SC1090
  . "$production_predicate_body"
  # shellcheck disable=SC1090
  . "$rollback_body"
  rollback_after_failure
  printf '%s\n' "$ROLLBACK_RESULT"
)

[[ $(run_new_host_rollback_fixture) == new-host-disabled ]]
[[ $(run_new_host_rollback_fixture legacy-spot) == new-host-containment-failed ]]
[[ $(run_new_host_rollback_fixture upload-usdm) == new-host-containment-failed ]]

mock_bin="$tmp_dir/bin"
mock_state="$tmp_dir/mock-state"
mkdir -p "$mock_bin" "$mock_state"
cat >"$mock_bin/aliyun" <<'MOCK_ALIYUN'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$MOCK_STATE_DIR/calls.log"
case "${1:-} ${2:-}" in
  'ecs RunCommand')
    printf '{"InvokeId":"mock-invoke"}\n'
    ;;
  'ecs DescribeInvocationResults')
    if [[ ${MOCK_TRANSIENT_ONCE:-0} == 1 && ! -f $MOCK_STATE_DIR/transient-seen ]]; then
      : >"$MOCK_STATE_DIR/transient-seen"
      exit 1
    elif [[ -f $MOCK_STATE_DIR/stopped && ${MOCK_IGNORE_STOP:-0} != 1 ]]; then
      status=Stopped
      exit_code=-1
    else
      status=${MOCK_STATUS:-Success}
      exit_code=${MOCK_EXIT_CODE:-0}
    fi
    printf '{"Invocation":{"InvocationStatus":"%s","ExitCode":"%s"}}\n' \
      "$status" "$exit_code"
    ;;
  'ecs StopInvocation')
    : >"$MOCK_STATE_DIR/stopped"
    printf '{}\n'
    ;;
  *)
    printf 'unexpected aliyun call: %s\n' "$*" >&2
    exit 2
    ;;
esac
MOCK_ALIYUN
cat >"$mock_bin/sleep" <<'MOCK_SLEEP'
#!/usr/bin/env bash
exit 0
MOCK_SLEEP
chmod +x "$mock_bin/aliyun" "$mock_bin/sleep"

common_env=(
  PATH="$mock_bin:$PATH"
  MOCK_STATE_DIR="$mock_state"
  ACTION=gate
  INSTANCE_ID=i-test123
  ARTIFACT_SHA256="$artifact"
  MONDAY_ALLOW_SHORT_OPERATION_TEST=1
  MONDAY_OPERATION_TEST_POLLS=2
  MONDAY_OPERATION_TEST_CANCEL_POLLS=2
)

run_commands_before=$(grep -c 'ecs RunCommand' "$mock_state/calls.log" 2>/dev/null || true)
if env \
  PATH="$mock_bin:$PATH" \
  MOCK_STATE_DIR="$mock_state" \
  ACTION=cutover \
  INSTANCE_ID=i-test123 \
  ARTIFACT_SHA256="$artifact" \
  MONDAY_OPERATION_TEST_POLLS=invalid \
  "$INVOKE" >"$tmp_dir/preflight.out" 2>&1; then
  printf 'operation wrapper accepted unauthorized test polling parameters\n' >&2
  exit 1
fi
run_commands_after=$(grep -c 'ecs RunCommand' "$mock_state/calls.log" 2>/dev/null || true)
[[ $run_commands_after == "$run_commands_before" ]] || {
  printf 'operation wrapper launched a remote command before validating test parameters\n' >&2
  exit 1
}

env "${common_env[@]}" MOCK_STATUS=Success MOCK_EXIT_CODE=0 "$INVOKE" \
  >"$tmp_dir/success.out"
grep -Fq 'gate completed successfully: mock-invoke' "$tmp_dir/success.out"

rm -f "$mock_state/stopped" "$mock_state/transient-seen"
env "${common_env[@]}" MOCK_TRANSIENT_ONCE=1 MOCK_STATUS=Success MOCK_EXIT_CODE=0 \
  "$INVOKE" >"$tmp_dir/transient.out"
grep -Fq 'gate completed successfully: mock-invoke' "$tmp_dir/transient.out"

rm -f "$mock_state/stopped"
if env "${common_env[@]}" MOCK_STATUS=PartialFailed MOCK_EXIT_CODE=1 "$INVOKE" \
  >"$tmp_dir/failed.out" 2>&1; then
  printf 'operation wrapper accepted PartialFailed\n' >&2
  exit 1
fi

rm -f "$mock_state/stopped"
if env "${common_env[@]}" MOCK_STATUS=Running MOCK_EXIT_CODE=0 "$INVOKE" \
  >"$tmp_dir/timeout.out" 2>&1; then
  printf 'operation wrapper accepted a locally timed-out invocation\n' >&2
  exit 1
fi
grep -Fq 'ecs StopInvocation' "$mock_state/calls.log"
grep -Fq 'invocation reached terminal state after cancellation: Stopped' "$tmp_dir/timeout.out"

rm -f "$mock_state/stopped"
if env "${common_env[@]}" MOCK_STATUS=Running MOCK_IGNORE_STOP=1 "$INVOKE" \
  >"$tmp_dir/unconfirmed.out" 2>&1; then
  printf 'operation wrapper accepted an unconfirmed cancellation\n' >&2
  exit 1
fi
grep -Fq 'invocation did not confirm cancellation' "$tmp_dir/unconfirmed.out"

printf 'Rust collector control-plane contracts passed\n'
