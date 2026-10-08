#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail

case ${1:-} in
  ''|--strategy-config|--production-scope) ;;
  *) printf 'unknown test scope: %s\n' "$1" >&2; exit 2 ;;
esac

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
printf '%s\n' .github/scripts/ci-owner-cache.sh >"$tmp_dir/ci-owner-cache.txt"
printf '%s\n' .github/scripts/test-ci-owner-cache.sh >"$tmp_dir/ci-owner-cache-test.txt"
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

# Keep test consumers while separating their release impact. The fixture includes
# formula's real dev edge and synthetic dev consumers for every image selector.
production_metadata="$fixtures/production-dependencies.fixture"
printf '%s\n' rust_hft/selector-fixtures/test-input/src/lib.rs >"$tmp_dir/dev-input.txt"
printf '%s\n' rust_hft/strategy-framework/strategies/formula/src/lib.rs >"$tmp_dir/formula-input.txt"
printf '%s\n' rust_hft/alpha-harness/engine/src/lib.rs >"$tmp_dir/engine-input.txt"
for event in pull_request push; do
  dev_scope=$(run_case "dev-only-$event" "$event" dev-input.txt "$production_metadata")
  for flag in loop handoff collector control toolchain; do assert_flag "$dev_scope" "$flag" true; done
  assert_owning_packages "$dev_scope" hft-ci-test-input
  assert_flag "$dev_scope" research_product none
  assert_flag "$dev_scope" production_trading_image false
  assert_flag "$dev_scope" production_collector_image false
  grep -Fqx 'loop_packages=,alpha-engine,alpha-harness,hft-cex-research-worker,' "$dev_scope"
  grep -Eq '^jobs=.*,(ci/rust),' "$dev_scope"
  grep -Eq '^jobs=.*,(ci/control-contracts),' "$dev_scope"
  grep -Eq '^jobs=.*,(ci/deployment-artifacts),' "$dev_scope"
  grep -Eq '^jobs=.*,(ploy/rust-research-heavy),' "$dev_scope"
  sed -n 's/^image_matrix=//p' "$dev_scope" | jq -e '.include == []' >/dev/null
  if grep -Eq '^jobs=.*,(ploy/research-image-(binaries|smoke)|ci/polymarket-evidence-compiler-image),' "$dev_scope"; then
    echo 'test-only consumers selected release compilation' >&2; exit 1
  fi

  formula_scope=$(run_case "formula-$event" "$event" formula-input.txt "$production_metadata")
  assert_owning_packages "$formula_scope" hft-strategy-formula
  assert_flag "$formula_scope" loop true
  assert_flag "$formula_scope" research_product none
  assert_flag "$formula_scope" production_trading_image true
  grep -Fqx 'loop_packages=,alpha-engine,alpha-harness,hft-cex-research-worker,' "$formula_scope"
  sed -n 's/^image_matrix=//p' "$formula_scope" | jq -e 'any(.include[]; .name == "deploy-paper")' >/dev/null

  # Normal, build, omitted and unfamiliar kinds cannot suppress releases.
  # Optional and target-specific edges also remain conservative.
  for kind in normal build missing opaque parallel optional-target; do
    jq --arg kind "$kind" '
      .packages[].dependencies |= if $kind == "parallel" then
        . + [.[] | select(.name == "hft-ci-test-input") | .kind = null]
      else map(if .name != "hft-ci-test-input" then .
        elif $kind == "missing" then del(.kind)
        elif $kind == "normal" then .kind = null
        elif $kind == "optional-target" then .kind = null | .optional = true | .target = "cfg(windows)"
        else .kind = $kind end)
      end' "$production_metadata" >"$tmp_dir/$kind.metadata"
    release_scope=$(run_case "release-$kind-$event" "$event" dev-input.txt "$tmp_dir/$kind.metadata")
    assert_flag "$release_scope" research_product cex-runner,controller,prediction-runner
    assert_flag "$release_scope" production_trading_image true
    assert_flag "$release_scope" production_collector_image true
    grep -Eq '^jobs=.*,(ploy/research-image-binaries),' "$release_scope"
    grep -Eq '^jobs=.*,(ci/polymarket-evidence-compiler-image),' "$release_scope"
    sed -n 's/^image_matrix=//p' "$release_scope" | jq -e 'any(.include[]; .name == "deploy-collector")' >/dev/null
  done

  engine_scope=$(run_case "engine-$event" "$event" engine-input.txt "$production_metadata")
  assert_flag "$engine_scope" research_product cex-runner,controller
  cat "$tmp_dir/engine-input.txt" >"$tmp_dir/engine-config.txt"
  printf '%s\n' rust_hft/prediction-markets/config/strategies/new-parameters.toml >>"$tmp_dir/engine-config.txt"
  mixed_scope=$(run_case "engine-config-$event" "$event" engine-config.txt "$production_metadata")
  assert_flag "$mixed_scope" research_product cex-runner,controller
  assert_flag "$mixed_scope" loop true
  grep -Eq '^jobs=.*,(ploy/strategy-config-contracts),' "$mixed_scope"
  if [[ $event == push ]]; then
    grep -Eq '^jobs=.*,(ploy/research-image-binaries),' "$mixed_scope"
    grep -Eq '^jobs=.*,(ploy/research-image-smoke),' "$mixed_scope"
  fi

  # Explicit source, image and host-control rules still override graph narrowing.
  for path in rust_hft/docker/Dockerfile rust_hft/deployment/docker/Dockerfile.trading \
    rust_hft/deployment/docker/Dockerfile.research rust_hft/.dockerignore \
    rust_hft/scripts/deploy-ecs-tools-collector.sh Makefile; do
    cat "$tmp_dir/dev-input.txt" >"$tmp_dir/explicit-image.txt"
    printf '%s\n' "$path" >>"$tmp_dir/explicit-image.txt"
    explicit_scope=$(run_case "explicit-$event" "$event" explicit-image.txt "$production_metadata")
    case "$path" in
      rust_hft/docker/Dockerfile)
        sed -n 's/^image_matrix=//p' "$explicit_scope" | jq -e 'any(.include[]; .name == "hft-core")' >/dev/null ;;
      rust_hft/deployment/docker/Dockerfile.trading)
        assert_flag "$explicit_scope" production_trading_image true ;;
      rust_hft/deployment/docker/Dockerfile.research)
        assert_flag "$explicit_scope" research_product cex-runner
        grep -Eq '^jobs=.*,(ploy/research-image-binaries),' "$explicit_scope" ;;
      *) assert_flag "$explicit_scope" production_collector_image true ;;
    esac
  done
