#!/usr/bin/env bash
# Literal contract strings intentionally contain shell syntax.
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
SOAK="$SCRIPT_DIR/host-rust-lob-shadow-soak.sh"
PREFLIGHT="$SCRIPT_DIR/host-rust-lob-shadow-preflight.sh"
UNIT="$SCRIPT_DIR/binance-lob-archiver-rust@.service"
INSTALLER="$SCRIPT_DIR/deploy-rust-lob-release.sh"

[[ -x $SOAK ]]
[[ -x $PREFLIGHT ]]
[[ $($SOAK --self-test) == 'shadow-soak self-test: ok' ]]
[[ $($PREFLIGHT --self-test) == 'shadow-preflight self-test: ok' ]]

grep -Fxq 'EnvironmentFile=-/run/monday/binance-lob-archiver-rust-%i-soak.env' "$UNIT"
grep -Fq 'host-rust-lob-shadow-soak.sh' "$INSTALLER"
grep -Fq '/opt/monday/bin/monday-rust-lob-shadow-soak' "$INSTALLER"
grep -Fq 'host-rust-lob-shadow-preflight.sh' "$INSTALLER"
grep -Fq '/opt/monday/bin/monday-rust-lob-shadow-preflight' "$INSTALLER"
grep -Fq 'asset=host-rust-lob-shadow-soak.sh' "$SOAK"
grep -Fq 'installed_asset=/opt/monday/bin/monday-rust-lob-shadow-soak' "$SOAK"
grep -Fq 'for command in aliyun awk chmod chown' "$SOAK"
grep -Fq 'trap cleanup_partial EXIT' "$SOAK"
grep -Fq 'rm -rf -- "$tmp_dir"' "$SOAK"

grep -Fq 'RUN_SPOOL_ROOT=/data/monday/spool/binance-lob-rust-shadow/runs' "$SOAK"
grep -Fq 'install -d -m 0755 -o root -g root' "$SOAK"
grep -Fq '"$RUN_SPOOL_ROOT" "$RUN_SPOOL_ROOT/$CANDIDATE_SHA256"' "$SOAK"
grep -Fq '"$RUN_SPOOL_ROOT/$CANDIDATE_SHA256/$evidence_run_id"' "$SOAK"
grep -Fq 'assert_no_symlink_ancestors "$run_spool_path"' "$SOAK"
grep -Fq 'formal_gate:false,cutover:false,live:false' "$SOAK"
grep -Fq 'receipt.json' "$SOAK"
grep -Fq 'readback_start_ns[$market]=$updated_ns' "$SOAK"
grep -Fq 'updated_ns >= ${recovery_started_ns[$market]}' "$SOAK"
grep -Fxq 'readonly BOOTSTRAP_SETTLE_SECONDS=900' "$SOAK"
grep -Fxq 'readonly RECOVERY_SETTLE_SECONDS=300' "$SOAK"
grep -Fq 'TOTAL_FEED_SECONDS=$((SOAK_SECONDS + BOOTSTRAP_SETTLE_SECONDS + 300))' "$SOAK"
grep -Fq 'current_mono - ${recovery_started_mono[$market]} > RECOVERY_SETTLE_SECONDS' "$SOAK"
grep -Fq '${recovery_started_mono[$market]} + RECOVERY_SETTLE_SECONDS > ready_deadline' "$SOAK"
grep -Fq 'ready_deadline=$(( $(monotonic_seconds) + BOOTSTRAP_SETTLE_SECONDS ))' "$SOAK"
grep -Fxq 'readonly CORRECTNESS_SECONDS=300' "$SOAK"
grep -Fxq 'readonly CORRECTNESS_SEGMENT_SECONDS=90' "$SOAK"
grep -Fq 'CORRECTNESS_SECONDS >= 3 * CORRECTNESS_SEGMENT_SECONDS' "$SOAK"
grep -Fq -- '--correctness' "$SOAK"
grep -Fq 'RUN_MODE=correctness' "$SOAK"
grep -Fq 'RUN_MODE=stability' "$SOAK"
grep -Fq 'MIN_STABILITY_SOAK_SECONDS=1201' "$SOAK"
grep -Fq 'SOAK_SECONDS=${2:-1800}' "$SOAK"
grep -Fq 'required_segments=$MIN_READBACK_SEGMENTS' "$SOAK"
grep -Fxq 'readonly MIN_READBACK_SEGMENTS=2' "$SOAK"
grep -Fq 'tail -n "$required_segments"' "$SOAK"
grep -Fq 'SEGMENT_SECONDS=%s' "$SOAK"
grep -Fq 'run_mode:$run_mode' "$SOAK" || grep -Fq 'run_mode:$RUN_MODE' "$SOAK"
grep -Fq 'preflight_receipt_sha256' "$SOAK"
grep -Fq '.build_id == $build' "$SOAK"
if grep -Fq 'HEALTH_SETTLE_SECONDS' "$SOAK"; then
  printf 'shadow-soak retains the obsolete shared settle timeout\n' >&2
  exit 1
