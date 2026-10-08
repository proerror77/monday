#!/usr/bin/env bash
set -euo pipefail

event=${GITHUB_EVENT_NAME:-}
base=
head=HEAD
changed_files=
metadata=
output=${GITHUB_OUTPUT:-/dev/stdout}

while (($#)); do
  case "$1" in
    --event) event=$2; shift 2 ;;
    --base) base=$2; shift 2 ;;
    --head) head=$2; shift 2 ;;
    --changed-files) changed_files=$2; shift 2 ;;
    --metadata) metadata=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

loop=false
handoff=false
json=false
ondo=false
collector=false
control=false
focused=false
focused_packages=
toolchain=false
jobs=
security_jobs=
research_image_relevant=false
research_product=none
products="$(dirname "${BASH_SOURCE[0]}")/research-release-products.sh"
architecture=false
owning_packages=
loop_packages=
clippy_loop=false
clippy_handoff=false
image_live=false
image_paper=false
production_collector_image=false
declare -a paths=()

select_job() {
  local job=$1
  [[ ,$jobs, == *,$job,* ]] || jobs=${jobs:+$jobs,}$job
}

select_security_job() {
  local job=$1
  [[ ,$security_jobs, == *,$job,* ]] || security_jobs=${security_jobs:+$security_jobs,}$job
}

select_all_security_jobs() {
  clippy_loop=true
  clippy_handoff=true
  loop_packages=alpha-domain,alpha-store,alpha-engine,alpha-onnx-evaluator,alpha-harness,hft-cex-research-worker,hft-harnessctl,hft-research-ml
  select_security_job security/sast-semgrep
  select_security_job security/cargo-audit
  select_security_job security/secret-presence
  select_security_job security/license-check
  select_security_job security/clippy-strict
  select_security_job security/cargo-machete
  select_security_job security/secret-detection
}

select_security_scope() {
  [[ $loop == true ]] && clippy_loop=true
  [[ $handoff == true ]] && clippy_handoff=true
  local scan_repository=false rust_relevant=false

  if [[ $event == schedule || $event == workflow_dispatch ]]; then
    select_all_security_jobs
    return
  fi

  for path in "${paths[@]}"; do
    case "$path" in
      docs/*|*.md|LICENSE*|rust_hft/docs/*|rust_hft/README*|rust_hft/*/README*) ;;
      *) scan_repository=true ;;
    esac

  done

  case ",$jobs," in
    *",ci/rust,"*|*",ci/market-recorder-contract,"*|*",ci/polymarket-evidence-compiler-image,"*|*",ci/rust-hft-engine-fast-lane,"*|\
    *",ploy/research-image-"*|*",ploy/rust-"*|*",ploy/audit,"*|*",ploy/integration-regressions,"*)
      rust_relevant=true
      ;;
  esac

  [[ $scan_repository == true ]] && select_security_job security/sast-semgrep
  [[ $rust_relevant == true ]] && select_security_job security/cargo-audit
  [[ $scan_repository == true ]] && select_security_job security/secret-presence
  if [[ $rust_relevant == true ]]; then
    select_security_job security/license-check
    if [[ $clippy_loop == true || $clippy_handoff == true ]]; then
      select_job ci/clippy-strict
    fi
    select_security_job security/cargo-machete
  fi
  if [[ $event == push && ${GITHUB_REF:-} == refs/heads/main && \
        $(jq ".include|length" <<<"$image_matrix") != 0 ]]; then
    select_security_job security/container-scan
  fi
  select_security_job security/secret-detection
}

select_all_ci_jobs() {
  select_job ci/rust-shell-scripts
  select_job ci/rust
  select_job ci/research-foundation
  select_job ci/market-recorder-contract
  select_job ci/deployment-artifacts
  select_job ci/polymarket-evidence-compiler-image
  select_job ci/rust-hft-engine-fast-lane
  select_job ci/node-install
}

select_all_rust_ci_jobs() {
  select_job ci/rust
  select_job ci/market-recorder-contract
  select_job ci/deployment-artifacts
  select_job ci/polymarket-evidence-compiler-image
  select_job ci/rust-hft-engine-fast-lane
}

select_all_ploy_jobs() {
  research_product=$(bash "$products" merge "$research_product" "${1:-all}")
  architecture=true
  research_image_relevant=true
  [[ $event == pull_request ]] && select_job ploy/commit-hygiene
  select_job ploy/research-image-binaries
  select_job ploy/research-image-smoke
  select_job ploy/rust-format
  select_job ploy/safety-scans
  select_job ploy/audit
  select_job ploy/rust-control-plane
  select_job ploy/rust-runner-lean
  select_job ploy/rust-runner-full
  select_job ploy/rust-market-data
  select_job ploy/rust-research-heavy
  select_job ploy/frontend
  select_job ploy/integration-regressions
}

