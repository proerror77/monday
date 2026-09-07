#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
selector="$script_dir/select-acr-publish-source.sh"
check_reader="$script_dir/read-acr-required-checks.sh"
artifact="$script_dir/research-image-release-artifact.sh"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

main_sha=1111111111111111111111111111111111111111
other_sha=2222222222222222222222222222222222222222
green_admission=(
  --main-sha "$main_sha"
  --monorepo-conclusion success
  --prediction-conclusion success
  --security-conclusion success
)

assert_source() {
  local name=$1 expected=$2
  shift 2
  local output="$tmp_dir/$name.out"
  "$selector" "$@" --output "$output"
  diff -u <(printf '%s\n' "$expected") "$output"
}

assert_source automated $'publish_target=research-runner\nresearch_mode=artifact\nsource_sha=1111111111111111111111111111111111111111\nartifact_run_id=1234' \
  --event workflow_run --conclusion success --source-event push --head-branch main \
  --head-sha "$main_sha" --run-id 1234 \
  --binaries-conclusion success --smoke-conclusion success \
  "${green_admission[@]}"
assert_source irrelevant $'publish_target=none\nresearch_mode=none\nsource_sha=1111111111111111111111111111111111111111\nartifact_run_id=1234' \
  --event workflow_run --conclusion success --source-event push --head-branch main \
  --head-sha "$main_sha" --run-id 1234 \
  --binaries-conclusion skipped --smoke-conclusion skipped
assert_source manual-rebuild $'publish_target=research-runner\nresearch_mode=rebuild\nsource_sha=1111111111111111111111111111111111111111\nartifact_run_id=5678' \
  --event workflow_dispatch --target research-runner --rebuild true \
  --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 \
  "${green_admission[@]}"
assert_source manual-nonresearch $'publish_target=hft-trading\nresearch_mode=none\nsource_sha=1111111111111111111111111111111111111111\nartifact_run_id=5678' \
  --event workflow_dispatch --target hft-trading --rebuild false \
  --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 \
  "${green_admission[@]}"
assert_source manual-raw-ops-alias $'publish_target=binance-lob-archiver\nresearch_mode=none\nsource_sha=1111111111111111111111111111111111111111\nartifact_run_id=5678' \
  --event workflow_dispatch --target polymarket-raw-ops --rebuild false \
  --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 \
  "${green_admission[@]}"
assert_source source-test $'publish_target=research-source-test\nresearch_mode=none\nsource_sha=1111111111111111111111111111111111111111\nartifact_run_id=5678\nsource_test_profile=binance-bstocks-attestation\nsource_test_tag=source-test-1111111111111111111111111111111111111111-binance-bstocks-attestation' \
  --event workflow_dispatch --target research-source-test --rebuild false \
  --source-test-sha "$main_sha" --source-test-profile binance-bstocks-attestation --current-ref refs/heads/main \
  --current-sha "$main_sha" --current-run-id 5678 --main-sha "$main_sha"

assert_source_test_identity() {
  local name=$1 profile=$2
  local output="$tmp_dir/$name.out" tag selected_profile
  "$selector" --event workflow_dispatch --target research-source-test --rebuild false \
    --source-test-sha "$main_sha" --source-test-profile "$profile" \
    --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 \
    --main-sha "$main_sha" \
    --output "$output"
  selected_profile=$(sed -n 's/^source_test_profile=//p' "$output")
  tag=$(sed -n 's/^source_test_tag=//p' "$output")
  test "$selected_profile" = "$profile"
  test "$tag" = "source-test-$main_sha-$profile"
}

assert_source_test_identity source-test-binance binance-bstocks-attestation
assert_source_test_identity source-test-bybit bybit-spot
assert_source_test_identity source-test-binance-repeat binance-bstocks-attestation
binance_source_test_tag=$(sed -n 's/^source_test_tag=//p' "$tmp_dir/source-test-binance.out")
bybit_source_test_tag=$(sed -n 's/^source_test_tag=//p' "$tmp_dir/source-test-bybit.out")
binance_repeat_source_test_tag=$(sed -n 's/^source_test_tag=//p' "$tmp_dir/source-test-binance-repeat.out")
test "$binance_source_test_tag" != "$bybit_source_test_tag"
test "$binance_source_test_tag" = "$binance_repeat_source_test_tag"

