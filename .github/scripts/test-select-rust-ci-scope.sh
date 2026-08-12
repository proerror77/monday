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
  local output="$tmp_dir/$name.out"
  local changed_file="$fixtures/$changed"
  [[ -f $changed_file ]] || changed_file="$tmp_dir/$changed"
  [[ $event == push ]] && ref=refs/heads/main
  GITHUB_REF=$ref "$selector" --event "$event" --changed-files "$changed_file" \
    --metadata "$fixtures/metadata.fixture" --output "$output"
  printf '%s\n' "$output"
}

printf '%s\n' package-lock.json >"$tmp_dir/root-node.txt"
printf '%s\n' .github/workflows/security.yml >"$tmp_dir/unknown-workflow.txt"
printf '%s\n' .github/workflows/security-enabled.yml >"$tmp_dir/security-workflow.txt"
printf '%s\n' .github/ISSUE_TEMPLATE/engineering-change.yml >"$tmp_dir/governance-template.txt"
printf '%s\n' docs/agents/issue-tracker.md >"$tmp_dir/governance-doc.txt"
printf '%s\n' Makefile >"$tmp_dir/unknown-root.txt"
printf '%s\n' config/risk.toml >"$tmp_dir/unknown-nested.txt"
printf '%s\n' rust_hft/deployment/docker/Dockerfile.trading >"$tmp_dir/trading-dockerfile.txt"
printf '%s\n' rust_hft/docs/README.md >"$tmp_dir/rust-docs.txt"
printf '%s\n' rust_hft/research-core/README.md >"$tmp_dir/package-readme.txt"

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
  actual=$(sed -n 's/^jobs=//p' "$output")
  [[ $actual == ,*, ]] || {
    printf '%s: jobs output must use exact comma-delimited membership: %s\n' "$output" "$actual" >&2
    exit 1
  }
  actual=${actual#,}
  actual=${actual%,}
  [[ $actual == "$expected" ]] || {
    printf '%s: expected jobs=%s, got jobs=%s\n' "$output" "$expected" "$actual" >&2
    exit 1
  }
}

assert_security_jobs() {
  local output=$1 expected=$2 actual
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
  'collector|pull_request|collector.txt|ci/rust,ci/polymarket-evidence-compiler-image'
  'pinned-aliyun|pull_request|pinned-aliyun.txt|ploy/integration-regressions,ci/rust'
  'pinned-aliyun-push|push|pinned-aliyun.txt|ploy/integration-regressions,ci/rust'
  'future-aliyun-pin|pull_request|future-aliyun-pin.txt|ploy/integration-regressions,ci/rust'
  'future-aliyun-markdown-pin|pull_request|future-aliyun-markdown-pin.txt|ploy/integration-regressions,ci/rust'
  'evaluator|pull_request|evaluator.txt|ploy/commit-hygiene,ploy/rust-format,ploy/safety-scans,ploy/rust-research-heavy'
  'shared-prediction|pull_request|shared-prediction.txt|ploy/commit-hygiene,ploy/rust-format,ploy/safety-scans,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'prediction-lock|pull_request|prediction-lock.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'research-dockerfile|pull_request|research-dockerfile.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans'
  'unknown-docker|pull_request|unknown-docker.txt|ci/rust,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'prediction-workflow|pull_request|prediction-workflow.txt|ploy/commit-hygiene,ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'root-node|pull_request|root-node.txt|ci/node-install'
  'security-workflow|pull_request|security-workflow.txt|ploy/commit-hygiene,ploy/workflow-lint'
  'security-workflow-push|push|security-workflow.txt|ploy/workflow-lint'
  'governance-template|pull_request|governance-template.txt|ploy/commit-hygiene,ploy/workflow-lint'
  'governance-doc|pull_request|governance-doc.txt|ploy/commit-hygiene,ploy/workflow-lint'
  'unknown-workflow|pull_request|unknown-workflow.txt|ci/rust,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,ploy/workflow-lint,ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'unknown-root|pull_request|unknown-root.txt|ci/rust,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'unknown-nested|pull_request|unknown-nested.txt|'
  'rust-docs|pull_request|rust-docs.txt|'
  'package-readme|pull_request|package-readme.txt|'
  'docs|pull_request|docs.txt|'
  'unknown-prediction|pull_request|unknown-prediction.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'mixed-prediction|pull_request|mixed-prediction.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans,ploy/rust-format,ploy/rust-research-heavy'
  'frontend|pull_request|frontend.txt|ploy/commit-hygiene,ploy/safety-scans,ploy/frontend'
  'backtest|pull_request|backtest.txt|ploy/research-image-binaries'
  'live-push|push|live.txt|ci/rust,ci/deployment-artifacts'
  'trading-dockerfile-push|push|trading-dockerfile.txt|ci/deployment-artifacts'
  'research-deployment-push|push|research-deployment.txt|ci/deployment-artifacts,ploy/research-image-binaries,ploy/research-image-smoke'
  'acr-workflow-push|push|acr-workflow.txt|ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke'
  'full|push|collector.txt|ci/rust,ci/polymarket-evidence-compiler-image,ploy/research-image-binaries,ploy/research-image-smoke'
)
for job_case in "${job_cases[@]}"; do
  IFS='|' read -r name event fixture expected <<<"$job_case"
  output=$(run_case "$name" "$event" "$fixture")
  assert_jobs "$output" "$expected"
  assert_flag "$output" selection_complete true