select_research_image_jobs() {
  research_product=$(bash "$products" merge "$research_product" "${1:-all}")
  research_image_relevant=true
  [[ $event == pull_request ]] && select_job ploy/commit-hygiene
  select_job ploy/research-image-binaries
  select_job ploy/research-image-smoke
  select_job ploy/safety-scans
}

select_main_research_image_jobs() {
  if [[ $event == push && $research_image_relevant == true ]]; then
    [[ $research_product != none ]] || research_product=$(bash "$products" normalize all)
    select_job ploy/research-image-binaries
    select_job ploy/research-image-smoke
  fi
}

select_all() {
  image_live=true
  image_paper=true
  loop=true
  loop_packages=alpha-domain,alpha-store,alpha-engine,alpha-onnx-evaluator,alpha-harness,hft-cex-research-worker,hft-harnessctl,hft-research-ml
  handoff=true
  json=true
  ondo=true
  collector=true
  control=true
  focused=true
  focused_packages=hft-live,hft-paper,hft-all-in-one,alpha-harness,hft-harnessctl
  toolchain=true
}

emit() {
  local value
  if [[ ,$jobs, == *,ploy/research-image-binaries,* && $research_product == none ]]; then research_product=$(bash "$products" normalize all); fi
  [[ ,$owning_packages, != *",hft-research-platform,"* ]] || select_job ci/research-foundation
  [[ $architecture == true ]] && select_job ploy/architecture-contracts
  [[ $control == true ]] && select_job ci/control-contracts
  # Every path that selects collector verification must exercise its production image.
  if [[ $collector == true ]]; then select_job ci/deployment-artifacts; fi
  image_matrix=$(printf '%s\n' "${paths[@]}" | bash "$(dirname "${BASH_SOURCE[0]}")/image-build-plan.sh" "$image_live" "$image_paper" "$collector" "$event")
  production_trading_image=$(jq -r 'any(.include[]; .name=="hft-trading")' <<<"$image_matrix")
  [[ $collector != true ]] || production_collector_image=true
  select_security_scope
  if [[ $event != schedule && ( $clippy_loop == true || $clippy_handoff == true ) ]]; then select_job ci/clippy-strict; fi
  for value in "$loop" "$handoff" "$json" "$ondo" "$collector" "$control" "$focused" "$toolchain"; do
    [[ $value == true || $value == false ]] || { printf 'invalid boolean selector output: %s\n' "$value" >&2; exit 1; }
  done
  [[ $jobs =~ ^((ci|ploy)/[a-z0-9/-]+(,(ci|ploy)/[a-z0-9/-]+)*)?$ ]] || { printf 'invalid job selector output: %s\n' "$jobs" >&2; exit 1; }
  [[ $security_jobs =~ ^(security/[a-z0-9/-]+(,security/[a-z0-9/-]+)*)?$ ]] || { printf 'invalid security job selector output: %s\n' "$security_jobs" >&2; exit 1; }
  [[ $owning_packages =~ ^([A-Za-z0-9_-]+(,[A-Za-z0-9_-]+)*)?$ ]] || { printf 'invalid owning package selector output: %s\n' "$owning_packages" >&2; exit 1; }
  printf '%s\n' \
    "research_product=$research_product" \
    "image_matrix=$image_matrix" \
    "production_trading_image=$production_trading_image" \
    "production_collector_image=$production_collector_image" \
    "jobs=,$jobs," \
    "security_jobs=,$security_jobs," \
    "owning_packages=,$owning_packages," \
    "loop_packages=,$loop_packages," \
    "clippy_loop=$clippy_loop" \
    "clippy_handoff=$clippy_handoff" \
    "loop=$loop" \
    "handoff=$handoff" \
    "json=$json" \
    "ondo=$ondo" \
    "collector=$collector" \
    "control=$control" \
    "focused=$focused" \
    "focused_packages=,$focused_packages," \
    "toolchain=$toolchain" \
    'selection_complete=true' >>"$output"
}

# Manual runs retain the complete build/package contract.
if [[ $event == workflow_dispatch ]]; then
  select_all
  select_all_ci_jobs
  select_job ploy/workflow-lint
  select_all_ploy_jobs
  emit
  exit 0
