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
toolchain=false
jobs=
security_jobs=
research_image_relevant=false

select_job() {
  local job=$1
  [[ ,$jobs, == *,$job,* ]] || jobs=${jobs:+$jobs,}$job
}

select_security_job() {
  local job=$1
  [[ ,$security_jobs, == *,$job,* ]] || security_jobs=${security_jobs:+$security_jobs,}$job
}

select_all_security_jobs() {
  select_security_job security/sast-semgrep
  select_security_job security/cargo-audit
  select_security_job security/secret-presence
  select_security_job security/license-check
  select_security_job security/clippy-strict
  select_security_job security/cargo-machete
  select_security_job security/secret-detection
}

select_security_scope() {
  local scan_repository=false rust_relevant=false container_relevant=false

  if [[ $event == schedule || $event == workflow_dispatch ]]; then
    select_all_security_jobs
    return
  fi

  for path in "${paths[@]}"; do
    case "$path" in
      docs/*|*.md|LICENSE*|rust_hft/docs/*|rust_hft/README*|rust_hft/*/README*) ;;
      *) scan_repository=true ;;
    esac
    case "$path" in
      .github/workflows/security-enabled.yml|rust_hft/docker/Dockerfile|\
      rust_hft/deployment/docker/Dockerfile.trading|rust_hft/.dockerignore)
        container_relevant=true
        ;;
    esac
  done

  case ",$jobs," in
    *",ci/rust,"*|*",ci/polymarket-evidence-compiler-image,"*|*",ci/rust-hft-engine-fast-lane,"*|\
    *",ploy/research-image-"*|*",ploy/rust-"*|*",ploy/audit,"*|*",ploy/integration-regressions,"*)
      rust_relevant=true
      ;;
  esac

  [[ $scan_repository == true ]] && select_security_job security/sast-semgrep
  [[ $rust_relevant == true ]] && select_security_job security/cargo-audit
  [[ $scan_repository == true ]] && select_security_job security/secret-presence
  if [[ $rust_relevant == true ]]; then
    select_security_job security/license-check
    select_security_job security/clippy-strict
    select_security_job security/cargo-machete
  fi
  if [[ $event == push && ${GITHUB_REF:-} == refs/heads/main && \
        ($rust_relevant == true || $container_relevant == true) ]]; then
    select_security_job security/container-scan
  fi
  select_security_job security/secret-detection
}

select_all_ci_jobs() {
  select_job ci/rust
  select_job ci/deployment-artifacts
  select_job ci/polymarket-evidence-compiler-image
  select_job ci/rust-hft-engine-fast-lane
  select_job ci/node-install
}

select_all_rust_ci_jobs() {
  select_job ci/rust
  select_job ci/deployment-artifacts
  select_job ci/polymarket-evidence-compiler-image
  select_job ci/rust-hft-engine-fast-lane
}

select_all_ploy_jobs() {
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
  research_image_relevant=true
  [[ $event == pull_request ]] && select_job ploy/commit-hygiene
  select_job ploy/research-image-binaries
  select_job ploy/research-image-smoke
  select_job ploy/safety-scans
}

select_main_research_image_jobs() {
  if [[ $event == push && $research_image_relevant == true ]]; then
    select_job ploy/research-image-binaries
    select_job ploy/research-image-smoke
  fi
}

select_all() {
  loop=true
  handoff=true
  json=true
  ondo=true
  collector=true
  control=true
  focused=true
  toolchain=true
}