done

all_security_jobs='security/sast-semgrep,security/cargo-audit,security/secret-presence,security/license-check,security/clippy-strict,security/cargo-machete,security/secret-detection'
assert_security_jobs "$tmp_dir/docs.out" 'security/secret-detection'
assert_security_jobs "$tmp_dir/security-workflow.out" "$all_security_jobs"
assert_security_jobs "$tmp_dir/security-workflow-push.out" "$all_security_jobs,security/container-scan"
assert_security_jobs "$tmp_dir/root-node.out" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
assert_security_jobs "$tmp_dir/unknown-nested.out" 'security/sast-semgrep,security/secret-presence,security/secret-detection'
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
assert_jobs "$security_workflow_develop" 'ploy/workflow-lint'
assert_security_jobs "$security_workflow_develop" "$all_security_jobs"
assert_security_jobs "$tmp_dir/collector.out" "$all_security_jobs"
assert_security_jobs "$tmp_dir/unknown-workflow.out" "$all_security_jobs"
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

live=$(run_case live pull_request live.txt)
for flag in handoff json ondo focused toolchain; do assert_flag "$live" "$flag" true; done
for flag in loop collector control; do assert_flag "$live" "$flag" false; done

control=$(run_case control pull_request control.txt)
assert_jobs "$control" 'ploy/integration-regressions,ci/rust'
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
# shellcheck disable=SC2016
always_condition='    if: ${{ always() }}'
grep -Fqx '    needs: selector' "$ci_workflow"
grep -Fqx "$always_condition" "$ci_workflow"
grep -Fqx "          if [[ \"\$SELECTOR_RESULT\" == success && \"\$SELECTED_COMPLETE\" == true ]] &&" "$ci_workflow"
grep -Fqx "             [[ \"\$SELECTED_JOBS\" =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,\$ ]] &&" "$ci_workflow"
grep -Fq "'jobs=,ci/rust,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,'" "$ci_workflow"
grep -Fq "contains(needs.scope.outputs.jobs, ',ci/rust,')" "$ci_workflow"
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
fast_gates_block=$(job_block rust_fast_gates)
[ -n "$rust_job_block" ]
[ -n "$fast_gates_block" ]

# rust_fast_gates must carry the same scope condition as the heavy rust job,
# checked inside its own block (a file-wide substring match would pass even
# if the condition were deleted from the fast job and gate enforcement
# silently bypassed).
grep -Fq "if: \${{ contains(needs.scope.outputs.jobs, ',ci/rust,') }}" <<<"$fast_gates_block"
grep -Fq "if: \${{ contains(needs.scope.outputs.jobs, ',ci/rust,') }}" <<<"$rust_job_block"

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
! grep -Fq 'cargo fmt --check' <<<"$rust_job_block"
! grep -Fq 'shellcheck' <<<"$rust_job_block"
! grep -Fq 'test-rust-lob-control-plane.sh' <<<"$rust_job_block"
! grep -Fq 'test-polymarket-market-recorder-release.sh' <<<"$fast_gates_block"
grep -Fq 'test-rust-lob-control-plane.sh' <<<"$fast_gates_block"
grep -Fq 'shellcheck' <<<"$fast_gates_block"
grep -Fq 'cargo fmt --check' <<<"$fast_gates_block"
grep -Fq 'test-polymarket-raw-ops-control-plane.sh' <<<"$rust_job_block"

# The market-recorder release contract runs as its own parallel job (#568).
ci_gate_block=$(job_block ci-gate)
grep -Fqx '      - market_recorder_contract' <<<"$ci_gate_block"
recorder_block=$(job_block market_recorder_contract)
[ -n "$recorder_block" ]
grep -Fq 'test-polymarket-market-recorder-release.sh' <<<"$recorder_block"
! grep -Fq 'test-polymarket-market-recorder-release.sh' <<<"$rust_job_block"
grep -Fqx "        if: always() && needs.scope.outputs.toolchain == 'true'" "$ci_workflow"