fi

# Scheduled security audits retain the complete security contract without
# selecting unrelated build or publication jobs.
if [[ $event == schedule ]]; then
  emit
  exit 0
fi

repo_root=$(git rev-parse --show-toplevel)
paths=()
if [[ -n $changed_files ]]; then
  while IFS= read -r path; do paths+=("$path"); done <"$changed_files"
else
  [[ -n $base ]] || { printf '%s\n' '--base is required when changed files are not provided' >&2; exit 2; }
  while IFS= read -r -d '' path; do paths+=("$path"); done \
    < <(git diff --no-renames --name-only --diff-filter=ACMRD -z "$base...$head")
fi

needs_metadata=false
lock_packages='[]'
lock_base=
lock_head=
if [[ -n $base ]]; then
  lock_base=$(git merge-base "$base" "$head")
  lock_head=$(git rev-parse "$head^{commit}")
fi
for path in "${paths[@]}"; do
  # workspace_runtime_retirement reads files directly, beyond Cargo's dependency
  # graph. Include the whole prediction tree (also its operational docs), retired
  # roots, canonical adapter manifests, and the external collector contracts.
  case "$path" in
    rust_hft/prediction-markets/*|products/ploy|products/ploy/*|\
    rust_hft/data-pipelines/adapters/adapter-polymarket/Cargo.toml|\
    rust_hft/execution-gateway/adapters/adapter-polymarket/Cargo.toml|\
    deployment/aliyun/polymarket*) architecture=true ;;
  esac
  case "$path" in
    AGENTS.md|*/AGENTS.md|CLAUDE.md|*/CLAUDE.md)
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    .github/workflows/acr-publish.yml|.github/scripts/test-acr-publish-workflow.sh|\
    .github/scripts/read-release-required-checks.sh|.github/scripts/wait-release-required-checks.sh|\
    .github/scripts/research-image-release-artifact.sh|.github/scripts/test-research-image-release-artifact.sh|\
    .github/scripts/publish-research-build-release.sh|.github/scripts/select-research-oss-policy.jq|\
    .github/scripts/migrate-research-oss-policy.sh|.github/scripts/test-migrate-research-oss-policy.sh|\
    .github/scripts/verify-research-runner-binaries.sh|\
    .github/scripts/read-acr-publish-source.sh|.github/scripts/select-acr-publish-source.sh|\
    .github/scripts/test-acr-publish-source-readback.sh|\
    .github/workflows/release.yml|.github/scripts/decide-release-once.sh|\
    .github/scripts/read-release-published.sh|.github/scripts/release-orchestrator-admit.sh|\
    .github/scripts/test-release-once.sh|.github/scripts/write-release-record.sh|\
    .github/scripts/test-release-record.sh)
      # Release policy changes run source/signature/manifest contracts on both
      # PR and main push. Only actual image/source/dependency inputs rebuild.
      select_job ci/ci-contracts
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      select_job ploy/workflow-lint
      continue
      ;;
    rust_hft/deployment/docker/Dockerfile.research)
      select_research_image_jobs cex-runner
      continue
      ;;
    rust_hft/deployment/docker/Dockerfile.prediction-research)
      select_research_image_jobs prediction-runner
      continue
      ;;
    deployment/aliyun/research/Dockerfile.campaign-cycle-controller)
      select_research_image_jobs controller
      continue
      ;;
    deployment/aliyun/research/Dockerfile.research-data)
      select_job ci/ci-contracts
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    .dockerignore)
      select_job ci/deployment-artifacts
      continue
      ;;
    rust_hft/.dockerignore)
      production_collector_image=true
      select_job ci/deployment-artifacts
      select_job ci/polymarket-evidence-compiler-image
      select_research_image_jobs
      continue
      ;;
    rust_hft/docker/Dockerfile|rust_hft/deployment/docker/Dockerfile.trading)
      select_job ci/deployment-artifacts
      continue
      ;;
    rust_hft/deployment/docker/Dockerfile.polymarket-evidence-compiler)
      select_job ci/polymarket-evidence-compiler-image
      continue
      ;;
    rust_hft/deployment/docker/*)
      select_all
      select_all_ci_jobs
      select_all_ploy_jobs
      continue
      ;;
    rust_hft/prediction-markets/ploy-frontend/*)
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      select_job ploy/safety-scans
      select_job ploy/frontend
      continue
      ;;
    rust_hft/prediction-markets/*.md)
      continue
      ;;
    rust_hft/Cargo.lock|rust_hft/runtime/Cargo.lock|rust_hft/shared/Cargo.lock|rust_hft/data-pipelines/Cargo.lock|rust_hft/research-core/Cargo.lock|rust_hft/research-core/platform/Cargo.lock|rust_hft/prediction-markets/Cargo.lock)
      if [[ -n $lock_base ]] && narrowed=$(bash "$(dirname "${BASH_SOURCE[0]}")/local-lock-impact.sh" "$lock_base" "$lock_head" "$path"); then
        lock_packages=$(jq -cn --argjson prior "$lock_packages" --argjson names "$narrowed" --arg workspace "${path%/Cargo.lock}" '$prior + [$names[] | {name:.,workspace:$workspace}]')
        needs_metadata=true
      elif [[ $path == rust_hft/prediction-markets/Cargo.lock ]]; then
        select_all_ploy_jobs prediction-runner
      else
        select_all
        select_all_rust_ci_jobs
        select_research_image_jobs
      fi
      continue
      ;;
    rust_hft/prediction-markets/Cargo.toml)
      select_all_ploy_jobs prediction-runner
      select_job ci/research-foundation
      continue
      ;;
    rust_hft/prediction-markets/*/Cargo.toml)
      select_research_image_jobs prediction-runner
      select_job ploy/audit
      needs_metadata=true
      ;;
    rust_hft/Cargo.toml|rust_hft/workspaces.json|rust_hft/runtime/Cargo.toml|rust_hft/shared/Cargo.toml|rust_hft/data-pipelines/Cargo.toml|rust_hft/research-core/Cargo.toml|rust_hft/research-core/platform/Cargo.toml)
      select_all
      select_all_rust_ci_jobs
      select_job ci/research-foundation
      select_research_image_jobs
      continue
      ;;
    rust_hft/*/Cargo.toml)
      needs_metadata=true
      ;;
    rust_hft/rust-toolchain*|rust_hft/.cargo/*|.cargo/*)
      select_all
      select_all_rust_ci_jobs
      select_all_ploy_jobs
      continue
      ;;
    .github/scripts/test-clickhouse-preparation.sh|.github/scripts/test-rust-workspaces.sh|.github/scripts/test-rust-docker-workspaces.rb)
      select_job ci/research-foundation
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      continue
      ;;
    .github/scripts/test-market-import.sh)
      select_job ci/research-foundation
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      continue
      ;;
    .github/scripts/research-release-products.sh|.github/scripts/research-release-products.json|.github/scripts/test-research-release-products.sh|.github/scripts/research-release-bundle.rb|.github/scripts/research-release-source-sha.sh|.github/scripts/test-research-checkout-ownership.sh|.github/scripts/verify-research-runtime-abi.sh|.github/scripts/test-research-runtime-abi.sh|.github/scripts/build-research-release.sh|.github/scripts/capture-research-build-inputs.sh|.github/scripts/research-image-smoke.sh|.github/scripts/verify-research-product-image.sh|.github/scripts/test-research-product-image.sh|.github/scripts/verify-research-controller-image.sh|.github/scripts/test-research-controller-image.sh|.github/scripts/download-research-release.sh|.github/scripts/test-download-research-release.sh)
      select_research_image_jobs
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      continue
      ;;
    deployment/aliyun/research/scripts/campaign-cycle-controller.sh|deployment/aliyun/research/scripts/campaign-job-watch.sh|deployment/aliyun/research/scripts/cex-materialization-entrypoint.sh|deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml)
      select_research_image_jobs controller
      control=true
      select_job ci/deployment-artifacts
      continue
      ;;
    .github/scripts/run-prediction-research-contracts.sh)
      select_job ploy/rust-research-heavy
      select_job ploy/workflow-lint
      continue
      ;;
    .github/scripts/write-ci-rust-evidence.sh|.github/scripts/verify-ci-rust-evidence.sh|.github/scripts/wait-ci-rust-evidence.sh|.github/scripts/test-ci-rust-evidence.sh|.github/scripts/check-collector-test-presence.sh)
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      continue
      ;;
    .github/scripts/classify-ack-research-job.sh|.github/scripts/test-classify-ack-research-job.sh|\
    .github/scripts/wait-ack-research-receipt.sh|.github/scripts/verify-ack-preflight.sh|\
    .github/scripts/wait-ack-rust-batch.sh|.github/scripts/verify-ack-rust-batch.sh|.github/scripts/test-ack-rust-batch.sh|\
    .github/scripts/test-ack-preflight-relay.sh|.github/scripts/test-preflight-workflow-gate.sh|\
    .github/ack-ci/receipt-public-key.pub|.github/ack-ci/PREFLIGHT.md|\
    .github/workflows/ack-flow-contracts.yml)
      # Public ACK routing/signature checks are control metadata. They have no
      # Cargo dependency impact; unknown future helpers block scope planning.
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    .github/scripts/run-collector-control-contracts.py|.github/scripts/test-collector-control-scheduling.py)
      # A deletion still appears in the Git diff. Never admit a reintroduced
      # tracked obsolete scheduler as executable CI code.
      if git -C "$repo_root" ls-files --error-unmatch -- "$path" >/dev/null 2>&1; then
        printf 'obsolete CI scheduler remains tracked: %s\n' "$path" >&2
        exit 2
      fi
      control=true
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    .github/scripts/run-collector-control-contracts.sh|.github/scripts/test-collector-control-scheduling.sh)
      control=true
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    .github/scripts/loop-nextest-archive.sh|.github/scripts/loop-nextest-shard.sh|.github/scripts/loop-nextest-counts.sh|\
    .github/scripts/loop-nextest-doctests.sh|.github/scripts/loop-nextest-wait.sh|.github/scripts/loop-nextest-gate.sh|.github/scripts/loop-nextest-plan.rb|\
    .github/scripts/install-cargo-nextest.sh|.github/scripts/test-loop-nextest.sh|.github/scripts/test-loop-nextest-doctests.sh)
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    .github/workflows/market-tape-seal-benchmark.yml|.github/workflows/release-rust.yml|\
    .github/workflows/ci.yml|.github/workflows/ploy-ci.yml|.github/workflows/security-enabled.yml|\
    .github/scripts/select-rust-ci-scope.sh|.github/scripts/research-workspace-locks.sh|.github/scripts/check-rust-workspace-reports.sh|.github/scripts/verify-ci-rust-same-run.sh|.github/scripts/test-ci-rust-same-run.sh|\
    .github/scripts/local-lock-impact.sh|.github/scripts/test-local-lock-impact.mjs|\
    .github/scripts/image-build-plan.sh|.github/scripts/test-image-build-plan.sh|\
    .github/scripts/save-tested-image.sh|.github/scripts/read-tested-image.sh|.github/scripts/test-tested-image.sh|\
    .github/scripts/read-published-image-source.sh|.github/scripts/select-main-image-scope.sh|\
    .github/scripts/test-main-image-scope.sh|.github/scripts/read-research-publish-baseline.sh|.github/scripts/select-main-research-scope.sh|.github/scripts/test-main-research-scope.sh|\
    .github/workflows/docker-smoke.yml|.github/workflows/docker-publish.yml|\
    .github/scripts/test-select-rust-ci-scope.sh|.github/scripts/fixtures/rust-ci-scope/*|\
    .github/scripts/verify-ci-gate.sh|.github/scripts/test-ci-monitor-scope.sh|\
    .github/scripts/test-agent-validation-gates.sh|.github/scripts/test-workflow-queue-lint.sh)
      select_job ci/ci-contracts
      select_job ploy/workflow-lint
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    .agents/skills/*/SKILL.md|.agents/skills/*/agents/openai.yaml|\
    .github/workflows/claude.yml|.github/workflows/claude-code-review.yml|\
    .github/ISSUE_TEMPLATE/*|.github/pull_request_template.md|\
    docs/agents/issue-tracker.md|docs/agents/triage-labels.md)
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      select_job ploy/workflow-lint
      continue
      ;;
    .github/scripts/agent-worktree-preflight.sh|.github/scripts/test-agent-worktree-preflight.sh)
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    package.json|package-lock.json|pnpm-lock.yaml|yarn.lock|.nvmrc|.node-version)
      select_job ci/node-install
      continue
      ;;
    .github/workflows/monitor-collector-host.yml|.github/scripts/test-monitor-collector-host.sh|\
    deployment/aliyun/collector-monitor-*|deployment/aliyun/collector-monitor.*|\
    deployment/aliyun/test-collector-monitor-*|deployment/aliyun/host-collector-monitor-*|\
    deployment/aliyun/monday-collector-health.*|deployment/aliyun/test-monday-collector-health.sh|\
    deployment/aliyun/host-collector-health-unit-release.sh|deployment/aliyun/test-collector-health-unit-release.sh)
      select_job ci/monitor-contracts
      select_job ploy/workflow-lint
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      continue
      ;;
    .github/workflows/*|.github/actions/*|.github/scripts/*)
      printf 'unmapped CI path: %s; add its owning contract mapping before dispatch\n' "$path" >&2
      exit 2
      ;;
    rust_hft/deployment/k8s/*|deployment/aliyun/research/k8s/*)
      select_job ci/deployment-artifacts
      continue
      ;;
    deployment/aliyun/polymarket-market-recorder-deploy.sh|\
    deployment/aliyun/test-polymarket-market-recorder-release.sh)
      control=true
      select_job ci/market-recorder-contract
      ;;
    deployment/aliyun/polymarket-market-tape.toml|\
    deployment/aliyun/polymarket-market-tape.service)
      control=true
      toolchain=true
      select_job ci/market-recorder-contract
      select_job ploy/integration-regressions
      select_job ci/rust
      ;;
    deployment/aliyun/polymarket-reference-collector.service|\
    deployment/aliyun/polymarket-reference-collector-shadow@.service|\
    deployment/aliyun/polymarket-market-tape-upload.service|\
    deployment/aliyun/polymarket-reference-upload.service)
      control=true
      toolchain=true
      select_job ploy/integration-regressions
      select_job ci/rust
      ;;
    deployment/aliyun/polymarket-raw-ops-*|\
    deployment/aliyun/test-polymarket-raw-ops-*|\
    deployment/aliyun/polymarket-shadow-gate-policy.jq|\
    deployment/aliyun/polymarket-legacy-health-policy.jq|\
    deployment/aliyun/polymarket-rust-health-policy.jq|\
    deployment/aliyun/polymarket-market-tape-upload-watchdog.sh|\
    deployment/aliyun/polymarket-market-tape-upload-watchdog.timer)
      control=true
      toolchain=true
      select_job ploy/integration-regressions
      select_job ci/rust
      ;;
    deployment/aliyun/research/backtest/*|deployment/aliyun/research/examples/*|\
    deployment/aliyun/research/builder/*)
      control=true
      continue
      ;;
    deployment/aliyun/*.md)
      continue
      ;;
    deployment/aliyun/*)
      control=true
      [[ $path == deployment/aliyun/research/* ]] && research_image_relevant=true
      ;;
    docs/*|*.md|LICENSE*)
      continue
      ;;
    rust_hft/docs/*|rust_hft/README*|rust_hft/*/README*)
      continue
      ;;
    rust_hft/scripts/deploy-ecs-tools-collector.sh)
      collector=true
      control=true
      toolchain=true
      select_job ci/rust-shell-scripts
      select_job ci/rust
      select_job ci/polymarket-evidence-compiler-image
      continue
      ;;
    rust_hft/scripts/workspace-metadata.sh|rust_hft/scripts/cargo-scoped.sh)
      select_all
      select_all_rust_ci_jobs
      select_all_ploy_jobs
      select_job ci/ci-contracts
      continue
      ;;
    rust_hft/scripts/*.sh)
      select_job ci/rust-shell-scripts
      continue
      ;;
    rust_hft/*)
      needs_metadata=true
      ;;
    *)
      if [[ $path != */* ]]; then
        select_all
        select_all_ci_jobs
        select_all_ploy_jobs
      fi
      ;;
  esac