fi
grep -Fq 'feed_deadline=$(( $(monotonic_seconds) + TOTAL_FEED_SECONDS ))' "$SOAK"
grep -Fq 'MIN_STABILITY_SOAK_SECONDS=1201' "$SOAK"
grep -Fq 'MIN_READBACK_SEGMENTS=2' "$SOAK"
grep -Fq "sed -n '1,100p'" "$SOAK"
grep -Fq 'if [[ $token == *.manifest.json && $token == lake/* ]]; then' "$SOAK"
grep -Fq 'install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$segment_dir"' "$SOAK"
grep -Fq -- '--uid="$SERVICE_USER"' "$SOAK"
grep -Fq -- '--gid="$SERVICE_USER"' "$SOAK"
grep -Fq 'journal-prestop-tail.txt' "$SOAK"
grep -Fq 'transport reset after the final health sample' "$SOAK"
grep -Fq 'journal-precleanup.txt' "$SOAK"
grep -Fq 'capture_final_producer_diagnostics precleanup' "$SOAK"
grep -Fq -- '--verify-raw-trade-continuity' "$SOAK"
grep -Fq 'monday.rust_lob_shadow_preflight.v1' "$SOAK"
grep -Fq 'sealed_triplets' "$PREFLIGHT"
grep -Fq ' -ge 1 ]]' "$PREFLIGHT"
grep -Fq 'tail -n 1' "$PREFLIGHT"
grep -Fq 'replay_identity_sha256' "$PREFLIGHT"
grep -Fq 'strict_verifier' "$PREFLIGHT"
grep -Fq 'checks:{candidate_identity:true,sealed_triplets:true,strict_verifier:true' "$PREFLIGHT"
grep -Fq 'sealed-triplet LOB continuity verification failed' "$PREFLIGHT"
grep -Fq 'run_strict_verifier --require-lob-continuity "${verifier_args[@]}"' "$PREFLIGHT"
if grep -Fq 'run_strict_verifier "${verifier_args[@]}"' "$PREFLIGHT"; then
  printf 'shadow preflight still invokes the trade-summary verifier unconditionally\n' >&2
  exit 1