done
manual_scope=$(run_case production-manual workflow_dispatch dev-input.txt "$production_metadata")
assert_flag "$manual_scope" research_product cex-runner,controller,prediction-runner
assert_flag "$manual_scope" production_trading_image true
assert_flag "$manual_scope" production_collector_image true
if [[ ${1:-} == --production-scope ]]; then
  printf 'production dependency scope contracts passed\n'
  exit 0
fi

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
  'ci-owner-cache|pull_request|ci-owner-cache.txt|ci/rust,ci/ci-contracts,ploy/workflow-lint,ploy/commit-hygiene,ploy/safety-scans,ci/deployment-artifacts'
  'ci-owner-cache-test|pull_request|ci-owner-cache-test.txt|ci/ci-contracts,ploy/workflow-lint'
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

# Parameter edits keep parsing and authority checks, without release products.
for event in pull_request push; do
  for path in \
    rust_hft/prediction-markets/config/strategies/02-pm5d-threelayer.live.toml \
    rust_hft/prediction-markets/config/strategies/new-parameters.toml \
    rust_hft/prediction-markets/config/default.toml; do
    printf '%s\n' "$path" >"$tmp_dir/strategy-config.txt"
    output=$(run_case "strategy-config-$event" "$event" strategy-config.txt)
    expected='ploy/strategy-config-contracts,ploy/architecture-contracts'
    [[ $event != pull_request ]] || expected="ploy/commit-hygiene,$expected"
    assert_jobs "$output" "$expected"
    assert_owning_packages "$output" ''
    assert_flag "$output" research_product none
    assert_flag "$output" toolchain false
    assert_flag "$output" selection_complete true
  done
done

# A parameter file must not acquire the root ploy owner during metadata lookup.
printf '%s\n' rust_hft/prediction-markets/crates/ploy-strategy-bundles/src/lib.rs >"$tmp_dir/strategy-source.txt"
source_scope=$(run_case strategy-source pull_request strategy-source.txt)
assert_jobs "$source_scope" 'ploy/commit-hygiene,ploy/rust-format,ploy/safety-scans,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/architecture-contracts'
source_jobs=$(sed -n 's/^jobs=,\(.*\),$/\1/p' "$source_scope")
cat "$tmp_dir/strategy-source.txt" >"$tmp_dir/strategy-mixed.txt"
printf '%s\n' rust_hft/prediction-markets/config/strategies/new-parameters.toml \
  rust_hft/prediction-markets/config/default.toml >>"$tmp_dir/strategy-mixed.txt"
