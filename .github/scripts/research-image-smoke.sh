#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
release=${1:?release directory required}
product=$(bash "$(dirname "${BASH_SOURCE[0]}")/research-release-products.sh" normalize "${2:-all}")
"$root/.github/scripts/research-image-release-artifact.sh" verify "$release" "$(git -C "$root" rev-parse HEAD)" "$GITHUB_RUN_ID" "$root/rust_hft" "$GITHUB_RUN_ATTEMPT" "${MONDAY_RELEASE_JOB_ID:-}" "$product"
context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
source_sha=$(git -C "$root" rev-parse HEAD)
for runner in cex-runner prediction-runner; do
  if ! bash "$root/.github/scripts/research-release-products.sh" contains "$product" "$runner"; then continue; fi
  runner_context="$context/$runner"
  mkdir -p "$runner_context/research-bin"
  while IFS= read -r binary; do
    install -m 0755 "$release/research-bin/$binary" "$runner_context/research-bin/$binary"
  done < <(bash "$root/.github/scripts/research-release-products.sh" binaries "$runner")
  if [[ $runner == cex-runner ]]; then dockerfile=Dockerfile.research; else dockerfile=Dockerfile.prediction-research; fi
  cp "$root/rust_hft/deployment/docker/$dockerfile" "$runner_context/Dockerfile"
  image="monday-$runner-ci:$GITHUB_RUN_ID"
  docker build --target prebuilt --label "org.opencontainers.image.revision=$source_sha" -t "$image" "$runner_context"
  bash "$root/.github/scripts/verify-research-product-image.sh" "$image" "$source_sha" "$release/research-bin" "$runner"
done
if bash "$root/.github/scripts/research-release-products.sh" contains "$product" controller; then
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
bash "$root/.github/scripts/verify-research-product-image.sh" "$controller_image" "$source_sha" "$release/research-bin" controller
fi
