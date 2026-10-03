#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
selector="$script_dir/select-rust-ci-scope.sh"
fixtures="$script_dir/fixtures/rust-ci-scope"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT


run_case() {
  local name=$1 event=$2 changed=$3 ref=
  local metadata=${4:-"$fixtures/metadata.fixture"}
  local output="$tmp_dir/$name.out"
  local changed_file="$fixtures/$changed"
  [[ -f $changed_file ]] || changed_file="$tmp_dir/$changed"
  [[ $event == push ]] && ref=refs/heads/main
  # Each scenario is a fresh workflow output file, including repeated paths.
  : > "$output"
  GITHUB_REF=$ref "$selector" --event "$event" --changed-files "$changed_file" \
    --metadata "$metadata" --output "$output"
  printf '%s\n' "$output"
}

printf '%s\n' package-lock.json >"$tmp_dir/root-node.txt"
printf '%s\n' .github/workflows/security.yml >"$tmp_dir/unknown-workflow.txt"
printf '%s\n' .github/workflows/security-enabled.yml >"$tmp_dir/security-workflow.txt"
printf '%s\n' .github/scripts/run-collector-control-contracts.sh >"$tmp_dir/control-scheduling.txt"
printf '%s\n' .github/scripts/test-collector-control-scheduling.sh >"$tmp_dir/control-scheduling-test.txt"
printf '%s\n' .github/scripts/run-collector-control-contracts.py .github/scripts/test-collector-control-scheduling.py >"$tmp_dir/control-scheduling-deletions.txt"
printf '%s\n' .github/ISSUE_TEMPLATE/engineering-change.yml >"$tmp_dir/governance-template.txt"
printf '%s\n' docs/agents/issue-tracker.md >"$tmp_dir/governance-doc.txt"
printf '%s\n' .agents/skills/monday-research-evidence-audit/SKILL.md >"$tmp_dir/skill.txt"
printf '%s\n' .agents/skills/monday-delivery-status/agents/openai.yaml >"$tmp_dir/skill-ui.txt"
printf '%s\n' AGENTS.md rust_hft/tools/collector/AGENTS.md >"$tmp_dir/agent-instructions.txt"
printf '%s\n' rust_hft/CLAUDE.md rust_hft/tools/collector/src/polymarket/reference.rs \
  >"$tmp_dir/agent-instructions-with-code.txt"
printf '%s\n' Makefile >"$tmp_dir/unknown-root.txt"
printf '%s\n' config/risk.toml >"$tmp_dir/unknown-nested.txt"
printf '%s\n' deployment/aliyun/host-rust-lob-cutover.sh >"$tmp_dir/lob-control.txt"
printf '%s\n' deployment/aliyun/polymarket-market-recorder-deploy.sh >"$tmp_dir/market-recorder-control.txt"
printf '%s\n' deployment/aliyun/polymarket-market-tape.service >"$tmp_dir/market-recorder-unit.txt"
printf '%s\n' deployment/aliyun/bybit-options-archiver.service >"$tmp_dir/unowned-unit.txt"
printf '%s\n' rust_hft/deployment/docker/Dockerfile.trading >"$tmp_dir/trading-dockerfile.txt"
printf '%s\n' rust_hft/docs/README.md >"$tmp_dir/rust-docs.txt"
printf '%s\n' rust_hft/research-core/README.md >"$tmp_dir/package-readme.txt"
printf '%s\n' rust_hft/scripts/clickhouse/run_bitget_dedup.sh >"$tmp_dir/rust-shell-script.txt"
printf '%s\n' rust_hft/scripts/deploy-ecs-tools-collector.sh >"$tmp_dir/rust-deploy-collector.txt"
printf '%s\n' \
  .github/scripts/agent-worktree-preflight.sh \
  .github/scripts/test-agent-worktree-preflight.sh >"$tmp_dir/preflight-only.txt"

assert_flag() {
  local output=$1 flag=$2 expected=$3
  grep -qx "$flag=$expected" "$output" || {
    printf '%s: expected %s=%s\n' "$output" "$flag" "$expected" >&2
    cat "$output" >&2
    exit 1
  }
}