ploy_workflow="$script_dir/../workflows/ploy-ci.yml"
grep -Fqx "  group: prediction-markets-\${{ github.ref == 'refs/heads/main' && github.run_id || github.ref }}" "$ploy_workflow"
grep -Fqx "  cancel-in-progress: \${{ github.ref != 'refs/heads/main' }}" "$ploy_workflow"
[[ $(grep -Fxc '    branches: [main, develop]' "$ploy_workflow") -eq 2 ]]
grep -Fqx '      - "deployment/aliyun/**"' "$ploy_workflow"
grep -Fqx '    needs: image-smoke-selector' "$ploy_workflow"
grep -Fqx "$always_condition" "$ploy_workflow"
grep -Fqx '        working-directory: .' "$ploy_workflow"
grep -Fqx "          if [[ \"\$SELECTOR_RESULT\" == success && \"\$SELECTED_COMPLETE\" == true ]] &&" "$ploy_workflow"
grep -Fqx "             [[ \"\$SELECTED_JOBS\" =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,\$ ]]; then" "$ploy_workflow"
for invalid_jobs in '' ci/rust; do
  [[ $invalid_jobs =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,$ ]] && exit 1
done
grep -Fq "'jobs=,ploy/commit-hygiene,ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions,'" "$ploy_workflow"
grep -Fq "contains(needs.image-smoke-scope.outputs.jobs, ',ploy/rust-research-heavy,')" "$ploy_workflow"
grep -Fqx "            mapfile -d '' workflow_files < <(" "$ploy_workflow"
grep -Fq -- '--diff-filter=ACMR -z' "$ploy_workflow"
grep -Fqx '          if ((${#workflow_files[@]} == 0)); then' "$ploy_workflow"
grep -Fqx '          "${HOME}/go/bin/actionlint" -color "${workflow_files[@]}"' "$ploy_workflow"
# sccache must use the #559/#566 pattern (sccache-action + per-job local
# cache, rustc/sccache-versioned rust-cache keys, continue-on-error fallback) in
# EVERY ploy-ci job that compiles Rust on the runner, and the homegrown
# actions/cache sccache block must stay removed.
! grep -Fq 'sccache --zero-stats' "$ploy_workflow"
! grep -Fq 'cargo install sccache' "$ploy_workflow"
! grep -Fq 'path: ~/.cache/sccache' "$ploy_workflow"
ploy_job_block() {
  awk -v job="^  $1:" '$0 ~ job {found=1; next} /^  [a-z0-9-]+:/ {found=0} found' "$ploy_workflow"
}
for ploy_rust_job in \
  research-image-binaries \
  rust-control-plane \
  rust-runner-lean \
  rust-runner-full \
  rust-market-data \
  rust-research-heavy \
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
  ! grep -Fq -- '}}-${{ github.sha }}' <<<"$ploy_block"
done
research_image_block=$(ploy_job_block research-image-binaries)
grep -Fqx '    timeout-minutes: 45' <<<"$research_image_block"
! grep -Fq 'SCCACHE_GHA_RW_MODE' <<<"$research_image_block"

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
printf '%s' '{"selector":{"result":"success"},"scope":{"result":"success"}}' | \
  bash "$gate" --job-prefix ci --expected-jobs ',ploy/workflow-lint,'
release_expected=',ci/rust,ci/polymarket-evidence-compiler-image,ploy/rust-research-heavy,'
release_needs='{"selector":{"result":"success"},"scope":{"result":"success"},"rust":{"result":"success"},"polymarket_evidence_compiler_image":{"result":"success"}}'
printf '%s' "$release_needs" | bash "$gate" --job-prefix ci --expected-jobs "$release_expected"
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
  '  push:' \
  '    branches: [main]' \
  '    tags:' \
  "      - 'v*'" \
  '    paths:' \
  '      - "rust_hft/**"' \
  '      - ".github/workflows/docker-publish.yml"' \
  '  workflow_dispatch:')
assert_docker_publish_triggers() {
  local trigger_block
  trigger_block=$(sed -n '/^  push:$/,/^  workflow_dispatch:$/p' "$1")
  [[ $trigger_block == "$expected_docker_publish_triggers" ]]
}
assert_docker_publish_triggers "$docker_publish_workflow"

docker_publish_counterexample="$tmp_dir/docker-publish-extra-path.yml"
awk '1; $0 == "      - \".github/workflows/docker-publish.yml\"" { print "      - \"docs/**\"" }' \
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
