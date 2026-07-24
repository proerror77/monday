#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
selector="$script_dir/select-rust-ci-scope.sh"
fixtures="$script_dir/fixtures/rust-ci-scope"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

run_case() {
  local name=$1 event=$2 changed=$3
  local output="$tmp_dir/$name.out"
  local changed_file="$fixtures/$changed"
  [[ -f $changed_file ]] || changed_file="$tmp_dir/$changed"
  "$selector" --event "$event" --changed-files "$changed_file" \
    --metadata "$fixtures/metadata.fixture" --output "$output"
  printf '%s\n' "$output"
}

printf '%s\n' package-lock.json >"$tmp_dir/root-node.txt"
printf '%s\n' .github/workflows/security.yml >"$tmp_dir/unknown-workflow.txt"
printf '%s\n' Makefile >"$tmp_dir/unknown-root.txt"
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

job_cases=(
  'collector|pull_request|collector.txt|ci/rust,ci/polymarket-evidence-compiler-image'
  'evaluator|pull_request|evaluator.txt|ploy/commit-hygiene,ploy/rust-format,ploy/safety-scans,ploy/rust-research-heavy'
  'shared-prediction|pull_request|shared-prediction.txt|ploy/commit-hygiene,ploy/rust-format,ploy/safety-scans,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'prediction-lock|pull_request|prediction-lock.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'research-dockerfile|pull_request|research-dockerfile.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans'
  'unknown-docker|pull_request|unknown-docker.txt|ci/rust,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'prediction-workflow|pull_request|prediction-workflow.txt|ploy/commit-hygiene,ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'root-node|pull_request|root-node.txt|ci/node-install'
  'unknown-workflow|pull_request|unknown-workflow.txt|ci/rust,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,ploy/workflow-lint,ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'unknown-root|pull_request|unknown-root.txt|ci/rust,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'rust-docs|pull_request|rust-docs.txt|'
  'package-readme|pull_request|package-readme.txt|'
  'docs|pull_request|docs.txt|'
  'unknown-prediction|pull_request|unknown-prediction.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions'
  'mixed-prediction|pull_request|mixed-prediction.txt|ploy/commit-hygiene,ploy/research-image-binaries,ploy/research-image-smoke,ploy/safety-scans,ploy/rust-format,ploy/rust-research-heavy'
  'frontend|pull_request|frontend.txt|ploy/commit-hygiene,ploy/safety-scans,ploy/frontend'
  'backtest|pull_request|backtest.txt|ploy/research-image-binaries'
  'live-push|push|live.txt|ci/rust,ci/deployment-artifacts'
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

collector="$tmp_dir/collector.out"
for flag in loop collector control toolchain; do assert_flag "$collector" "$flag" true; done
for flag in handoff json ondo focused; do assert_flag "$collector" "$flag" false; done

live=$(run_case live pull_request live.txt)
for flag in handoff json ondo focused toolchain; do assert_flag "$live" "$flag" true; done
for flag in loop collector control; do assert_flag "$live" "$flag" false; done

control=$(run_case control pull_request control.txt)
assert_jobs "$control" 'ci/rust'
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
grep -Fqx "             [[ \"\$SELECTED_JOBS\" =~ ^,(|[a-z0-9/-]+(,[a-z0-9/-]+)*),\$ ]] &&" "$ci_workflow"
grep -Fq "'jobs=,ci/rust,ci/deployment-artifacts,ci/polymarket-evidence-compiler-image,ci/rust-hft-engine-fast-lane,ci/node-install,'" "$ci_workflow"
grep -Fq "contains(needs.scope.outputs.jobs, ',ci/rust,')" "$ci_workflow"

ploy_workflow="$script_dir/../workflows/ploy-ci.yml"
grep -Fqx '    needs: image-smoke-selector' "$ploy_workflow"
grep -Fqx "$always_condition" "$ploy_workflow"
grep -Fqx '        working-directory: .' "$ploy_workflow"
grep -Fqx "          if [[ \"\$SELECTOR_RESULT\" == success && \"\$SELECTED_COMPLETE\" == true ]] &&" "$ploy_workflow"
grep -Fqx "             [[ \"\$SELECTED_JOBS\" =~ ^,(|[a-z0-9/-]+(,[a-z0-9/-]+)*),\$ ]]; then" "$ploy_workflow"
for invalid_jobs in '' ci/rust; do
  [[ $invalid_jobs =~ ^,(|[a-z0-9/-]+(,[a-z0-9/-]+)*),$ ]] && exit 1
done
grep -Fq "'jobs=,ploy/commit-hygiene,ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke,ploy/rust-format,ploy/safety-scans,ploy/audit,ploy/rust-control-plane,ploy/rust-runner-lean,ploy/rust-runner-full,ploy/rust-market-data,ploy/rust-research-heavy,ploy/frontend,ploy/integration-regressions,'" "$ploy_workflow"
grep -Fq "contains(needs.image-smoke-scope.outputs.jobs, ',ploy/rust-research-heavy,')" "$ploy_workflow"

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

printf 'rust CI scope selector tests passed\n'
