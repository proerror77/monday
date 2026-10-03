#!/usr/bin/env bash
# CI builds bytes and provenance; it never acquires research compute or runs a
# Campaign. Release admission and authorized deployment remain separate.
set -euo pipefail
cd "$(dirname "$0")/../../rust_hft"
source_sha=$(git rev-parse HEAD)
export MONDAY_RELEASE_JOB_ID
MONDAY_RELEASE_JOB_ID=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" --jq '.jobs|map(select(.name=="Research image binaries" or .name=="Research release binaries"))|if length==1 then .[0].id else error("ambiguous release producer") end')
export MONDAY_SOURCE_REVISION=$source_sha
cargo build --release --locked -p hft-backtest -p alpha-harness
cargo build --release --locked -p hft-collector \
  --bin lob-pit-materializer --bin binance-market-tape-slicer \
  --bin binance-replay-parquet-materializer --bin clickhouse-analytics-materializer
cargo build --release --locked -p hft-research-platform --features control --bins
cargo build --manifest-path prediction-markets/Cargo.toml --release --locked \
  -p ploy-research --features db --bin monday-prediction-research \
  --bin monday-prediction-evaluator --bin monday-prediction-snapshot
release=${RUNNER_TEMP:?}/research-release
mkdir -p "$release/research-bin"
for binary in hft-backtest alpha-harness lob-pit-materializer binance-market-tape-slicer \
  binance-replay-parquet-materializer clickhouse-analytics-materializer research-orchestrator researchctl research-prepare; do
  install -m 0755 "target/release/$binary" "$release/research-bin/$binary"
done
for binary in monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  install -m 0755 "prediction-markets/target/release/$binary" "$release/research-bin/$binary"
done
../.github/scripts/research-image-release-artifact.sh create "$release" "$source_sha" "$GITHUB_RUN_ID" .
