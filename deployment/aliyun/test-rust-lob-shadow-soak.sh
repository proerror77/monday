#!/usr/bin/env bash
# Literal contract strings intentionally contain shell syntax.
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
SOAK="$SCRIPT_DIR/host-rust-lob-shadow-soak.sh"
UNIT="$SCRIPT_DIR/binance-lob-archiver-rust@.service"
INSTALLER="$SCRIPT_DIR/deploy-rust-lob-release.sh"

[[ -x $SOAK ]]
[[ $($SOAK --self-test) == 'shadow-soak self-test: ok' ]]

grep -Fxq 'EnvironmentFile=-/run/monday/binance-lob-archiver-rust-%i-soak.env' "$UNIT"
grep -Fq 'host-rust-lob-shadow-soak.sh' "$INSTALLER"
grep -Fq '/opt/monday/bin/monday-rust-lob-shadow-soak' "$INSTALLER"
grep -Fq 'asset=host-rust-lob-shadow-soak.sh' "$SOAK"
grep -Fq 'installed_asset=/opt/monday/bin/monday-rust-lob-shadow-soak' "$SOAK"
grep -Fq 'for command in aliyun awk chmod chown' "$SOAK"
grep -Fq 'trap cleanup_partial EXIT' "$SOAK"
grep -Fq 'rm -rf -- "$tmp_dir"' "$SOAK"

grep -Fq 'RUN_SPOOL_ROOT=/data/monday/spool/binance-lob-rust-shadow/runs' "$SOAK"
grep -Fq 'assert_no_symlink_ancestors "$run_spool_path"' "$SOAK"
grep -Fq 'formal_gate:false,cutover:false,live:false' "$SOAK"
grep -Fq 'receipt.json' "$SOAK"
grep -Fq 'readback_start_ns[$market]=$updated_ns' "$SOAK"
grep -Fq 'updated_ns >= ${recovery_started_ns[$market]}' "$SOAK"
grep -Fq 'TOTAL_FEED_SECONDS=$((SOAK_SECONDS + HEALTH_SETTLE_SECONDS + 300))' "$SOAK"
grep -Fq 'MIN_SOAK_SECONDS=1201' "$SOAK"
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
if grep -Eq 'readonly[[:space:]]+(INSTANCE_ID|EXPECTED_PRODUCTION_[A-Za-z0-9_]*)[[:space:]]*=' "$SOAK" \
  || grep -Eq 'readonly CANDIDATE_SHA256=[a-f0-9]{64}$' "$SOAK"; then
  printf 'shadow-soak still hardcodes a run or production identity\n' >&2
  exit 1
fi

stop_line=$(grep -n 'if ! stop_primaries_and_wait; then' "$SOAK" | cut -d: -f1)
override_cleanup_line=$(grep -n 'rm -f -- "${override_file\[$market\]}"' "$SOAK" | cut -d: -f1)
((stop_line < override_cleanup_line))

printf 'Rust LOB shadow-soak contract passed\n'