done

[[ $control == true ]] && select_job ploy/safety-scans

if [[ $needs_metadata == false ]]; then
  [[ $toolchain == true ]] && select_job ci/rust
  select_main_research_image_jobs
  emit
  exit 0
fi

if [[ -z $metadata ]]; then
  metadata_dir=$(mktemp -d)
  metadata="$metadata_dir/combined.json"
  trap 'rm -rf "$metadata_dir"' EXIT
  "$repo_root/rust_hft/scripts/workspace-metadata.sh" >"$metadata"
fi

# Turn proven local lock edits into owning manifest paths, then use the same
# reverse-dependency traversal as source changes. Unknown ownership fails closed.
while IFS=$'\t' read -r lock_package lock_workspace; do
  [[ -n $lock_package ]] || continue
  manifests=$(jq -c --arg name "$lock_package" --arg root "$repo_root/" \
    '[.packages[] | select(.name==$name) | .manifest_path | ltrimstr($root) | select(startswith("rust_hft/"))] | unique' "$metadata")
  # A path dependency may belong to the other workspace. Root packages are
  # deliberately skipped by ordinary ownership, so preserve their broad lane.
  if [[ $(jq length <<<"$manifests") != 1 || $lock_package == rust-hft-workspace || $lock_package == ploy ]]; then
    if [[ $lock_workspace == rust_hft/prediction-markets ]]; then
      select_all_ploy_jobs
    else
      select_all
      select_all_rust_ci_jobs
      select_research_image_jobs
    fi
  else
    paths+=("$(jq -r '.[0]' <<<"$manifests")")
  fi
