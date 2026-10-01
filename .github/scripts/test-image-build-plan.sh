#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
plan() { printf '%s\n' "$4" | bash "$root/image-build-plan.sh" "$1" "$2" "$3" "${5:-push}"; }
[[ $(plan false false false 'rust_hft/research-core/README.md' | jq '.include|length') == 0 ]]
[[ $(plan false false false '.github/workflows/docker-publish.yml' | jq '.include|length') == 0 ]]
[[ $(plan false false true '' | jq -r '.include[].name') == deploy-collector ]]
[[ $(plan false true false '' | jq -r '.include[].name') == deploy-paper ]]
[[ $(plan true false false '' | jq '.include|length') == 3 ]]
[[ $(plan false false false 'rust_hft/deployment/docker/Dockerfile.trading' | jq -r '.include[].name') == hft-trading ]]
[[ $(plan false false false 'deploy/Dockerfile.hft' | jq '.include|length') == 3 ]]
[[ $(plan false false false '' workflow_dispatch | jq '.include|length') == 5 ]]
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for scenario in collector live research policy core root-ignore; do
  case $scenario in
    root-ignore) path=.dockerignore ;;
    core) path=rust_hft/docker/Dockerfile ;;
    collector) path=rust_hft/tools/collector/src/lib.rs ;;
    live) path=rust_hft/apps/live/src/main.rs ;;
    research) path=rust_hft/alpha-harness/app/src/main.rs ;;
    policy) path=.github/workflows/docker-publish.yml ;;
  esac
  printf '%s\n' "$path" >"$tmp/paths"
  GITHUB_REF=refs/heads/main bash "$root/select-rust-ci-scope.sh" --event push --changed-files "$tmp/paths" --metadata "$root/fixtures/rust-ci-scope/metadata.fixture" --output "$tmp/$scenario"
done
[[ $(sed -n 's/^image_matrix=//p' "$tmp/collector" | jq -r '.include[].name') == deploy-collector ]]
[[ $(sed -n 's/^image_matrix=//p' "$tmp/live" | jq '.include|length') == 3 ]]
[[ $(sed -n 's/^image_matrix=//p' "$tmp/core" | jq -r '.include[].name') == hft-core ]]
[[ $(sed -n 's/^image_matrix=//p' "$tmp/root-ignore" | jq '.include|length') == 3 ]]
for scenario in research policy; do
  [[ $(sed -n 's/^image_matrix=//p' "$tmp/$scenario" | jq '.include|length') == 0 ]]
  if grep '^security_jobs=' "$tmp/$scenario" | grep -q 'container-scan'; then exit 1; fi
done
printf 'PASS: source dependencies and recipe paths choose individual images; research/policy changes do not build unrelated runtime images\n'

for scenario in experiment-config embedded-controller; do
  if [[ $scenario == experiment-config ]]; then path=deployment/aliyun/research/backtest/default.yaml
  else path=deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml; fi
  printf '%s\n' "$path" >"$tmp/paths"
  GITHUB_REF=refs/heads/main bash "$root/select-rust-ci-scope.sh" --event push --changed-files "$tmp/paths" --metadata "$root/fixtures/rust-ci-scope/metadata.fixture" --output "$tmp/$scenario"
done
if grep '^jobs=' "$tmp/experiment-config" | grep -q 'research-image'; then exit 1; fi
grep '^jobs=' "$tmp/embedded-controller" | grep -q 'research-image-binaries'
printf 'PASS: runtime experiment config does not rebuild research software; embedded image input still does\n'