for rejected in failed-run pull-request-run branch-run automated-stale-main manual-nonmain manual-stale-main required-missing required-pending required-skipped required-failed implicit-rebuild source-test-missing-sha source-test-nonmain source-test-untrusted-sha source-test-stale-main source-test-rebuild source-test-invalid-profile source-test-on-runtime automated-source-test; do
  case "$rejected" in
    failed-run) args=(--event workflow_run --conclusion failure --source-event push --head-branch main --head-sha "$main_sha" --run-id 1234) ;;
    pull-request-run) args=(--event workflow_run --conclusion success --source-event pull_request --head-branch main --head-sha "$main_sha" --run-id 1234) ;;
    branch-run) args=(--event workflow_run --conclusion success --source-event push --head-branch develop --head-sha "$main_sha" --run-id 1234) ;;
    automated-stale-main) args=(--event workflow_run --conclusion success --source-event push --head-branch main --head-sha "$other_sha" --run-id 1234 --binaries-conclusion success --smoke-conclusion success "${green_admission[@]}") ;;
    manual-nonmain) args=(--event workflow_dispatch --target hft-trading --rebuild false --current-ref refs/heads/codex/example --current-sha "$other_sha" --current-run-id 5678) ;;
    manual-stale-main) args=(--event workflow_dispatch --target hft-trading --rebuild false --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678 "${green_admission[@]}") ;;
    required-missing) args=(--event workflow_dispatch --target hft-trading --rebuild false --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 --main-sha "$main_sha" --monorepo-conclusion missing --prediction-conclusion success --security-conclusion success) ;;
    required-pending) args=(--event workflow_dispatch --target hft-trading --rebuild false --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 --main-sha "$main_sha" --monorepo-conclusion success --prediction-conclusion in_progress --security-conclusion success) ;;
    required-skipped) args=(--event workflow_dispatch --target hft-trading --rebuild false --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 --main-sha "$main_sha" --monorepo-conclusion success --prediction-conclusion success --security-conclusion skipped) ;;
    required-failed) args=(--event workflow_dispatch --target hft-trading --rebuild false --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 --main-sha "$main_sha" --monorepo-conclusion failure --prediction-conclusion success --security-conclusion success) ;;
    implicit-rebuild) args=(--event workflow_dispatch --target research-runner --rebuild false --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678) ;;
    source-test-missing-sha) args=(--event workflow_dispatch --target research-source-test --rebuild false --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678) ;;
    source-test-nonmain) args=(--event workflow_dispatch --target research-source-test --rebuild false --source-test-sha "$main_sha" --current-ref refs/heads/codex/example --current-sha "$other_sha" --current-run-id 5678) ;;
    source-test-untrusted-sha) args=(--event workflow_dispatch --target research-source-test --rebuild false --source-test-sha "$main_sha" --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678) ;;
    source-test-stale-main) args=(--event workflow_dispatch --target research-source-test --rebuild false --source-test-sha "$other_sha" --source-test-profile binance-bstocks-attestation --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678 --main-sha "$main_sha") ;;
    source-test-rebuild) args=(--event workflow_dispatch --target research-source-test --rebuild true --source-test-sha "$main_sha" --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678) ;;
    source-test-invalid-profile) args=(--event workflow_dispatch --target research-source-test --rebuild false --source-test-sha "$main_sha" --source-test-profile invalid-profile --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678) ;;
    source-test-on-runtime) args=(--event workflow_dispatch --target hft-trading --rebuild false --source-test-sha "$main_sha" --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678) ;;
    automated-source-test) args=(--event workflow_run --conclusion success --source-event push --head-branch main --head-sha "$main_sha" --run-id 1234 --binaries-conclusion success --smoke-conclusion success --source-test-sha "$main_sha") ;;
  esac
  if "$selector" "${args[@]}" --output "$tmp_dir/$rejected.out" >/dev/null 2>&1; then
    printf '%s unexpectedly selected a publish source\n' "$rejected" >&2
    exit 1
  fi