mixed_scope=$(run_case strategy-mixed pull_request strategy-mixed.txt)
assert_jobs "$mixed_scope" "$source_jobs,ploy/strategy-config-contracts"
assert_flag "$mixed_scope" research_product "$(sed -n 's/^research_product=//p' "$source_scope")"

# The installer contract is CI policy code and must have an executable owner.
printf '%s\n' .github/workflows/test-install-ubuntu-packages.sh >"$tmp_dir/installer-contract.txt"
for event in pull_request push; do
  output=$(run_case "installer-contract-$event" "$event" installer-contract.txt)
  expected='ci/ci-contracts,ploy/workflow-lint'
  [[ $event != pull_request ]] || expected="$expected,ploy/commit-hygiene"
  assert_jobs "$output" "$expected"
  assert_owning_packages "$output" ''
  assert_flag "$output" research_product none
  assert_flag "$output" toolchain false
  assert_flag "$output" selection_complete true
done
bash "$script_dir/../workflows/test-install-ubuntu-packages.sh"
bash "$script_dir/test-strategy-config-contracts.sh"
if [[ ${1:-} == --strategy-config ]]; then
  printf 'strategy configuration selector contracts passed\n'
  exit 0
fi

# The fixed probability inference is a runtime owner with a real live-intake consumer.
printf '%s\n' rust_hft/strategy-framework/strategies/probability_reversal/src/lib.rs >"$tmp_dir/probability-strategy.txt"
probability_scope=$(run_case probability-strategy pull_request probability-strategy.txt)
assert_owning_packages "$probability_scope" hft-strategy-probability-reversal
assert_flag "$probability_scope" handoff true

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
  .github/scripts/publish-research-build-release.sh
  .github/scripts/select-research-oss-policy.jq
  .github/scripts/migrate-research-oss-policy.sh
  .github/scripts/test-migrate-research-oss-policy.sh
  .github/scripts/verify-research-runner-binaries.sh
  .github/scripts/read-acr-publish-source.sh
  .github/scripts/select-acr-publish-source.sh
  .github/scripts/test-acr-publish-source-readback.sh
  .github/workflows/release.yml
  .github/scripts/decide-release-once.sh
  .github/scripts/read-release-published.sh
  .github/scripts/release-orchestrator-admit.sh
  .github/scripts/test-release-once.sh
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
  expected='ci/ci-contracts'
  [[ $event == pull_request ]] && expected+=',ploy/commit-hygiene'
  assert_jobs "$image_scope" "$expected"
  grep -Fqx research_product=none "$image_scope"
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
# Controller assets retain their operational contracts when selecting only the
# controller image. Every copied script and template must exercise this boundary.
for asset in scripts/campaign-cycle-controller.sh scripts/campaign-job-watch.sh scripts/cex-materialization-entrypoint.sh k8s/campaign-cycle-controller-job.example.yaml; do
  printf '%s\n' "deployment/aliyun/research/$asset" >"$tmp_dir/controller-asset.txt"
  for event in pull_request push; do
    asset_scope=$(run_case "controller-asset-$event" "$event" controller-asset.txt)
    expected='ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans,ci/deployment-artifacts'
    [[ $event == pull_request ]] && expected+=',ploy/commit-hygiene'
    assert_jobs "$asset_scope" "$expected"
    assert_flag "$asset_scope" control true
    grep -qx 'research_product=controller' "$asset_scope"
    assert_flag "$asset_scope" toolchain false
  done
done
# Domain source changes select only their own runner. A neutral shared source
# selects both consumers through the dependency graph, not a mixed image.
for domain in cex prediction; do
  if [[ $domain == cex ]]; then
    path=rust_hft/apps/backtest/src/main.rs
    expected_product=cex-runner
  else
    path=rust_hft/prediction-markets/crates/ploy-research/src/lib.rs
    expected_product=prediction-runner
  fi
  printf '%s\n' "$path" >"$tmp_dir/domain-image.txt"
  domain_scope=$(run_case "$domain-image" push domain-image.txt)
  grep -Fqx "research_product=$expected_product" "$domain_scope"
  grep -Fq ',ploy/research-image-binaries,' "$domain_scope"