emit() {
  local value
  select_security_scope
  for value in "$loop" "$handoff" "$json" "$ondo" "$collector" "$control" "$focused" "$toolchain"; do
    [[ $value == true || $value == false ]] || { printf 'invalid boolean selector output: %s\n' "$value" >&2; exit 1; }
  done
  [[ $jobs =~ ^((ci|ploy)/[a-z0-9/-]+(,(ci|ploy)/[a-z0-9/-]+)*)?$ ]] || { printf 'invalid job selector output: %s\n' "$jobs" >&2; exit 1; }
  [[ $security_jobs =~ ^(security/[a-z0-9/-]+(,security/[a-z0-9/-]+)*)?$ ]] || { printf 'invalid security job selector output: %s\n' "$security_jobs" >&2; exit 1; }
  printf '%s\n' \
    "jobs=,$jobs," \
    "security_jobs=,$security_jobs," \
    "loop=$loop" \
    "handoff=$handoff" \
    "json=$json" \
    "ondo=$ondo" \
    "collector=$collector" \
    "control=$control" \
    "focused=$focused" \
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
declare -a paths=()
if [[ -n $changed_files ]]; then
  while IFS= read -r path; do paths+=("$path"); done <"$changed_files"
else
  [[ -n $base ]] || { printf '%s\n' '--base is required when changed files are not provided' >&2; exit 2; }
  while IFS= read -r -d '' path; do paths+=("$path"); done \
    < <(git diff --no-renames --name-only --diff-filter=ACMRD -z "$base...$head")
fi

needs_metadata=false
for path in "${paths[@]}"; do
  case "$path" in
    .github/workflows/ploy-ci.yml)
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      select_job ploy/workflow-lint
      select_all_ploy_jobs
      continue
      ;;
    .github/workflows/acr-publish.yml|.github/scripts/test-acr-publish-workflow.sh)
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      select_job ploy/workflow-lint
      research_image_relevant=true
      continue
      ;;
    rust_hft/deployment/docker/Dockerfile.research)
      select_research_image_jobs
      continue
      ;;
    rust_hft/.dockerignore)
      select_job ci/deployment-artifacts
      select_job ci/polymarket-evidence-compiler-image
      select_research_image_jobs
      continue
      ;;
    rust_hft/deployment/docker/Dockerfile.trading)
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
    rust_hft/prediction-markets/Cargo.toml|rust_hft/prediction-markets/Cargo.lock)
      select_all_ploy_jobs
      continue
      ;;
    rust_hft/prediction-markets/*/Cargo.toml)
      select_research_image_jobs
      select_job ploy/audit
      needs_metadata=true
      ;;
    rust_hft/*/Cargo.toml)
      needs_metadata=true
      ;;
    rust_hft/Cargo.toml|rust_hft/Cargo.lock)
      select_all
      select_all_rust_ci_jobs
      select_research_image_jobs
      continue
      ;;
    rust_hft/rust-toolchain*|rust_hft/.cargo/*|.cargo/*)
      select_all
      select_all_rust_ci_jobs
      select_all_ploy_jobs
      continue
      ;;
    .github/workflows/ci.yml)
      select_all
      select_all_ci_jobs
      continue
      ;;
    .github/workflows/security-enabled.yml)
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      select_job ploy/workflow-lint
      select_all_security_jobs
      continue
      ;;
    .github/workflows/claude.yml|.github/workflows/claude-code-review.yml|\
    .github/ISSUE_TEMPLATE/*|.github/pull_request_template.md|\
    docs/agents/issue-tracker.md|docs/agents/triage-labels.md)
      [[ $event == pull_request ]] && select_job ploy/commit-hygiene
      select_job ploy/workflow-lint
      continue
      ;;
    .github/scripts/select-rust-ci-scope.sh|.github/scripts/test-select-rust-ci-scope.sh|.github/scripts/fixtures/rust-ci-scope/*)
      select_all
      select_all_ci_jobs
      select_job ploy/workflow-lint
      select_all_ploy_jobs
      continue
      ;;
    package.json|package-lock.json|pnpm-lock.yaml|yarn.lock|.nvmrc|.node-version)
      select_job ci/node-install
      continue
      ;;
    .github/workflows/*|.github/actions/*|.github/scripts/*)
      select_all
      select_all_ci_jobs
      select_job ploy/workflow-lint
      select_all_ploy_jobs
      continue
      ;;
    rust_hft/deployment/k8s/*|deployment/aliyun/research/k8s/*)
      select_job ci/deployment-artifacts
      [[ $path == deployment/aliyun/research/* ]] && research_image_relevant=true
      continue
      ;;
    deployment/aliyun/*)
      control=true
      toolchain=true
      select_job ploy/integration-regressions
      [[ $path == deployment/aliyun/research/* ]] && research_image_relevant=true
      ;;
    docs/*|*.md|LICENSE*)
      continue
      ;;
    rust_hft/docs/*|rust_hft/README*|rust_hft/*/README*)
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
  (cd "$repo_root/rust_hft" && cargo metadata --format-version 1 --no-deps --locked) >"$metadata_dir/rust-hft.json"
  (cd "$repo_root/rust_hft/prediction-markets" && cargo metadata --format-version 1 --no-deps --locked) >"$metadata_dir/prediction-markets.json"
  jq -s '{packages: [.[].packages[]]}' \
    "$metadata_dir/rust-hft.json" "$metadata_dir/prediction-markets.json" >"$metadata"
fi