assert_jobs() {
  local output=$1 expected=$2
  local actual
  if grep -Eq '^clippy_(loop|handoff)=true$' "$output" && ! grep -Fqx 'jobs=,,' "$output"; then expected="${expected:+$expected,}ci/clippy-strict"; fi
  if grep -Fqx 'control=true' "$output"; then
    expected="${expected:+$expected,}ci/control-contracts"
  fi
  actual=$(sed -n 's/^jobs=//p' "$output")
  [[ $actual == ,*, ]] || {
    printf '%s: jobs output must use exact comma-delimited membership: %s\n' "$output" "$actual" >&2
    exit 1
  }
  actual=${actual#,}
  actual=${actual%,}
  [[ $(printf '%s' "$actual" | tr ',' '\n' | sort) == "$(printf '%s' "$expected" | tr ',' '\n' | sort)" ]] || {
    printf '%s: expected jobs=%s, got jobs=%s\n' "$output" "$expected" "$actual" >&2
    exit 1
  }
}

assert_owning_packages() {
  local output=$1 expected=$2 actual
  actual=$(sed -n 's/^owning_packages=//p' "$output")
  [[ $actual == ,*, ]] || {
    printf '%s: owning_packages output must use exact comma-delimited membership: %s\n' "$output" "$actual" >&2
    exit 1
  }
  actual=${actual#,}
  actual=${actual%,}
  [[ $actual == "$expected" ]] || {
    printf '%s: expected owning_packages=%s, got owning_packages=%s\n' "$output" "$expected" "$actual" >&2
    exit 1
  }
}

assert_security_jobs() {
  local output=$1 expected=$2 actual
  if grep -q '^jobs=.*ci/clippy-strict' "$output" && ! grep -q '^security_jobs=.*security/clippy-strict' "$output"; then expected=${expected/,security\/clippy-strict/}; fi
  actual=$(sed -n 's/^security_jobs=//p' "$output")
  [[ $actual == ,*, ]] || {
    printf '%s: security_jobs output must use exact comma-delimited membership: %s\n' "$output" "$actual" >&2
    exit 1
  }
  actual=${actual#,}
  actual=${actual%,}
  [[ $actual == "$expected" ]] || {
    printf '%s: expected security_jobs=%s, got security_jobs=%s\n' "$output" "$expected" "$actual" >&2
    exit 1
  }
}

job_cases=(
  'collector|pull_request|collector.txt|ci/rust,ci/polymarket-evidence-compiler-image,ci/deployment-artifacts'
  'pinned-aliyun|pull_request|pinned-aliyun.txt|ci/market-recorder-contract,ploy/integration-regressions,ci/rust,ploy/safety-scans,ploy/architecture-contracts'
  'pinned-aliyun-push|push|pinned-aliyun.txt|ci/market-recorder-contract,ploy/integration-regressions,ci/rust,ploy/safety-scans,ploy/architecture-contracts'
  'future-aliyun-pin|pull_request|future-aliyun-pin.txt|ploy/safety-scans'
  'future-aliyun-markdown-pin|pull_request|future-aliyun-markdown-pin.txt|'
  'lob-control|pull_request|lob-control.txt|ploy/safety-scans'
  'market-recorder-control|pull_request|market-recorder-control.txt|ci/market-recorder-contract,ploy/safety-scans,ploy/architecture-contracts'
  'market-recorder-unit|pull_request|market-recorder-unit.txt|ci/market-recorder-contract,ploy/integration-regressions,ci/rust,ploy/safety-scans,ploy/architecture-contracts'
  'unowned-unit|pull_request|unowned-unit.txt|ploy/safety-scans'
  'evaluator|pull_request|evaluator.txt|ploy/commit-hygiene,ploy/rust-format,ploy/safety-scans,ploy/rust-research-heavy,ploy/architecture-contracts'
  'shared-prediction|pull_request|shared-prediction.txt|ploy/commit-hygiene,ploy/rust-format,ploy/safety-scans,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions,ploy/architecture-contracts'
  'prediction-lock|pull_request|prediction-lock.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions,ploy/architecture-contracts'
  'research-dockerfile|pull_request|research-dockerfile.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans'
  'campaign-controller-dockerfile|pull_request|campaign-controller-dockerfile.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans'
  'unknown-docker|pull_request|unknown-docker.txt|ci/rust-shell-scripts,ci/rust,ci/research-foundation,ci/market-recorder-contract,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions,ploy/architecture-contracts'
  'prediction-workflow|pull_request|prediction-workflow.txt|ci/ci-contracts,ploy/workflow-lint,ploy/commit-hygiene'
  'root-node|pull_request|root-node.txt|ci/node-install'
  'security-workflow|pull_request|security-workflow.txt|ci/ci-contracts,ploy/workflow-lint,ploy/commit-hygiene'
  'security-workflow-push|push|security-workflow.txt|ci/ci-contracts,ploy/workflow-lint'
  'control-scheduling|pull_request|control-scheduling.txt|ci/ci-contracts,ploy/workflow-lint,ploy/commit-hygiene,ploy/safety-scans'
  'control-scheduling-test|pull_request|control-scheduling-test.txt|ci/ci-contracts,ploy/workflow-lint,ploy/commit-hygiene,ploy/safety-scans'
  'control-scheduling-deletions|pull_request|control-scheduling-deletions.txt|ci/ci-contracts,ploy/workflow-lint,ploy/commit-hygiene,ploy/safety-scans'
  'governance-template|pull_request|governance-template.txt|ploy/commit-hygiene,ploy/workflow-lint'
  'governance-doc|pull_request|governance-doc.txt|ploy/commit-hygiene,ploy/workflow-lint'
  'skill|pull_request|skill.txt|ploy/commit-hygiene,ploy/workflow-lint'
  'skill-push|push|skill.txt|ploy/workflow-lint'
  'skill-ui|pull_request|skill-ui.txt|ploy/commit-hygiene,ploy/workflow-lint'
  'agent-instructions|pull_request|agent-instructions.txt|ploy/commit-hygiene'
  'agent-instructions-with-code|pull_request|agent-instructions-with-code.txt|ploy/commit-hygiene,ci/rust,ci/polymarket-evidence-compiler-image,ci/deployment-artifacts'
  'preflight-only|pull_request|preflight-only.txt|ploy/commit-hygiene'
  'unknown-root|pull_request|unknown-root.txt|ci/rust-shell-scripts,ci/rust,ci/research-foundation,ci/market-recorder-contract,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions,ploy/architecture-contracts'
  'unknown-nested|pull_request|unknown-nested.txt|'
  'rust-docs|pull_request|rust-docs.txt|'
  'package-readme|pull_request|package-readme.txt|'
  'rust-shell-script|pull_request|rust-shell-script.txt|ci/rust-shell-scripts'
  'rust-deploy-collector|pull_request|rust-deploy-collector.txt|ci/rust-shell-scripts,ci/rust,ci/polymarket-evidence-compiler-image,ploy/safety-scans,ci/deployment-artifacts'
  'docs|pull_request|docs.txt|ploy/architecture-contracts'
  'unknown-prediction|pull_request|unknown-prediction.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions,ploy/architecture-contracts'
  'mixed-prediction|pull_request|mixed-prediction.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans,ploy/rust-format,ploy/rust-research-heavy,ploy/architecture-contracts'
  'frontend|pull_request|frontend.txt|ploy/commit-hygiene,ploy/safety-scans,ploy/frontend,ploy/architecture-contracts'
  'backtest|pull_request|backtest.txt|ploy/research-image-binaries,ci/rust|hft-backtest'
  'live-push|push|live.txt|ci/rust,ci/deployment-artifacts'
  'trading-dockerfile-push|push|trading-dockerfile.txt|ci/deployment-artifacts'
  'research-deployment-push|push|research-deployment.txt|ci/deployment-artifacts'
  'acr-workflow-push|push|acr-workflow.txt|ci/ci-contracts,ploy/workflow-lint'
  'full|push|collector.txt|ci/rust,ci/polymarket-evidence-compiler-image,ploy/research-image-binaries,ploy/research-image-smoke,ci/deployment-artifacts'
)
for job_case in "${job_cases[@]}"; do
  IFS='|' read -r name event fixture expected expected_owning <<<"$job_case"
  output=$(run_case "$name" "$event" "$fixture")
  assert_jobs "$output" "$expected"
  assert_owning_packages "$output" "${expected_owning:-}"
  assert_flag "$output" selection_complete true
done

# Known ACK metadata helpers must select their owning shell/workflow contracts
# without setting Cargo-impact flags or selecting images on main push. The
# production image/dependency inputs retain their own build coverage.
ack_metadata_paths=(
  .github/scripts/classify-ack-research-job.sh
  .github/scripts/test-classify-ack-research-job.sh
  .github/scripts/wait-ack-research-receipt.sh
  .github/scripts/verify-ack-rust-batch.sh
  .github/scripts/wait-ack-rust-batch.sh
  .github/scripts/test-ack-rust-batch.sh
  .github/ack-ci/receipt-public-key.pub
  .github/ack-ci/PREFLIGHT.md
  .github/scripts/verify-ack-preflight.sh
  .github/scripts/test-ack-preflight-relay.sh
  .github/scripts/test-preflight-workflow-gate.sh
  .github/workflows/ack-flow-contracts.yml
)
release_metadata_paths=(
  .github/workflows/acr-publish.yml
  .github/scripts/test-acr-publish-workflow.sh
  .github/scripts/read-release-required-checks.sh
  .github/scripts/wait-release-required-checks.sh
  .github/scripts/research-image-release-artifact.sh
  .github/scripts/test-research-image-release-artifact.sh
  .github/scripts/verify-research-runner-binaries.sh
  .github/scripts/read-acr-publish-source.sh
  .github/scripts/select-acr-publish-source.sh
  .github/scripts/test-acr-publish-source-readback.sh
)
for kind in ack release; do
  if [[ $kind == ack ]]; then infrastructure_paths=("${ack_metadata_paths[@]}"); else infrastructure_paths=("${release_metadata_paths[@]}"); fi
  for path in "${infrastructure_paths[@]}"; do
    printf '%s\n' "$path" >"$tmp_dir/infrastructure.txt"
    for event in pull_request push; do
      scoped=$(run_case infrastructure "$event" infrastructure.txt)
      expected='ci/ci-contracts,ploy/workflow-lint'
      [[ $event == pull_request ]] && expected+=',ploy/commit-hygiene'
      assert_jobs "$scoped" "$expected"
      assert_owning_packages "$scoped" ''
      for flag in loop handoff json ondo collector control focused toolchain clippy_loop clippy_handoff; do
        assert_flag "$scoped" "$flag" false
      done
      # Policy-only source still receives repository security scans; it has no
      # dependency/toolchain impact and must not trigger Cargo or image audits.
      if [[ $path == *.md ]]; then
        assert_security_jobs "$scoped" 'security/secret-detection'
      else
        assert_security_jobs "$scoped" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
      fi
    done
  done
done
# All 20 actual PR 1266 paths, including this fixture, must remain policy-only
# on both PR and main push. Mixed source/image inputs retain their own gates.
for event in pull_request push; do
  ack_flow_pr=$(run_case "ack-flow-pr-$event" "$event" ack-flow-pr-1266.txt)
  expected='ci/ci-contracts,ploy/workflow-lint'
  [[ $event == pull_request ]] && expected+=',ploy/commit-hygiene'
  assert_jobs "$ack_flow_pr" "$expected"
  assert_security_jobs "$ack_flow_pr" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
  assert_owning_packages "$ack_flow_pr" ''
  for flag in loop handoff json ondo collector control focused toolchain clippy_loop clippy_handoff; do
    assert_flag "$ack_flow_pr" "$flag" false
  done
  cat "$fixtures/ack-flow-pr-1266.txt" "$fixtures/collector.txt" >"$tmp_dir/ack-flow-pr-with-collector.txt"
  ack_flow_mixed=$(run_case "ack-flow-collector-$event" "$event" ack-flow-pr-with-collector.txt)
  expected='ci/ci-contracts,ploy/workflow-lint,ci/rust,ci/polymarket-evidence-compiler-image,ci/deployment-artifacts'
  if [[ $event == pull_request ]]; then expected+=',ploy/commit-hygiene';
  else expected+=',ploy/research-image-binaries,ploy/research-image-smoke'; fi
  assert_jobs "$ack_flow_mixed" "$expected"
  for flag in loop collector control toolchain; do assert_flag "$ack_flow_mixed" "$flag" true; done
  cat "$fixtures/ack-flow-pr-1266.txt" "$fixtures/research-dockerfile.txt" >"$tmp_dir/ack-flow-pr-with-image.txt"
  ack_flow_image=$(run_case "ack-flow-image-$event" "$event" ack-flow-pr-with-image.txt)
  expected='ci/ci-contracts,ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans'
  [[ $event == pull_request ]] && expected+=',ploy/commit-hygiene'
  assert_jobs "$ack_flow_image" "$expected"
done

printf '%s\n' deployment/aliyun/research/Dockerfile.research-data >"$tmp_dir/research-data-dockerfile.txt"
for event in pull_request push; do
  image_scope=$(run_case research-data-dockerfile "$event" research-data-dockerfile.txt)
  expected='ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans'
  [[ $event == pull_request ]] && expected+=',ploy/commit-hygiene'
  assert_jobs "$image_scope" "$expected"
  for flag in loop handoff json ondo collector control focused toolchain; do assert_flag "$image_scope" "$flag" false; done
done
for helper in verify-research-controller-image.sh test-research-controller-image.sh; do
  printf '%s\n' ".github/scripts/$helper" >"$tmp_dir/controller-image-helper.txt"
  for event in pull_request push; do
    helper_scope=$(run_case "controller-helper-$event" "$event" controller-image-helper.txt)
    expected='ci/ci-contracts,ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans'
    [[ $event == pull_request ]] && expected+=',ploy/commit-hygiene'
    assert_jobs "$helper_scope" "$expected"
    assert_flag "$helper_scope" toolchain false
  done
done
# Adding an infrastructure helper to real Rust work must preserve the same
# source-graph suites; the cheap path is not a short circuit for mixed changes.
printf '%s\n' .github/scripts/wait-ack-research-receipt.sh >"$tmp_dir/ack-with-collector.txt"
cat "$fixtures/collector.txt" >>"$tmp_dir/ack-with-collector.txt"
mixed_ack=$(run_case ack-with-collector pull_request ack-with-collector.txt)
assert_jobs "$mixed_ack" 'ci/ci-contracts,ploy/workflow-lint,ploy/commit-hygiene,ci/rust,ci/polymarket-evidence-compiler-image,ci/deployment-artifacts'
for flag in loop collector control toolchain; do assert_flag "$mixed_ack" "$flag" true; done
# Unknown infrastructure is an incomplete plan, even alongside known source.
# It must emit no successful selection or compiler dispatch plan.
for unknown in .github/scripts/future-ack-unreviewed.sh .github/workflows/security.yml .github/actions/future/action.yml; do
  for event in pull_request push; do
    for mixed in false true; do
      printf '%s\n' "$unknown" >"$tmp_dir/unmapped-ci.txt"
      [[ $mixed == false ]] || cat "$fixtures/collector.txt" >>"$tmp_dir/unmapped-ci.txt"
      : >"$tmp_dir/unmapped.out"
      status=0
      "$selector" --event "$event" --changed-files "$tmp_dir/unmapped-ci.txt" \
        --metadata "$fixtures/metadata.fixture" --output "$tmp_dir/unmapped.out" \
        >"$tmp_dir/unmapped.stdout" 2>"$tmp_dir/unmapped.error" || status=$?
      [[ $status == 2 && ! -s $tmp_dir/unmapped.out && ! -s $tmp_dir/unmapped.stdout ]] || {
        echo 'unknown CI path emitted a successful/full dispatch plan' >&2; exit 1;
      }
      grep -Fq "unmapped CI path: $unknown; add its owning contract mapping before dispatch" "$tmp_dir/unmapped.error"
    done
  done
done

printf '%s\n' \
  .github/scripts/agent-worktree-preflight.sh \
  rust_hft/tools/future-rust-tool/src/lib.rs >"$tmp_dir/preflight-and-rust.txt"
preflight_and_rust=$(run_case preflight-and-rust pull_request preflight-and-rust.txt)
assert_jobs "$preflight_and_rust" 'ploy/commit-hygiene,ci/rust'
assert_owning_packages "$preflight_and_rust" 'future-rust-tool'
assert_flag "$preflight_and_rust" toolchain true

owning_package_cases=(
  'hft-binance-md|rust_hft/apps/binance-md/src/main.rs'
  'hft-replay|rust_hft/apps/replay/src/main.rs'
  'ws-connection-test|rust_hft/tools/ws_test/src/main.rs'
  'listing-monitor|rust_hft/tools/listing-monitor/src/main.rs'
  'future-rust-tool|rust_hft/tools/future-rust-tool/src/lib.rs'
)
for owning_case in "${owning_package_cases[@]}"; do
  IFS='|' read -r package path <<<"$owning_case"
  fixture="owning-$package.txt"
  printf '%s\n' "$path" >"$tmp_dir/$fixture"
  output=$(run_case "owning-$package" pull_request "$fixture")
  assert_jobs "$output" 'ci/rust'
  assert_owning_packages "$output" "$package"
  assert_flag "$output" toolchain true
done

# Architecture tests scan files outside the owning package's Cargo graph. A
# research-only edit must run them without pulling in PostgreSQL integration.
architecture_paths=(
  rust_hft/prediction-markets/crates/ploy-research/src/bin/monday-prediction-evaluator.rs
  rust_hft/prediction-markets/ploy-frontend/src/App.tsx
  rust_hft/prediction-markets/examples/openclaw/skill-ploy-rpc/bin/ployrpc
  rust_hft/prediction-markets/config/default.toml
  rust_hft/prediction-markets/docs/OPENCLAW_INTEGRATION.md
  rust_hft/prediction-markets/.env
  rust_hft/prediction-markets/tests/workspace_runtime_retirement.rs
  rust_hft/data-pipelines/adapters/adapter-polymarket/Cargo.toml
  rust_hft/execution-gateway/adapters/adapter-polymarket/Cargo.toml
  deployment/aliyun/polymarket-market-tape.service
  deployment/aliyun/polymarket_reference_collector.py
  products/ploy/src/lib.rs
)
for path in "${architecture_paths[@]}"; do
  printf '%s\n' "$path" >"$tmp_dir/architecture.txt"
  architecture_output=$(run_case architecture pull_request architecture.txt)
  grep -q '^jobs=.*[,/]architecture-contracts,' "$architecture_output"
done
if grep -q '^jobs=.*ploy/integration-regressions,' "$tmp_dir/evaluator.out"; then
  echo 'a research-only architecture check must not require PostgreSQL integration' >&2
  exit 1
fi
for output in "$tmp_dir/backtest.out" "$tmp_dir/rust-docs.out"; do
  if grep -q '^jobs=.*ploy/architecture-contracts,' "$output"; then
    echo 'unrelated source or docs selected prediction architecture contracts' >&2
    exit 1
  fi
done

# The measured engine and its upstream interfaces must select the lane that
# actually runs the release latency gate, including benchmark-only changes.
for path in \
  rust_hft/market-core/engine/benches/hotpath_latency_p99.rs \
  rust_hft/market-core/engine/src/lib.rs \
  rust_hft/market-core/ports/src/lib.rs \
  rust_hft/market-core/core/src/lib.rs; do
  printf '%s\n' "$path" >"$tmp_dir/engine-latency.txt"
  latency_output=$(run_case engine-latency pull_request engine-latency.txt)
  grep -q '^jobs=.*ci/rust-hft-engine-fast-lane,' "$latency_output"
done

printf '%s\n' \
  rust_hft/tools/collector/src/polymarket/reference.rs \
  rust_hft/tools/future-rust-tool/src/lib.rs >"$tmp_dir/known-and-future.txt"
known_and_future=$(run_case known-and-future pull_request known-and-future.txt)
assert_jobs "$known_and_future" 'ci/rust,ci/polymarket-evidence-compiler-image,ci/deployment-artifacts'
assert_owning_packages "$known_and_future" 'future-rust-tool'

printf '%s\n' rust_hft/apps/live/src/lib.rs rust_hft/apps/paper/src/main.rs \
  >"$tmp_dir/same-suite.txt"
printf '%s\n' rust_hft/data-pipelines/market-pipeline/src/market_import.rs >"$tmp_dir/market-pipeline.txt"
pipeline=$(run_case market-pipeline pull_request market-pipeline.txt)
assert_jobs "$pipeline" 'ci/research-foundation'
assert_flag "$pipeline" collector false
assert_flag "$pipeline" loop false
assert_flag "$pipeline" handoff false
printf '%s\n' .github/scripts/test-market-import.sh >"$tmp_dir/market-import-driver.txt"
import_driver=$(run_case market-import-driver pull_request market-import-driver.txt)
assert_jobs "$import_driver" 'ci/research-foundation,ci/ci-contracts,ploy/workflow-lint'

# This graph contains every member and local dependency of the six locked owners,
# including control -> backtest -> Alpha. Keep the smaller fixture for its existing
# scenarios, which deliberately model a different dependency graph.
owners_metadata="$fixtures/workspace-owners.fixture"
jq -e '.packages | length == 92 and (map(.name) | unique | length == 92)' "$owners_metadata" >/dev/null
workspace_inputs=(
  rust_hft/Cargo.toml
  rust_hft/workspaces.json
  rust_hft/shared/Cargo.toml
  rust_hft/data-pipelines/Cargo.toml
  rust_hft/research-core/Cargo.toml
  rust_hft/research-core/platform/Cargo.toml
  rust_hft/runtime/Cargo.toml
  rust_hft/prediction-markets/Cargo.toml
)
for event in pull_request push; do
  for path in "${workspace_inputs[@]}"; do
    printf '%s\n' "$path" >"$tmp_dir/owner-config.txt"
    owner_config=$(run_case "owner-config-$event" "$event" owner-config.txt "$owners_metadata")
    grep -Eq '^jobs=.*,(ci/research-foundation),' "$owner_config" || {
      printf 'workspace input omitted foundation: %s %s\n' "$event" "$path" >&2
      exit 1
    }
    assert_flag "$owner_config" selection_complete true
  done
  printf '%s\n' rust_hft/research-core/platform/src/build.rs >"$tmp_dir/control-owner.txt"
  control_owner=$(run_case "control-owner-$event" "$event" control-owner.txt "$owners_metadata")
  assert_owning_packages "$control_owner" hft-research-platform
  grep -Fqx 'loop_packages=,alpha-harness,' "$control_owner"
  grep -Eq '^jobs=.*,(ci/research-foundation),' "$control_owner"
  grep -Eq '^jobs=.*,(ploy/research-image-binaries),' "$control_owner"
  if [[ $event == push ]]; then grep -Eq '^jobs=.*,(ploy/research-image-smoke),' "$control_owner"; fi
  owner_docs=$(run_case "owner-docs-$event" "$event" docs.txt "$owners_metadata")
  assert_flag "$owner_docs" toolchain false
  if grep -Eq '^jobs=.*,(ci/rust|ci/research-foundation|ploy/research-image-binaries|ploy/research-image-smoke),' "$owner_docs"; then
    echo 'docs-only owner graph selected compilation' >&2; exit 1
  fi
done

same_suite=$(run_case same-suite pull_request same-suite.txt)
assert_jobs "$same_suite" 'ci/rust,ci/deployment-artifacts'
assert_owning_packages "$same_suite" ''
assert_flag "$same_suite" focused true

all_security_jobs='security/sast-semgrep,security/cargo-audit,security/secret-presence,security/license-check,security/clippy-strict,security/cargo-machete,security/secret-detection'
assert_security_jobs "$tmp_dir/docs.out" 'security/secret-detection'
assert_security_jobs "$tmp_dir/security-workflow.out" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
assert_security_jobs "$tmp_dir/security-workflow-push.out" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
assert_security_jobs "$tmp_dir/root-node.out" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
assert_security_jobs "$tmp_dir/unknown-nested.out" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
assert_security_jobs "$tmp_dir/lob-control.out" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
assert_security_jobs "$tmp_dir/rust-shell-script.out" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
assert_security_jobs "$tmp_dir/rust-deploy-collector.out" "${all_security_jobs/,security\/clippy-strict/}"
assert_security_jobs "$tmp_dir/trading-dockerfile-push.out" 'security/sast-semgrep,security/secret-presence,security/container-scan,security/secret-detection'
trading_develop="$tmp_dir/trading-dockerfile-develop.out"
GITHUB_REF=refs/heads/develop "$selector" --event push \
  --changed-files "$tmp_dir/trading-dockerfile.txt" \
  --metadata "$fixtures/metadata.fixture" --output "$trading_develop"
assert_jobs "$trading_develop" 'ci/deployment-artifacts'
assert_security_jobs "$trading_develop" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
security_workflow_develop="$tmp_dir/security-workflow-develop.out"
GITHUB_REF=refs/heads/develop "$selector" --event push \
  --changed-files "$tmp_dir/security-workflow.txt" \
  --metadata "$fixtures/metadata.fixture" --output "$security_workflow_develop"
assert_jobs "$security_workflow_develop" 'ci/ci-contracts,ploy/workflow-lint'
assert_security_jobs "$security_workflow_develop" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
assert_security_jobs "$tmp_dir/collector.out" "$all_security_jobs"
assert_security_jobs "$tmp_dir/unknown-root.out" "$all_security_jobs"
security_schedule="$tmp_dir/security-schedule.out"
"$selector" --event schedule --output "$security_schedule"
assert_jobs "$security_schedule" ''
assert_security_jobs "$security_schedule" "$all_security_jobs"
security_manual="$tmp_dir/security-manual.out"
"$selector" --event workflow_dispatch --output "$security_manual"
assert_security_jobs "$security_manual" "$all_security_jobs"

collector="$tmp_dir/collector.out"
for flag in loop collector control toolchain; do assert_flag "$collector" "$flag" true; done
for flag in handoff json ondo focused; do assert_flag "$collector" "$flag" false; done

rust_deploy_collector="$tmp_dir/rust-deploy-collector.out"
for flag in collector control toolchain; do assert_flag "$rust_deploy_collector" "$flag" true; done
for flag in loop handoff json ondo focused; do assert_flag "$rust_deploy_collector" "$flag" false; done

live=$(run_case live pull_request live.txt)
for flag in handoff json ondo focused toolchain; do assert_flag "$live" "$flag" true; done
for flag in loop collector control; do assert_flag "$live" "$flag" false; done

control=$(run_case control pull_request control.txt)
assert_jobs "$control" 'ploy/integration-regressions,ci/rust,ploy/safety-scans,ploy/architecture-contracts'
for flag in control toolchain; do assert_flag "$control" "$flag" true; done
for flag in loop handoff json ondo collector focused; do assert_flag "$control" "$flag" false; done

docs="$tmp_dir/docs.out"
for flag in loop handoff json ondo collector control focused toolchain; do
  assert_flag "$docs" "$flag" false
done

full="$tmp_dir/full.out"
for flag in loop collector control toolchain; do assert_flag "$full" "$flag" true; done
for flag in handoff json ondo focused; do assert_flag "$full" "$flag" false; done

ci_workflow="$script_dir/../workflows/ci.yml"
root_toolchain="$script_dir/../../rust-toolchain.toml"
grep -Fqx 'channel = "1.98.1"' "$root_toolchain"
for workflow in \
  "$ci_workflow" \
  "$script_dir/../workflows/release-rust.yml" \
  "$script_dir/../workflows/security-enabled.yml"; do
  # Action refs are immutable SHAs after the supply-chain review. Compiler
  # version pinning is independent from the action's tag spelling.
  stable_uses=$(grep -Fc 'dtolnay/rust-toolchain@' "$workflow")
  pinned_toolchains=$(grep -Fxc '          toolchain: 1.98.1' "$workflow")
  test "$stable_uses" -eq "$pinned_toolchains"
done
# Research software runs under the private ACK profile, so public metadata
# jobs carry neither a compiler container nor its cache/toolchain setup.
grep -Fq 'bash .github/scripts/build-research-release.sh' "$script_dir/../workflows/ploy-ci.yml"
# shellcheck disable=SC2016
always_condition='    if: ${{ always() }}'
grep -Fqx '    needs: selector' "$ci_workflow"
grep -Fqx "$always_condition" "$ci_workflow"
grep -Fqx "          if [[ \"\$SELECTOR_RESULT\" == success && \"\$SELECTED_COMPLETE\" == true ]] &&" "$ci_workflow"
grep -Fqx "             [[ \"\$SELECTED_JOBS\" =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,\$ ]] &&" "$ci_workflow"
grep -Fq 'CI selection failed or returned an invalid plan' "$ci_workflow"
grep -Fq "contains(needs.scope.outputs.jobs, ',ci/rust,')" "$ci_workflow"
[[ $(grep -Fxc '      owning_packages: ${{ steps.scope.outputs.owning_packages }}' "$ci_workflow") -eq 2 ]]
grep -Fqx '      - name: Summarize check plan' "$ci_workflow"
grep -Fq -- '- Collector control contract: %s' "$ci_workflow"
grep -Fqx '      CARGO_PROFILE_DEV_DEBUG: "0"' "$ci_workflow"
grep -Fqx '      CARGO_PROFILE_TEST_DEBUG: "0"' "$ci_workflow"
grep -Fqx '  rust_fast_gates:' "$ci_workflow"
grep -Fqx '      - rust_fast_gates' "$ci_workflow"
grep -Fqx '      RUSTC_WRAPPER: sccache' "$ci_workflow"
grep -Fqx '      SCCACHE_GHA_ENABLED: "false"' "$ci_workflow"
grep -Fqx '        uses: mozilla-actions/sccache-action@v0.0.10' "$ci_workflow"

# Job-block extraction: lines from '^  <name>:' up to (excluding) the next
# two-space top-level job key.
job_block() {
  awk -v job="^  $1:" '$0 ~ job {found=1; next} /^  [a-z_]+:/ {found=0} found' "$ci_workflow"
}
rust_job_block=$(job_block rust)
rust_shell_scripts_block=$(job_block rust_shell_scripts)
fast_gates_block=$(job_block rust_fast_gates)
scope_job_block=$(job_block scope)
control_job_block=$(job_block control_contracts)
[ -n "$rust_job_block" ]
[ -n "$rust_shell_scripts_block" ]
[ -n "$fast_gates_block" ]
[ -n "$scope_job_block" ]

# rust_fast_gates must preserve repository policy checks for both compiled
# Rust changes and lightweight Rust shell changes.
grep -Fq "if: \${{ contains(needs.scope.outputs.jobs, ',ci/rust,') || contains(needs.scope.outputs.jobs, ',ci/rust-shell-scripts,') }}" <<<"$fast_gates_block"
grep -Fq "contains(needs.scope.outputs.jobs, ',ci/rust,')" <<<"$rust_job_block"
grep -Fq 'check-collector-test-presence.sh' <<<"$rust_job_block"
grep -Fq "if: \${{ contains(needs.scope.outputs.jobs, ',ci/rust-shell-scripts,') }}" <<<"$rust_shell_scripts_block"
grep -Fq "uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4.3.1" <<<"$rust_shell_scripts_block"
grep -Fq "find rust_hft/scripts -type f -name '*.sh' -exec bash -n {} \\;" <<<"$rust_shell_scripts_block"
grep -Fq 'Enforce Rust-only research and runtime source' <<<"$fast_gates_block"
grep -Fq 'Python runtime or package-manager command' <<<"$fast_gates_block"

# sccache must be wired into EACH of the two heavy jobs (per-job presence,
# not a file-wide count).
grep -Fqx '      RUSTC_WRAPPER: sccache' <<<"$rust_job_block"
grep -Fqx '      SCCACHE_GHA_ENABLED: "false"' <<<"$rust_job_block"
grep -Fq 'uses: mozilla-actions/sccache-action@v0.0.10' <<<"$rust_job_block"
fast_lane_block=$(job_block rust_hft_engine_fast_lane)
grep -Fqx '      RUSTC_WRAPPER: sccache' <<<"$fast_lane_block"
grep -Fqx '      SCCACHE_GHA_ENABLED: "false"' <<<"$fast_lane_block"
grep -Fq 'uses: mozilla-actions/sccache-action@v0.0.10' <<<"$fast_lane_block"

# Suite placement is pinned both ways: fast-only work stays out of the heavy
# job, and each suite's required home is asserted positively.
if grep -Fq 'cargo-scoped.sh" fmt --check' <<<"$rust_job_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
if grep -Fq 'shellcheck' <<<"$rust_job_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
if grep -Fq 'test-rust-lob-control-plane.sh' <<<"$rust_job_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
if grep -Fq 'test-polymarket-market-recorder-release.sh' <<<"$fast_gates_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
if grep -Fq 'test-rust-lob-control-plane.sh' <<<"$fast_gates_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
if grep -Fq 'test-rust-lob-recovery-queue.sh' <<<"$fast_gates_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
if grep -Fq 'shellcheck' <<<"$fast_gates_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
grep -Fq 'bash ../.github/scripts/run-collector-control-contracts.sh' <<<"$control_job_block"
grep -Fq 'test-rust-lob-control-plane.sh' "$script_dir/run-collector-control-contracts.sh"
grep -Fq 'test-rust-lob-recovery-queue.sh' "$script_dir/run-collector-control-contracts.sh"
[[ $scope_job_block != *test-* && $scope_job_block != *shellcheck* ]]
grep -Fq 'test-monday-collector-health.sh' "$script_dir/run-collector-control-contracts.sh"
grep -Fq 'shellcheck' <<<"$control_job_block"
grep -Fq 'cargo-scoped.sh" fmt --check' <<<"$fast_gates_block"
grep -Fq 'test-polymarket-raw-ops-control-plane.sh' <<<"$rust_job_block"
grep -Fqx '      - name: Test directly changed Rust packages' "$ci_workflow"
grep -Fq 'cargo-scoped.sh" test "${args[@]}" --locked' <<<"$rust_job_block"

# The market-recorder release contract runs as its own parallel job (#568).
ci_gate_block=$(job_block ci-gate)
grep -Fqx '      - market_recorder_contract' <<<"$ci_gate_block"
grep -Fqx '      - rust_shell_scripts' <<<"$ci_gate_block"
recorder_block=$(job_block market_recorder_contract)
[ -n "$recorder_block" ]
grep -Fq 'test-polymarket-market-recorder-release.sh' <<<"$recorder_block"
grep -Fq "contains(needs.scope.outputs.jobs, ',ci/market-recorder-contract,')" <<<"$recorder_block"
if grep -Fq 'test-polymarket-market-recorder-release.sh' <<<"$rust_job_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
grep -Fq "if: \${{ (always() && needs.scope.outputs.toolchain == 'true') }}" <<<"$rust_job_block"

ploy_workflow="$script_dir/../workflows/ploy-ci.yml"
grep -Fqx "  group: prediction-markets-\${{ github.ref == 'refs/heads/main' && github.run_id || github.ref }}" "$ploy_workflow"
grep -Fqx "  cancel-in-progress: \${{ github.ref != 'refs/heads/main' }}" "$ploy_workflow"
[[ $(grep -Fxc '    branches: [main, develop]' "$ploy_workflow") -eq 2 ]]
if sed -n '/^  push:$/,/^  workflow_dispatch:$/p' "$ploy_workflow" \
  | grep -Eq '^    paths(-ignore)?:'; then
  echo 'Prediction Markets CI can skip a main SHA by path' >&2
  exit 1
fi
grep -Fqx '    needs: image-smoke-selector' "$ploy_workflow"
grep -Fqx "$always_condition" "$ploy_workflow"
grep -Fqx '        working-directory: .' "$ploy_workflow"
grep -Fqx "          if [[ \"\$SELECTOR_RESULT\" == success && \"\$SELECTED_COMPLETE\" == true ]] &&" "$ploy_workflow"
grep -Fqx "             [[ \"\$SELECTED_JOBS\" =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,\$ ]]; then" "$ploy_workflow"
for invalid_jobs in '' ci/rust; do
  [[ $invalid_jobs =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,$ ]] && exit 1
done
grep -Fq 'CI selection failed or returned an invalid plan' "$ploy_workflow"
grep -Fq "contains(needs.image-smoke-scope.outputs.jobs, ',ploy/rust-research-heavy,')" "$ploy_workflow"
grep -Fqx "            mapfile -d '' workflow_files < <(" "$ploy_workflow"
grep -Fq -- '--diff-filter=ACMR -z' "$ploy_workflow"
grep -Fqx '          if ((${#workflow_files[@]} == 0)); then' "$ploy_workflow"
grep -Fqx '          .github/scripts/lint-workflow-files.sh "${HOME}/go/bin/actionlint" "${workflow_files[@]}"' "$ploy_workflow"
grep -Fqx '          .github/scripts/test-workflow-queue-lint.sh "${HOME}/go/bin/actionlint"' "$ploy_workflow"
# Exercise the actual CI validator with valid and invalid metadata.
sed -n "/^          ruby -ryaml <<'RUBY'$/,/^          RUBY$/p" "$ploy_workflow" \
  | sed 's/^          //' >"$tmp_dir/validate-skills.sh"
test -s "$tmp_dir/validate-skills.sh"
metadata_case() {
  local label=$1 name=$2 description=$3 extra=$4 ui=$5 expected=$6 result=0
  local root="$tmp_dir/metadata-$label"
  local skill="$root/.agents/skills/$name"
  mkdir -p "$skill/agents"
  printf '%s\n' '---' "name: $name" "description: $description" "$extra" '---' 'Body' >"$skill/SKILL.md"
  printf '%s\n' "$ui" >"$skill/agents/openai.yaml"
  (cd "$root" && bash "$tmp_dir/validate-skills.sh") >"$root/result" 2>&1 || result=$?
  if [[ $expected == pass && $result != 0 || $expected == fail && $result == 0 ]]; then
    printf 'metadata case %s: expected %s, exit %s\n' "$label" "$expected" "$result" >&2
    cat "$root/result" >&2
    exit 1
  fi
}
metadata_case valid example Audit '' 'interface: {display_name: Example}' pass
metadata_case optional-interface example Audit '' 'interface: {}' pass
metadata_case underscore invalid_skill Audit '' 'interface: {}' fail
metadata_case long-name "$(printf '%065d' 0)" Audit '' 'interface: {}' fail
metadata_case angle-bracket example '"<audit>"' '' 'interface: {}' fail
metadata_case unknown-key example Audit 'unsupported: true' 'interface: {}' fail
metadata_case interface-type example Audit '' 'interface: broken' fail
metadata_case field-type example Audit '' 'interface: {display_name: 42}' fail
metadata_case short-description example Audit '' 'interface: {short_description: tiny}' fail
metadata_case prompt example Audit '' 'interface: {default_prompt: Audit this}' fail
metadata_case color example Audit '' 'interface: {brand_color: red}' fail
# sccache must use the #559/#566 pattern (sccache-action + per-job local
# cache, rustc/sccache-versioned rust-cache keys, continue-on-error fallback) in
# EVERY ploy-ci job that compiles Rust on the runner, and the homegrown
# actions/cache sccache block must stay removed.
if grep -Fq 'sccache --zero-stats' "$ploy_workflow"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
if grep -Fq 'cargo install sccache' "$ploy_workflow"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
if grep -Fq 'path: ~/.cache/sccache' "$ploy_workflow"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
ploy_job_block() {
  awk -v job="^  $1:" '$0 ~ job {found=1; next} /^  [a-z0-9-]+:/ {found=0} found' "$ploy_workflow"
}
for ploy_rust_job in \
  rust-control-plane \
  rust-runner-lean \
  rust-runner-full \
  rust-market-data \
  integration-regressions; do
  ploy_block=$(ploy_job_block "$ploy_rust_job")
  [ -n "$ploy_block" ]
  grep -Fqx '      RUSTC_WRAPPER: sccache' <<<"$ploy_block"
  grep -Fqx '      SCCACHE_GHA_ENABLED: "false"' <<<"$ploy_block"
  grep -Fqx '        uses: mozilla-actions/sccache-action@v0.0.10' <<<"$ploy_block"
  grep -Fqx '        continue-on-error: true' <<<"$ploy_block"
  grep -Fq "if: steps.sccache.outcome == 'failure'" <<<"$ploy_block"
  grep -Fq 'steps.cache-info.outputs.rust' <<<"$ploy_block"
  grep -Fq 'steps.cache-info.outputs.sccache' <<<"$ploy_block"
  if grep -Fq -- '}}-${{ github.sha }}' <<<"$ploy_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
done
# Native test/build jobs preserve domain coverage and have no resource relay.
ruby -ryaml - "$ci_workflow" "$script_dir/../workflows/security-enabled.yml" "$ploy_workflow" <<'RUBY'
ci,security,ploy=ARGV.map{|p| YAML.safe_load(File.read(p)).fetch('jobs')}
[ci,security,ploy].each do |jobs|
  abort 'active cloud execution relay' if jobs.to_s.include?('wait-ack-') || jobs.to_s.include?('ack_research')
end
abort 'missing selected Rust domain tests' unless %w[loop owning handoff json ondo collector control focused clippy_loop clippy_handoff].all?{|id|ci.fetch('rust').fetch('steps').any?{|s|s['id']==id}}
abort 'missing shared producer evidence' unless ci.fetch('rust').to_s.include?('write-ci-rust-evidence.sh')
abort 'Security duplicates Clippy on PR/push' unless security.fetch('clippy-strict').fetch('steps').select{|s|s.fetch('run','').include?('cargo-scoped.sh" clippy')}.all?{|s|s.fetch('if','').include?("github.event_name == 'schedule'")}
abort 'Clippy lacks same-run dependency' unless ci.fetch('clippy_strict').fetch('needs').include?('rust') && ci.fetch('clippy_strict').to_s.include?('verify-ci-rust-same-run.sh')
abort 'Security still polls native Clippy' if security.fetch('clippy-strict').to_s.include?('wait-ci-rust-evidence.sh')
abort 'smoke does not reuse binary job' unless ploy.fetch('research-image-smoke').fetch('needs').include?('research-image-binaries')
%w[research-image-binaries rust-format rust-research-heavy].each do |id|
  abort "native compiler absent #{id}" unless ploy.fetch(id).to_s.include?('dtolnay/rust-toolchain@')
end
RUBY

deletion_repo="$tmp_dir/deletion-repo"
mkdir -p "$deletion_repo/rust_hft/tools/collector/src"
git -C "$deletion_repo" init -q
touch "$deletion_repo/rust_hft/tools/collector/src/removed.rs"
git -C "$deletion_repo" add .
git -C "$deletion_repo" -c user.name=CI -c user.email=ci@example.invalid commit -qm base
deletion_base=$(git -C "$deletion_repo" rev-parse HEAD)
rm "$deletion_repo/rust_hft/tools/collector/src/removed.rs"
git -C "$deletion_repo" add -u
git -C "$deletion_repo" -c user.name=CI -c user.email=ci@example.invalid commit -qm deletion
deletion="$tmp_dir/deletion.out"
(cd "$deletion_repo" && "$selector" --event pull_request --base "$deletion_base" \
  --head HEAD --metadata "$fixtures/metadata.fixture" --output "$deletion")
for flag in loop collector control toolchain; do assert_flag "$deletion" "$flag" true; done

rename_repo="$tmp_dir/rename-repo"
mkdir -p "$rename_repo/rust_hft/tools/collector/src" "$rename_repo/docs"
git -C "$rename_repo" init -q
touch "$rename_repo/rust_hft/tools/collector/src/moved.rs"
git -C "$rename_repo" add .
git -C "$rename_repo" -c user.name=CI -c user.email=ci@example.invalid commit -qm base
rename_base=$(git -C "$rename_repo" rev-parse HEAD)
git -C "$rename_repo" mv rust_hft/tools/collector/src/moved.rs docs/moved.rs
git -C "$rename_repo" -c user.name=CI -c user.email=ci@example.invalid commit -qm rename
rename="$tmp_dir/rename.out"
(cd "$rename_repo" && "$selector" --event pull_request --base "$rename_base" \
  --head HEAD --metadata "$fixtures/metadata.fixture" --output "$rename")
for flag in loop collector control toolchain; do assert_flag "$rename" "$flag" true; done

gate="$script_dir/verify-ci-gate.sh"
printf '%s' '{"selector":{"result":"success"},"rust":{"result":"skipped"}}' | \
  bash "$gate" --expected-jobs ',,'
if printf '%s' '{"selector":{"result":"success"},"rust":{"result":"skipped"}}' | \
  bash "$gate" --expected-jobs ',ci/rust,' >/dev/null 2>&1; then
  echo 'CI gate accepted a skipped selected job' >&2
  exit 1
fi
printf '%s' '{"selector":{"result":"success"},"rust":{"result":"success"}}' | \
  bash "$gate" --expected-jobs ',ci/rust,'
for selected in ploy/architecture-contracts ci/rust-hft-engine-fast-lane ci/research-foundation; do
  job=${selected#*/}
  [[ $selected == ci/* ]] && job=${job//-/_}
  for state in missing skipped failure cancelled; do
    needs=$(jq -cn --arg job "$job" --arg state "$state" \
      'if $state == "missing" then {} else {($job): {result: $state}} end')
    if printf '%s' "$needs" | bash "$gate" --expected-jobs ",$selected," >/dev/null 2>&1; then
      printf 'CI gate accepted %s with state %s\n' "$selected" "$state" >&2
      exit 1
    fi
  done
  jq -cn --arg job "$job" '{($job): {result: "success"}}' | \
    bash "$gate" --expected-jobs ",$selected,"
done
printf '%s' '{"selector":{"result":"success"},"scope":{"result":"success"}}' | \
  bash "$gate" --job-prefix ci --expected-jobs ',ploy/workflow-lint,'
release_expected=',ci/rust,ci/polymarket-evidence-compiler-image,ploy/rust-research-heavy,ci/deployment-artifacts,'
release_needs='{"selector":{"result":"success"},"scope":{"result":"success"},"rust":{"result":"success"},"polymarket_evidence_compiler_image":{"result":"success"},"deployment_artifacts":{"result":"success"}}'
printf '%s' "$release_needs" | bash "$gate" --job-prefix ci --expected-jobs "$release_expected"
for image_state in missing skipped failure; do
  bad_image_needs=$(jq -cn --argjson needs "$release_needs" --arg state "$image_state" \
    '$needs | if $state == "missing" then del(.deployment_artifacts) else .deployment_artifacts.result=$state end')
  if printf '%s' "$bad_image_needs" | bash "$gate" --job-prefix ci --expected-jobs "$release_expected" >/dev/null 2>&1; then
    printf 'Monorepo gate accepted a %s production image check\n' "$image_state" >&2
    exit 1
  fi
done
unrelated_needs='{"image-smoke-selector":{"result":"success"},"image-smoke-scope":{"result":"success"},"rust-research-heavy":{"result":"failure"}}'
if printf '%s' "$unrelated_needs" | \
  bash "$gate" --job-prefix ploy --expected-jobs "$release_expected" >/dev/null 2>&1; then
  echo 'Prediction gate accepted an unrelated-lane failure' >&2
  exit 1
fi
if printf '%s' '{"security-selector":{"result":"success"},"security-scope":{"result":"success"},"cargo-machete":{"result":"skipped"}}' | \
  bash "$gate" --expected-jobs ',security/cargo-machete,' >/dev/null 2>&1; then
  echo 'CI gate accepted a skipped selected security job' >&2
  exit 1
fi
printf '%s' '{"security-selector":{"result":"success"},"security-scope":{"result":"success"},"cargo-machete":{"result":"success"}}' | \
  bash "$gate" --expected-jobs ',security/cargo-machete,'
if printf '%s' '{"selector":{"result":"skipped"},"rust":{"result":"skipped"}}' | \
  bash "$gate" --expected-jobs ',,' >/dev/null 2>&1; then
  echo 'CI gate accepted a skipped selector' >&2
  exit 1
fi
if printf '%s' '{"selector":{"result":"success"},"rust":{"result":"success"}}' | \
  bash "$gate" --expected-jobs ',ci/missing-job,' >/dev/null 2>&1; then
  echo 'CI gate accepted an unknown selected job' >&2
  exit 1
fi
if printf '%s' '{"selector":{"result":"failure"}}' | bash "$gate" >/dev/null 2>&1; then
  echo 'CI gate accepted a failed selected job' >&2
  exit 1
fi
if printf '%s' '{"selector":{"result":"cancelled"}}' | bash "$gate" >/dev/null 2>&1; then
  echo 'CI gate accepted a cancelled selected job' >&2
  exit 1
fi

grep -Fqx '  ci-gate:' "$ci_workflow"
grep -Fqx '    name: Monorepo CI gate' "$ci_workflow"
grep -Fqx '      - uses: actions/checkout@8ade135a41bc03ea155e62e844d188df1ea18608 # v4.1.0' "$ci_workflow"
grep -Fqx '          EXPECTED_JOBS: ${{ needs.scope.outputs.jobs }}' "$ci_workflow"
grep -Fqx "        run: printf '%s' \"\$GATE_NEEDS\" | bash .github/scripts/verify-ci-gate.sh --job-prefix ci --expected-jobs \"\$EXPECTED_JOBS\"" "$ci_workflow"
grep -Fqx '  prediction-markets-gate:' "$ploy_workflow"
grep -Fqx '    name: Prediction Markets CI gate' "$ploy_workflow"
grep -Fqx '      - uses: actions/checkout@8ade135a41bc03ea155e62e844d188df1ea18608 # v4.1.0' "$ploy_workflow"
grep -Fqx '          EXPECTED_JOBS: ${{ needs.image-smoke-scope.outputs.jobs }}' "$ploy_workflow"
grep -Fqx "        run: printf '%s' \"\$GATE_NEEDS\" | bash .github/scripts/verify-ci-gate.sh --job-prefix ploy --expected-jobs \"\$EXPECTED_JOBS\"" "$ploy_workflow"

security_workflow="$script_dir/../workflows/security-enabled.yml"
grep -Fqx '  security-selector:' "$security_workflow"
grep -Fqx '  security-scope:' "$security_workflow"
grep -Fqx '      - uses: actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803 # v6' "$security_workflow"
grep -Fq "contains(needs.security-scope.outputs.jobs, ',security/cargo-audit,')" "$security_workflow"
grep -Fq "contains(needs.security-scope.outputs.jobs, ',security/container-scan,')" "$security_workflow"
grep -Fqx '      - cargo-machete' "$security_workflow"
grep -Fqx '          EXPECTED_JOBS: ${{ needs.security-scope.outputs.jobs }}' "$security_workflow"
grep -Fqx "        run: printf '%s' \"\$GATE_NEEDS\" | bash .github/scripts/verify-ci-gate.sh --job-prefix security --expected-jobs \"\$EXPECTED_JOBS\"" "$security_workflow"
summary_upload_line=$(grep -nF '      - name: Upload summary' "$security_workflow" | cut -d: -f1)
security_gate_line=$(grep -nF '      - name: Require selected security jobs to pass' "$security_workflow" | cut -d: -f1)
((security_gate_line > summary_upload_line))

docker_publish_workflow="$script_dir/../workflows/docker-publish.yml"
expected_docker_publish_triggers=$(printf '%s\n' \
  '  workflow_run:' \
  '    workflows: ["Monorepo CI", "Prediction Markets CI", "Security & Quality (ENABLED)"]' \
  '    types: [completed]' \
  '    branches: [main]' \
  '  push:' \
  '    tags:' \
  "      - 'v*'" \
  '  workflow_dispatch:')
assert_docker_publish_triggers() {
  local trigger_block
  trigger_block=$(sed -n '/^  workflow_run:$/,/^  workflow_dispatch:$/p' "$1")
  [[ $trigger_block == "$expected_docker_publish_triggers" ]] || return 1
  grep -Fq 'bash .github/scripts/select-main-image-scope.sh "$SOURCE_SHA" "$plan"' "$1" &&
    grep -Fq 'any(.include[]; .name=="hft-core")' "$1"
}
assert_docker_publish_triggers "$docker_publish_workflow"

docker_publish_counterexample="$tmp_dir/docker-publish-extra-path.yml"
sed 's@.name=="hft-core"@.name=="research-runner"@' \
  "$docker_publish_workflow" >"$docker_publish_counterexample"
if assert_docker_publish_triggers "$docker_publish_counterexample"; then
  echo 'Docker Publish trigger contract accepted an unrelated path' >&2
  exit 1
fi

listing_monitor_workflow="$script_dir/../workflows/deploy-listing-monitor.yml"
expected_listing_monitor_triggers=$(printf '%s\n' 'on:' '  workflow_dispatch:')
assert_listing_monitor_triggers() {
  local trigger_block
  trigger_block=$(sed -n '/^on:$/,/^env:$/p' "$1" | sed '$d')
  [[ $trigger_block == "$expected_listing_monitor_triggers" ]]
}
assert_listing_monitor_triggers "$listing_monitor_workflow"

listing_monitor_counterexample="$tmp_dir/listing-monitor-push.yml"
awk '1; $0 == "on:" { print "  push:"; print "    branches: [main]" }' \
  "$listing_monitor_workflow" >"$listing_monitor_counterexample"
if assert_listing_monitor_triggers "$listing_monitor_counterexample"; then
  echo 'Listing Monitor trigger contract accepted an automatic push' >&2
  exit 1
fi

printf 'rust CI scope selector tests passed\n'

# Prediction-only work does not compile the unrelated CEX research/runtime lint
# profiles. Scheduled audits and unknown root changes retain full coverage.
for name in prediction-lock evaluator; do
  assert_flag "$tmp_dir/$name.out" clippy_loop false
  assert_flag "$tmp_dir/$name.out" clippy_handoff false
  if grep -q '^security_jobs=.*security/clippy-strict' "$tmp_dir/$name.out"; then
    echo 'prediction-only change selected unrelated root Clippy' >&2; exit 1
  fi
done
assert_flag "$security_schedule" clippy_loop true
assert_flag "$security_schedule" clippy_handoff true
assert_flag "$live" clippy_loop false
assert_flag "$live" clippy_handoff true
assert_flag "$collector" clippy_loop true

# Leaf research changes select the affected package, while shared-domain changes
# retain reverse-dependency coverage. The same list feeds tests and Clippy.
printf '%s\n' rust_hft/alpha-harness/app/src/main.rs >"$tmp_dir/alpha-leaf.txt"
output=$(run_case alpha-leaf pull_request alpha-leaf.txt)
assert_flag "$output" loop_packages ',alpha-harness,'
assert_flag "$output" clippy_loop true
output=$(run_case schedule schedule alpha-leaf.txt)
assert_flag "$output" loop_packages ',alpha-domain,alpha-store,alpha-engine,alpha-onnx-evaluator,alpha-harness,hft-harnessctl,hft-research-ml,'