done
printf '%s\n' rust_hft/apps/backtest/src/main.rs rust_hft/prediction-markets/crates/ploy-research/src/lib.rs >"$tmp_dir/domain-image.txt"
domain_scope=$(run_case both-domains push domain-image.txt)
grep -Fqx research_product=cex-runner,prediction-runner "$domain_scope"
# The separate Prediction operator publishes its own domain. Shared cluster IO
# reaches both the operator and CEX consumers through their real dependencies.
"$script_dir/../../rust_hft/scripts/workspace-metadata.sh" >"$tmp_dir/operator-metadata.fixture"
jq -e '.packages[] | select(.name == "alpha-engine") | any(.dependencies[]; .name == "hft-strategy-formula" and .kind == "dev")' "$tmp_dir/operator-metadata.fixture" >/dev/null
for event in pull_request push; do
  native_formula=$(run_case "native-formula-$event" "$event" formula-input.txt "$tmp_dir/operator-metadata.fixture")
  assert_flag "$native_formula" research_product none
  assert_flag "$native_formula" loop true
  assert_flag "$native_formula" production_trading_image true
  grep -Eq '^loop_packages=.*[,](alpha-engine),' "$native_formula"
  assert_owning_packages "$native_formula" hft-strategy-formula
done
printf '%s\n' rust_hft/prediction-markets/crates/research-operator/src/dispatch.rs >"$tmp_dir/operator-image.txt"
operator_scope=$(run_case prediction-operator push operator-image.txt "$tmp_dir/operator-metadata.fixture")
grep -Fqx research_product=prediction-runner "$operator_scope"
grep -Fq ',ploy/research-image-binaries,' "$operator_scope"
printf '%s\n' rust_hft/research-core/dispatch-io/src/lib.rs >"$tmp_dir/shared-dispatch-image.txt"
dispatch_scope=$(run_case shared-dispatch push shared-dispatch-image.txt "$tmp_dir/operator-metadata.fixture")
grep -Fqx research_product=cex-runner,controller,prediction-runner "$dispatch_scope"
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

# Removing the engine dependency must not remove depth's actual 22-test gate.
"$script_dir/../../rust_hft/scripts/workspace-metadata.sh" >"$tmp_dir/depth-metadata.fixture"
printf '%s\n' rust_hft/market-core/binance-depth/src/book_sync.rs >"$tmp_dir/depth-leaf.txt"
depth_output=$(run_case depth-leaf pull_request depth-leaf.txt "$tmp_dir/depth-metadata.fixture")
grep -q '^jobs=.*ci/rust-hft-engine-fast-lane,' "$depth_output"

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
jq -e '.packages | length == 93 and (map(.name) | unique | length == 93)' "$owners_metadata" >/dev/null
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
  grep -Eq '^research_product=(cex-runner,)?controller(,prediction-runner)?$' "$control_owner"
  if [[ $event == push ]]; then
    grep -Eq '^jobs=.*,(ploy/research-image-binaries),' "$control_owner"
    grep -Eq '^jobs=.*,(ploy/research-image-smoke),' "$control_owner"
  fi
  grep -Fqx 'loop_packages=,alpha-harness,hft-cex-research-worker,' "$control_owner"
  grep -Eq '^jobs=.*,(ci/research-foundation),' "$control_owner"
  grep -Eq '^jobs=.*,(ploy/research-image-binaries),' "$control_owner"
  if [[ $event == push ]]; then grep -Eq '^jobs=.*,(ploy/research-image-smoke),' "$control_owner"; fi
  owner_docs=$(run_case "owner-docs-$event" "$event" docs.txt "$owners_metadata")
  assert_flag "$owner_docs" toolchain false
  if grep -Eq '^jobs=.*,(ci/rust|ci/research-foundation|ploy/research-image-binaries|ploy/research-image-smoke),' "$owner_docs"; then
    echo 'docs-only owner graph selected compilation' >&2; exit 1
  fi
done

# A future platform-only change must rebuild its direct controller product even
# when no Alpha/CEX package depends on that workspace.
jq '.packages |= map(.dependencies |= map(select(.name != "hft-research-platform")))' "$owners_metadata" >"$tmp_dir/isolated-platform.metadata"
for event in pull_request push; do
  isolated_platform=$(run_case "isolated-platform-$event" "$event" control-owner.txt "$tmp_dir/isolated-platform.metadata")
  assert_owning_packages "$isolated_platform" hft-research-platform
  grep -Fqx 'loop_packages=,,' "$isolated_platform"
  grep -Fqx 'research_product=controller' "$isolated_platform"
  grep -Eq '^jobs=.*,(ploy/research-image-binaries),' "$isolated_platform"
  grep -Eq '^jobs=.*,(ploy/research-image-smoke),' "$isolated_platform"
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
# Compilation cache is Swatinem/rust-cache only. sccache must not wrap rustc or
# write a GHA backend; pull requests restore main's cache and do not save.
for workflow in \
  "$ci_workflow" \
  "$script_dir/../workflows/ploy-ci.yml" \
  "$script_dir/../workflows/release-rust.yml" \
  "$script_dir/../workflows/security-enabled.yml" \
  "$script_dir/../workflows/acr-publish.yml" \
  "$script_dir/../workflows/docker-publish.yml" \
  "$script_dir/../workflows/docker-smoke.yml"; do
  if grep -Fq 'sccache' "$workflow" || grep -Fq 'RUSTC_WRAPPER' "$workflow" || grep -Fq 'mode=max' "$workflow"; then
    echo "sccache or gha mode=max must stay out of $workflow" >&2
    exit 1
  fi