declare -a package_names=() package_dirs=() package_dependencies=()
while IFS=$'\t' read -r name manifest dependencies; do
  if [[ $manifest == "$repo_root/"* ]]; then manifest=${manifest#"$repo_root/"}; fi
  package_names+=("$name")
  package_dirs+=("${manifest%/Cargo.toml}")
  package_dependencies+=("$dependencies")
done < <(jq -r '.packages[] | [.name, .manifest_path, ([.dependencies[] | select(.path != null) | .name] | join(","))] | @tsv' "$metadata")

affected=$'\n'
is_affected() { [[ $affected == *$'\n'"$1"$'\n'* ]]; }
mark_affected() { is_affected "$1" || affected+="$1"$'\n'; }

for path in "${paths[@]}"; do
  [[ $path == rust_hft/* ]] || continue
  case "$path" in
    rust_hft/deployment/docker/*|rust_hft/deployment/k8s/*|rust_hft/.dockerignore|\
    rust_hft/Cargo.toml|rust_hft/Cargo.lock|rust_hft/prediction-markets/Cargo.toml|\
    rust_hft/prediction-markets/Cargo.lock|rust_hft/prediction-markets/ploy-frontend/*|\
    rust_hft/prediction-markets/*.md|rust_hft/rust-toolchain*|rust_hft/.cargo/*)
      continue
      ;;
  esac
  owner=
  owner_length=0
  for ((index = 0; index < ${#package_names[@]}; index++)); do
    name=${package_names[$index]}
    directory=${package_dirs[$index]}
    if [[ $path == "$directory"/* && ${#directory} -gt $owner_length ]]; then
      owner=$name
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
  local package
  for package in "$@"; do
    if is_affected "$package"; then
      case "$flag" in
        loop) loop=true ;;
        handoff) handoff=true ;;
        json) json=true ;;
        ondo) ondo=true ;;
        collector) collector=true ;;
        focused) focused=true ;;
      esac
      toolchain=true
      return
    fi
  done
}

select_job_if_affected() {
  local job=$1
  shift
  local package
  for package in "$@"; do
    if is_affected "$package"; then
      select_job "$job"
      return
    fi
  done
}

select_if_affected loop alpha-domain alpha-store alpha-engine alpha-onnx-evaluator \
  alpha-harness hft-harnessctl hft-research-ml
select_if_affected handoff hft-live
select_if_affected json hft-integration hft-data-adapter-binance hft-infra-redis hft-live
select_if_affected ondo hft-data-adapter-ondo-perps hft-execution-adapter-ondo-perps hft-live
select_if_affected collector hft-collector
select_if_affected focused hft-live hft-paper hft-all-in-one

# Collector source and its host controls are one release boundary.
if [[ $collector == true ]]; then control=true; fi

if [[ $toolchain == true ]]; then select_job ci/rust; fi
if [[ $collector == true ]]; then select_job ci/polymarket-evidence-compiler-image; fi
select_job_if_affected ci/deployment-artifacts hft-live
select_job_if_affected ci/rust-hft-engine-fast-lane hft-engine

prediction_package_affected=false
for ((index = 0; index < ${#package_names[@]}; index++)); do
  if [[ ${package_dirs[$index]} == rust_hft/prediction-markets* ]] && is_affected "${package_names[$index]}"; then
    prediction_package_affected=true
    break
  fi
done
if [[ $prediction_package_affected == true ]]; then
  research_image_relevant=true
  [[ $event == pull_request ]] && select_job ploy/commit-hygiene
  select_job ploy/rust-format
  select_job ploy/safety-scans
fi
select_job_if_affected ploy/rust-control-plane ploy-agent-sidecar ploy-daemon-host new-ployd \
  ployctl ploy-control-client ploytui ploy-deployments ploy-operator-contracts ploy-platform \
  ploy-platform-runtime ploy-trading
select_job_if_affected ploy/rust-runner-lean ploy-strategy-bundles ploy-market-data \
  ploy-strategy-runtime ploy-replay
select_job_if_affected ploy/rust-runner-full new-ploy-runner ploy-backtest ploy-runner-host \
  ploy-strategy-runtime ploy-strategy-bundles ploy-connectivity
select_job_if_affected ploy/rust-market-data ploy-market-data
select_job_if_affected ploy/rust-research-heavy ploy-feed-loaders ploy-research ploy-market-data
select_job_if_affected ploy/frontend ploy-operator-contracts
select_job_if_affected ploy/integration-regressions ploy

if is_affected hft-collector || is_affected alpha-harness || is_affected hft-backtest; then
  research_image_relevant=true
fi
if [[ $event == pull_request ]] && is_affected hft-backtest; then
  select_job ploy/research-image-binaries
fi
select_main_research_image_jobs

emit