done < <(jq -r '.[] | [.name,.workspace] | @tsv' <<<"$lock_packages")

declare -a package_names=() package_dirs=() package_dependencies=()
while IFS=$'\t' read -r name manifest dependencies; do
  if [[ $manifest == "$repo_root/"* ]]; then manifest=${manifest#"$repo_root/"}; fi
  package_names+=("$name")
  package_dirs+=("${manifest%/Cargo.toml}")
  package_dependencies+=("$dependencies")
done < <(jq -r '.packages[] | [.name, .manifest_path, ([.dependencies[] | select(.path != null) | .name] | join(","))] | @tsv' "$metadata")

affected=$'\n'
direct_root_packages=$'\n'
checked_direct_packages=$'\n'
is_affected() { [[ $affected == *$'\n'"$1"$'\n'* ]]; }
mark_affected() { is_affected "$1" || affected+="$1"$'\n'; }
is_direct_root_package() { [[ $direct_root_packages == *$'\n'"$1"$'\n'* ]]; }
mark_direct_root_package() { is_direct_root_package "$1" || direct_root_packages+="$1"$'\n'; }
is_checked_direct_package() { [[ $checked_direct_packages == *$'\n'"$1"$'\n'* ]]; }
mark_checked_direct_package() {
  is_direct_root_package "$1" || return 0
  is_checked_direct_package "$1" || checked_direct_packages+="$1"$'\n'
}

for path in "${paths[@]}"; do
  [[ $path == rust_hft/* ]] || continue
  case "$path" in
    rust_hft/deployment/docker/*|rust_hft/deployment/k8s/*|rust_hft/.dockerignore|\
    rust_hft/Cargo.toml|rust_hft/Cargo.lock|rust_hft/prediction-markets/Cargo.toml|\
    rust_hft/prediction-markets/Cargo.lock|rust_hft/prediction-markets/ploy-frontend/*|\
    rust_hft/prediction-markets/*.md|rust_hft/rust-toolchain*|rust_hft/.cargo/*|\
    rust_hft/AGENTS.md|rust_hft/*/AGENTS.md|rust_hft/CLAUDE.md|rust_hft/*/CLAUDE.md)
      continue
      ;;
  esac
  owner=
  owner_directory=
  owner_length=0
  for ((index = 0; index < ${#package_names[@]}; index++)); do
    name=${package_names[$index]}
    directory=${package_dirs[$index]}
    if [[ $path == "$directory"/* && ${#directory} -gt $owner_length ]]; then
      owner=$name
      owner_directory=$directory
      owner_length=${#directory}
    fi
  done
  if [[ $owner == ploy ]]; then
    select_all_ploy_jobs
    continue
  fi
  if [[ -z $owner || $owner == rust-hft-workspace ]]; then
    select_all
    if [[ $path == rust_hft/prediction-markets/* ]]; then
      select_all_ploy_jobs
    else
      select_all_rust_ci_jobs
    fi
    continue
  fi
  [[ $owner_directory == rust_hft/prediction-markets* ]] || mark_direct_root_package "$owner"
  mark_affected "$owner"
done

# Cargo.toml remains the source of truth for downstream package impact.
changed=true
while [[ $changed == true ]]; do
  changed=false
  for ((index = 0; index < ${#package_names[@]}; index++)); do
    name=${package_names[$index]}
    is_affected "$name" && continue
    dependency_list=${package_dependencies[$index]}
    [[ -n $dependency_list ]] || continue
    IFS=',' read -ra dependencies <<<"$dependency_list"
    for dependency in "${dependencies[@]}"; do
      if [[ -n $dependency ]] && is_affected "$dependency"; then
        mark_affected "$name"
        changed=true
        break
      fi
    done
  done
done

select_if_affected() {
  local flag=$1
  shift
  local package selected=false
  for package in "$@"; do
    if is_affected "$package"; then
      mark_checked_direct_package "$package"
      if [[ $flag == loop ]]; then
        [[ ,$loop_packages, == *,$package,* ]] || loop_packages=${loop_packages:+$loop_packages,}$package
      fi
      selected=true
    fi
  done
  [[ $selected == true ]] || return 0
  case "$flag" in
    loop) loop=true ;;
    handoff) handoff=true ;;
    json) json=true ;;
    ondo) ondo=true ;;
    collector) collector=true ;;
    focused) focused=true ;;
  esac
  toolchain=true
}

select_job_if_affected() {
  local job=$1
  shift
  local package selected=false
  for package in "$@"; do
    if is_affected "$package"; then
      mark_checked_direct_package "$package"
      selected=true
    fi
  done
  [[ $selected == true ]] && select_job "$job"
  return 0
}

select_if_affected loop alpha-domain alpha-store alpha-engine alpha-onnx-evaluator \
  alpha-harness hft-cex-research-worker hft-harnessctl hft-research-ml
select_if_affected handoff hft-live
select_if_affected json hft-integration hft-data-adapter-binance hft-infra-redis hft-live
select_if_affected ondo hft-data-adapter-ondo-perps hft-execution-adapter-ondo-perps hft-live
select_if_affected collector hft-collector
select_if_affected focused hft-live hft-paper hft-all-in-one
if [[ $focused == true && -z $focused_packages ]]; then
  for package in hft-live hft-paper hft-all-in-one alpha-harness hft-harnessctl; do
    if is_affected "$package"; then focused_packages=${focused_packages:+$focused_packages,}$package; fi
  done
fi

is_affected hft-live && image_live=true
is_affected hft-paper && image_paper=true

# Collector source and its host controls are one release boundary.
if [[ $collector == true ]]; then control=true; fi

if [[ $toolchain == true ]]; then select_job ci/rust; fi
if [[ $collector == true ]]; then select_job ci/polymarket-evidence-compiler-image; fi
select_job_if_affected ci/deployment-artifacts hft-live
select_job_if_affected ci/rust-hft-engine-fast-lane hft-engine hft-binance-depth

prediction_package_affected=false
for ((index = 0; index < ${#package_names[@]}; index++)); do
  if [[ ${package_dirs[$index]} == rust_hft/prediction-markets* ]] && is_affected "${package_names[$index]}"; then
    prediction_package_affected=true
    break
  fi
done
if [[ $prediction_package_affected == true ]]; then
  [[ $event == pull_request ]] && select_job ploy/commit-hygiene
  select_job ploy/rust-format
  select_job ploy/safety-scans
fi
select_job_if_affected ploy/rust-control-plane ploy-agent-sidecar ploy-daemon-host new-ployd \
  ployctl ploy-control-client ploytui ploy-deployments ploy-operator-contracts ploy-platform \
  ploy-platform-runtime
select_job_if_affected ploy/rust-runner-lean ploy-strategy-bundles ploy-market-data \
  ploy-strategy-runtime ploy-replay
select_job_if_affected ploy/rust-runner-full new-ploy-runner ploy-backtest ploy-runner-host \
  ploy-strategy-runtime ploy-strategy-bundles
select_job_if_affected ploy/rust-market-data ploy-market-data
select_job_if_affected ploy/rust-research-heavy ploy-feed-loaders ploy-research ploy-market-data ploy-agent-sidecar
select_job_if_affected ploy/frontend ploy-operator-contracts
select_job_if_affected ploy/integration-regressions ploy

if is_affected ploy-research || is_affected hft-prediction-research-worker || is_affected hft-prediction-research-operator; then
  research_product=$(bash "$products" merge "$research_product" prediction-runner)
  research_image_relevant=true
fi
if is_affected hft-collector || is_affected alpha-harness || is_affected hft-cex-research-worker || is_affected hft-backtest; then
  research_product=$(bash "$products" merge "$research_product" cex-runner)
  research_image_relevant=true
fi
if is_affected hft-collector || is_affected alpha-harness; then
  research_product=$(bash "$products" merge "$research_product" controller)
fi
# The controller now directly owns the platform release importer.
if is_affected hft-research-platform; then
  select_research_image_jobs controller
fi
if [[ $event == pull_request ]] && is_affected hft-backtest; then
  select_job ploy/research-image-binaries
fi
select_job_if_affected ci/research-foundation hft-market-pipeline
select_job_if_affected ci/research-foundation hft-data
select_main_research_image_jobs

while IFS= read -r package; do
  [[ -n $package ]] || continue
  if ! is_checked_direct_package "$package"; then
    [[ ,$owning_packages, == *,$package,* ]] || owning_packages=${owning_packages:+$owning_packages,}$package
  fi
done <<<"$direct_root_packages"
if [[ -n $owning_packages ]]; then
  toolchain=true
  select_job ci/rust
fi

emit
