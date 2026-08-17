#!/usr/bin/env bash
set -euo pipefail

directory=${1:?expected research-runner binary directory}
expected=(
  hft-backtest
  alpha-harness
  lob-pit-materializer
  binance-market-tape-slicer
  binance-replay-parquet-materializer
  monday-prediction-research
  monday-prediction-evaluator
  monday-prediction-snapshot
)

test -d "$directory"
test "$(find "$directory" -mindepth 1 -maxdepth 1 -print | wc -l)" -eq "${#expected[@]}"

for binary in "${expected[@]}"; do
  test -f "$directory/$binary"
  test -x "$directory/$binary"
done