done

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

# Each heavy Rust job saves Swatinem/rust-cache only from main.
grep -Fq 'uses: Swatinem/rust-cache@' <<<"$rust_job_block"
grep -Fq 'save-if: false' <<<"$rust_job_block"
grep -Fq "if: \${{ github.ref == 'refs/heads/main' && success() && needs.scope.outputs.toolchain == 'true' }}" <<<"$rust_job_block"
grep -Fq 'Save trusted workspace dependency cache' <<<"$rust_job_block"
grep -Fq 'save-if: true' <<<"$rust_job_block"
fast_lane_block=$(job_block rust_hft_engine_fast_lane)
grep -Fq 'uses: Swatinem/rust-cache@' <<<"$fast_lane_block"
grep -Fq "save-if: \${{ github.ref == 'refs/heads/main' }}" <<<"$fast_lane_block"

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
grep -Fq 'key: rust_hft-ci-owner-v1-rust-${{ steps.cache-info.outputs.rust }}' <<<"$rust_job_block"

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
# Prediction-market Rust jobs keep a shared rust-cache key and save only on main.
# The removed per-job sccache directory cache must not come back.
if grep -Fq 'path: ~/.cache/sccache' "$ploy_workflow"; then echo "unexpected sccache directory cache" >&2; exit 1; fi
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
  grep -Fq 'uses: Swatinem/rust-cache@' <<<"$ploy_block"
  grep -Fq "save-if: \${{ github.ref == 'refs/heads/main' }}" <<<"$ploy_block"
  grep -Fq 'steps.cache-info.outputs.rust' <<<"$ploy_block"
  if grep -Fq 'steps.cache-info.outputs.sccache' <<<"$ploy_block"; then echo "unexpected sccache cache key" >&2; exit 1; fi
  if grep -Fq -- '}}-${{ github.sha }}' <<<"$ploy_block"; then echo "unexpected duplicate CI command" >&2; exit 1; fi
done
# Docker GHA cache exports final layers only, and only from main. Pull requests
# keep cache-from so they can read that cache.
for workflow in \
  "$ci_workflow" \
  "$script_dir/../workflows/security-enabled.yml" \
  "$script_dir/../workflows/docker-smoke.yml" \
  "$script_dir/../workflows/docker-publish.yml" \
  "$script_dir/../workflows/acr-publish.yml"; do
  grep -Fq 'cache-from: type=gha' "$workflow"
  grep -Fq "github.ref == 'refs/heads/main'" "$workflow"
  grep -Fq 'mode=min' "$workflow"
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
config_job = ploy.fetch('strategy-config-contracts')
abort 'configuration checks must not start services' if config_job.key?('services')
abort 'configuration failures must fail their lane' if config_job['continue-on-error']
abort 'configuration parser is not selected' unless config_job.fetch('steps').any? { |step| step.fetch('run', '').include?('run-strategy-config-contracts.sh') && !step['continue-on-error'] }
abort 'configuration lane missing from required gate' unless ploy.fetch('prediction-markets-gate').fetch('needs').include?('strategy-config-contracts')
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
for selected in ploy/architecture-contracts ploy/strategy-config-contracts ci/rust-hft-engine-fast-lane ci/research-foundation; do
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

# Release wakeups, the single reusable entry, and the hft-core guard live in
# test-release-once.sh. This scope test still runs that contract.
bash "$script_dir/test-release-once.sh"

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
assert_flag "$output" loop_packages ',alpha-harness,hft-cex-research-worker,'
assert_flag "$output" clippy_loop true
output=$(run_case schedule schedule alpha-leaf.txt)
assert_flag "$output" loop_packages ',alpha-domain,alpha-store,alpha-engine,alpha-onnx-evaluator,alpha-harness,hft-cex-research-worker,hft-harnessctl,hft-research-ml,'
