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
# Package the controller from the same verified binaries; this target contains
# no Rust compiler. Its scripts and Job template come from this exact checkout.
controller_context="$context/controller"
mkdir -p "$controller_context/rust_hft" "$controller_context/deployment/aliyun/research/scripts" \
  "$controller_context/deployment/aliyun/research/k8s"
cp -R "$release/research-bin" "$controller_context/rust_hft/research-bin"
cp "$root/deployment/aliyun/research/Dockerfile.campaign-cycle-controller" "$controller_context/Dockerfile"
for asset in cex-materialization-entrypoint.sh campaign-cycle-controller.sh campaign-job-watch.sh; do
  cp "$root/deployment/aliyun/research/scripts/$asset" "$controller_context/deployment/aliyun/research/scripts/$asset"
done
cp "$root/deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml" \
  "$controller_context/deployment/aliyun/research/k8s/"
source_sha=$(git -C "$root" rev-parse HEAD)
controller_image="monday-controller-ci:$GITHUB_RUN_ID"
docker build --target prebuilt --label "org.opencontainers.image.revision=$source_sha" \
  -t "$controller_image" "$controller_context"
bash "$root/.github/scripts/verify-research-controller-image.sh" "$controller_image" "$source_sha" "$release/research-bin"
# Platform CLIs reject missing explicit configuration before any connection.
for binary in researchctl research-orchestrator research-prepare; do
  if docker run --rm --network none --entrypoint "/usr/local/bin/$binary" monday-research-ci:"$GITHUB_RUN_ID"; then
    echo 'platform command accepted implicit production configuration' >&2; exit 1
  fi
done