fi
grep -Fq 'triplet_identity_sha256' "$PREFLIGHT"
grep -Fq 'TRIPLET_ROOT_CANONICAL' "$PREFLIGHT"
grep -Fq 'assert_no_symlink_ancestors' "$PREFLIGHT"
grep -Fq 'EXPECTED_REPLAY_IDENTITY_SHA256' "$PREFLIGHT"
grep -Fq 'Usage: monday-rust-lob-shadow-preflight <candidate-sha256> <sealed-triplet-root> <expected-replay-identity-sha256>' "$PREFLIGHT"
grep -Fq 'expected_replay_identity_sha256' "$PREFLIGHT"
grep -Fq 'one or more sealed triplets per market' "$PREFLIGHT"
grep -Fq 'independently verifies one or more latest Spot and USD-M' "$SCRIPT_DIR/README.md"
grep -Fq 'REPLAY_IDENTITY_BEFORE' "$PREFLIGHT"
grep -Fq 'REPLAY_IDENTITY_AFTER' "$PREFLIGHT"
grep -Fq 'systemd-run --quiet --wait --collect' "$PREFLIGHT"
grep -Fq -- '--property=TimeoutStartSec=300' "$PREFLIGHT"
grep -Fq -- '--property=MemoryHigh=5000M' "$PREFLIGHT"
grep -Fq -- '--property=MemoryMax=6400M' "$PREFLIGHT"
grep -Fq -- '--uid="$SERVICE_USER"' "$PREFLIGHT"
grep -Fq -- 'binance-lob-archiver $SOURCE_REVISION' "$PREFLIGHT"
if grep -Eq 'runuser --user "\$SERVICE_USER" -- "\$candidate_binary" --(require-lob|verify-(aggregate|raw)-trade)-continuity' "$PREFLIGHT"; then
  printf 'preflight continuity verifier bypasses the bounded systemd helper\n' >&2
  exit 1
fi
grep -Fq 'PREFLIGHT_EVIDENCE_ROOT' "$SOAK"
grep -Fq 'preflight_receipt_canonical' "$SOAK"
grep -Fq 'preflight receipt path is not canonical' "$SOAK"
grep -Fq 'preflight_triplet_root' "$SOAK"
grep -Fq 'preflight_replay_identity' "$SOAK"
grep -Fq 'triplet_identity_sha256' "$SOAK"
grep -Fq 'preflight_replay_identity=$(find' "$SOAK"
grep -Fq 'preflight_run_id' "$SOAK"
grep -Fq '.run_id == $run_id' "$SOAK"
if grep -Eq 'checks\.(event_time|upload_contract)' "$PREFLIGHT" "$SOAK"; then
  printf 'shadow preflight claims parser/upload checks it does not perform\n' >&2
  exit 1
fi
if grep -Eq 'readonly[[:space:]]+(INSTANCE_ID|EXPECTED_PRODUCTION_[A-Za-z0-9_]*)[[:space:]]*=' "$SOAK" \
  || grep -Eq 'readonly CANDIDATE_SHA256=[a-f0-9]{64}$' "$SOAK"; then
  printf 'shadow-soak still hardcodes a run or production identity\n' >&2
  exit 1
fi

stop_line=$(grep -n 'if ! stop_primaries_and_wait; then' "$SOAK" | cut -d: -f1)
override_cleanup_line=$(grep -n 'rm -f -- "${override_file\[$market\]}"' "$SOAK" | cut -d: -f1)
((stop_line < override_cleanup_line))

ready_loop_line=$(grep -n '^while ! health_passes spot || ! health_passes usdm; do$' "$SOAK" | cut -d: -f1)
observation_start_line=$(grep -n '^observation_started_ns=$(date +%s%N)$' "$SOAK" | cut -d: -f1)
soak_deadline_line=$(grep -n '^soak_deadline=\$(( \$(monotonic_seconds) + SOAK_SECONDS ))$' "$SOAK" | cut -d: -f1)
((ready_loop_line < observation_start_line && observation_start_line < soak_deadline_line))

replay_before_line=$(grep -n '^REPLAY_IDENTITY_BEFORE=' "$PREFLIGHT" | cut -d: -f1)
verify_spot_line=$(grep -n '^verify_market spot ' "$PREFLIGHT" | cut -d: -f1)
replay_after_line=$(grep -n '^REPLAY_IDENTITY_AFTER=' "$PREFLIGHT" | cut -d: -f1)
((replay_before_line < verify_spot_line && verify_spot_line < replay_after_line))

printf 'Rust LOB shadow-soak contract passed\n'
