#!/usr/bin/env bash
set -euo pipefail

directory=${1:?expected research-runner binary directory}
expected=(
  hft-backtest
  alpha-harness
  lob-pit-materializer
  binance-market-tape-slicer
  binance-replay-parquet-materializer
  research-orchestrator
  researchctl
  research-prepare
  clickhouse-analytics-materializer
  monday-prediction-research
  monday-prediction-evaluator
  monday-prediction-snapshot
)

fail() { printf 'research release validation: %s\n' "$*" >&2; exit 1; }
test -d "$directory" || fail "binary directory missing: $directory"
test "$(find "$directory" -mindepth 1 -maxdepth 1 -print | wc -l)" -eq "${#expected[@]}" || fail 'unexpected binary file count'

for binary in "${expected[@]}"; do
  test -f "$directory/$binary" && test ! -L "$directory/$binary" || fail "regular binary missing: $binary"
  test -x "$directory/$binary" || fail "executable mode lost: $binary; transport release bytes in the verified tar bundle"
done
