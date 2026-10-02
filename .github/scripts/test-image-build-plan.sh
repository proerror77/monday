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

# The Monorepo image/Kubernetes job must honor the same source-derived plan.
# A selected manifest check is not an instruction to compile both images.
for scenario in job-yaml trading collector docker-ignore; do
  case "$scenario" in
    job-yaml) path=deployment/aliyun/research/k8s/research-data-request-job.example.yaml; trading=false; collector=false ;;
    trading) path=rust_hft/deployment/docker/Dockerfile.trading; trading=true; collector=false ;;
    collector) path=rust_hft/tools/collector/src/lib.rs; trading=false; collector=true ;;
    docker-ignore) path=rust_hft/.dockerignore; trading=true; collector=true ;;
  esac
  printf '%s\n' "$path" >"$tmp/paths"
  bash "$root/select-rust-ci-scope.sh" --event pull_request --changed-files "$tmp/paths" --metadata "$root/fixtures/rust-ci-scope/metadata.fixture" --output "$tmp/production-$scenario"
  grep -Fqx "production_trading_image=$trading" "$tmp/production-$scenario"
  grep -Fqx "production_collector_image=$collector" "$tmp/production-$scenario"
  if [[ $scenario == job-yaml ]]; then
    grep -Fqx 'toolchain=false' "$tmp/production-$scenario"
    [[ $(sed -n 's/^image_matrix=//p' "$tmp/production-$scenario" | jq '.include|length') == 0 ]]
  fi
done
ruby -ryaml - "$root/../workflows/ci.yml" <<'RUBY'
jobs=YAML.load_file(ARGV[0]).fetch('jobs')
steps=jobs.fetch('deployment_artifacts').fetch('steps')
{'Build production trading image'=>'production_trading_image',
 'Build production collector image'=>'production_collector_image',
 'Verify production collector binary source and self-test'=>'production_collector_image'}.each do |name, flag|
  step=steps.find { |s| s['name']==name }
  abort "#{name} ignores source scope" unless step && step['if']=="needs.scope.outputs.#{flag} == 'true'"
  %w[selector scope].each { |job| abort "#{job} does not expose #{flag}" unless jobs[job]['outputs'].key?(flag) }
end
abort 'manifest verification disappeared' unless steps.any? { |s| s['name']=='Validate Kubernetes manifests without a cluster' && !s.key?('if') }
RUBY
printf 'PASS: ordinary Job manifests compile zero images; actual production image inputs retain their builds\n'
