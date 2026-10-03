#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
release=${1:?release directory required}
"$root/.github/scripts/research-image-release-artifact.sh" verify "$release" "$(git -C "$root" rev-parse HEAD)" "$GITHUB_RUN_ID" "$root/rust_hft"
context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
cp -R "$release/research-bin" "$context/research-bin"
cp "$root/rust_hft/deployment/docker/Dockerfile.research" "$context/Dockerfile"
docker build --target prebuilt -t monday-research-ci:"$GITHUB_RUN_ID" "$context"
for binary in hft-backtest alpha-harness lob-pit-materializer binance-market-tape-slicer \
  binance-replay-parquet-materializer clickhouse-analytics-materializer \
  monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  docker run --rm --network none --entrypoint "/usr/local/bin/$binary" monday-research-ci:"$GITHUB_RUN_ID" --help >/dev/null
done
# Platform CLIs reject missing explicit configuration before any connection.
for binary in researchctl research-orchestrator research-prepare; do
  if docker run --rm --network none --entrypoint "/usr/local/bin/$binary" monday-research-ci:"$GITHUB_RUN_ID"; then
    echo 'platform command accepted implicit production configuration' >&2; exit 1
  fi
done
