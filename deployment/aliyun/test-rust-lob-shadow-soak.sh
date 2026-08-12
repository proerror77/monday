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

grep -Fq 'RUN_SPOOL_ROOT=/data/monday/spool/binance-lob-rust-shadow/runs' "$SOAK"
grep -Fq 'formal_gate:false,cutover:false,live:false' "$SOAK"
grep -Fq 'receipt.json' "$SOAK"
grep -Fq 'readback_start_ns[$market]=$updated_ns' "$SOAK"
grep -Fq 'updated_ns >= ${recovery_started_ns[$market]}' "$SOAK"
grep -Fq 'TOTAL_FEED_SECONDS=$((SOAK_SECONDS + HEALTH_SETTLE_SECONDS + 300))' "$SOAK"
if grep -Eq 'readonly (INSTANCE_ID|EXPECTED_PRODUCTION_)=' "$SOAK" \
  || grep -Eq 'readonly CANDIDATE_SHA256=[a-f0-9]{64}$' "$SOAK"; then
  printf 'shadow-soak still hardcodes a run or production identity\n' >&2
  exit 1
fi

stop_line=$(grep -n 'if ! stop_primaries_and_wait; then' "$SOAK" | cut -d: -f1)
override_cleanup_line=$(grep -n 'rm -f -- "${override_file\[$market\]}"' "$SOAK" | cut -d: -f1)
((stop_line < override_cleanup_line))

printf 'Rust LOB shadow-soak contract passed\n'