done
if "$selector" --event workflow_run --conclusion success --source-event push \
  --head-branch main --head-sha "$main_sha" --run-id 1234 \
  --binaries-conclusion success --smoke-conclusion skipped \
  --output "$tmp_dir/partial.out" >/dev/null 2>&1; then
  printf 'partial research validation unexpectedly selected a publish source\n' >&2
  exit 1
fi

checks_json="$tmp_dir/checks.json"
checks_output="$tmp_dir/checks.out"
jq -n '{check_runs:[
  {id:1,name:"Monorepo CI gate",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}},
  {id:2,name:"Prediction Markets CI gate",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}},
  {id:3,name:"Security Summary Report",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}}
]}' >"$checks_json"
"$check_reader" "$checks_json" "$checks_output"
diff -u <(printf '%s\n' \
  'monorepo_conclusion=success' \
  'prediction_conclusion=success' \
  'security_conclusion=success') "$checks_output"
"$check_reader" /dev/stdin "$checks_output" <"$checks_json"
jq -s '.' "$checks_json" >"$tmp_dir/check-pages.json"
"$check_reader" "$tmp_dir/check-pages.json" "$checks_output"

jq -n '{check_runs:[
  {id:1,name:"Monorepo CI gate",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}},
  {id:4,name:"Monorepo CI gate",status:"in_progress",conclusion:null,app:{id:15368,slug:"github-actions"}},
  {id:5,name:"Monorepo CI gate",status:"completed",conclusion:"success",app:{id:1,slug:"untrusted"}},
  {id:2,name:"Prediction Markets CI gate",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}}
]}' >"$checks_json"
"$check_reader" "$checks_json" "$checks_output"
diff -u <(printf '%s\n' \
  'monorepo_conclusion=in_progress' \
  'prediction_conclusion=success' \
  'security_conclusion=missing') "$checks_output"

jq -n '{check_runs:[
  {id:1,name:"Monorepo CI gate",status:"completed",conclusion:"unexpected",app:{id:15368,slug:"github-actions"}}
]}' >"$checks_json"
if "$check_reader" "$checks_json" "$checks_output" >/dev/null 2>&1; then
  printf 'invalid required check state unexpectedly passed\n' >&2
  exit 1
fi

repo="$tmp_dir/repo"
release="$tmp_dir/release"
mkdir -p "$repo/prediction-markets" "$release/research-bin"
printf 'root lock\n' >"$repo/Cargo.lock"
printf 'prediction lock\n' >"$repo/prediction-markets/Cargo.lock"
for binary in hft-backtest alpha-harness lob-pit-materializer binance-market-tape-slicer binance-replay-parquet-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  printf '%s\n' "$binary" >"$release/research-bin/$binary"
  chmod 0755 "$release/research-bin/$binary"
done

"$artifact" create "$release" "$main_sha" 1234 "$repo"
"$artifact" verify "$release" "$main_sha" 1234 "$repo"

assert_rejected() {
  local name=$1 expected_sha=${2:-$main_sha} expected_run=${3:-1234}
  local candidate="$tmp_dir/$name"
  cp -R "$release" "$candidate"
  case "$name" in
    missing) rm "$candidate/research-bin/hft-backtest" ;;
    extra) touch "$candidate/research-bin/unexpected" ;;
    digest) printf 'tampered\n' >>"$candidate/research-bin/alpha-harness" ;;
    source-mismatch|run-mismatch|lock-mismatch) ;;
  esac
  if "$artifact" verify "$candidate" "$expected_sha" "$expected_run" "$repo"; then
    printf '%s unexpectedly verified\n' "$name" >&2
    exit 1
  fi
}

assert_rejected missing
assert_rejected extra
assert_rejected digest
assert_rejected source-mismatch "$other_sha"
assert_rejected run-mismatch "$main_sha" 9999
printf 'changed lock\n' >>"$repo/Cargo.lock"
assert_rejected lock-mismatch

printf 'research image release artifact tests passed\n'
