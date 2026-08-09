#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
selector="$script_dir/select-acr-publish-source.sh"
artifact="$script_dir/research-image-release-artifact.sh"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

main_sha=1111111111111111111111111111111111111111
other_sha=2222222222222222222222222222222222222222

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
  --binaries-conclusion success --smoke-conclusion success
assert_source irrelevant $'publish_target=none\nresearch_mode=none\nsource_sha=1111111111111111111111111111111111111111\nartifact_run_id=1234' \
  --event workflow_run --conclusion success --source-event push --head-branch main \
  --head-sha "$main_sha" --run-id 1234 \
  --binaries-conclusion skipped --smoke-conclusion skipped
assert_source manual-rebuild $'publish_target=research-runner\nresearch_mode=rebuild\nsource_sha=2222222222222222222222222222222222222222\nartifact_run_id=5678' \
  --event workflow_dispatch --target research-runner --rebuild true \
  --current-sha "$other_sha" --current-run-id 5678
assert_source manual-nonresearch $'publish_target=hft-trading\nresearch_mode=none\nsource_sha=2222222222222222222222222222222222222222\nartifact_run_id=5678' \
  --event workflow_dispatch --target hft-trading --rebuild false \
  --current-sha "$other_sha" --current-run-id 5678
assert_source manual-raw-ops-alias $'publish_target=binance-lob-archiver\nresearch_mode=none\nsource_sha=2222222222222222222222222222222222222222\nartifact_run_id=5678' \
  --event workflow_dispatch --target polymarket-raw-ops --rebuild false \
  --current-sha "$other_sha" --current-run-id 5678
assert_source source-test $'publish_target=research-source-test\nresearch_mode=none\nsource_sha=1111111111111111111111111111111111111111\nartifact_run_id=5678\nsource_test_profile=binance-bstocks-attestation\nsource_test_tag=source-test-1111111111111111111111111111111111111111-binance-bstocks-attestation' \
  --event workflow_dispatch --target research-source-test --rebuild false \
  --source-test-sha "$main_sha" --source-test-profile binance-bstocks-attestation --current-ref refs/heads/main \
  --current-sha "$main_sha" --current-run-id 5678

assert_source_test_identity() {
  local name=$1 profile=$2
  local output="$tmp_dir/$name.out" tag selected_profile
  "$selector" --event workflow_dispatch --target research-source-test --rebuild false \
    --source-test-sha "$main_sha" --source-test-profile "$profile" \
    --current-ref refs/heads/main --current-sha "$main_sha" --current-run-id 5678 \
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

for rejected in failed-run pull-request-run branch-run implicit-rebuild source-test-missing-sha source-test-nonmain source-test-untrusted-sha source-test-rebuild source-test-invalid-profile source-test-on-runtime automated-source-test; do
  case "$rejected" in
    failed-run) args=(--event workflow_run --conclusion failure --source-event push --head-branch main --head-sha "$main_sha" --run-id 1234) ;;
    pull-request-run) args=(--event workflow_run --conclusion success --source-event pull_request --head-branch main --head-sha "$main_sha" --run-id 1234) ;;
    branch-run) args=(--event workflow_run --conclusion success --source-event push --head-branch develop --head-sha "$main_sha" --run-id 1234) ;;
    implicit-rebuild) args=(--event workflow_dispatch --target research-runner --rebuild false --current-sha "$other_sha" --current-run-id 5678) ;;
    source-test-missing-sha) args=(--event workflow_dispatch --target research-source-test --rebuild false --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678) ;;
    source-test-nonmain) args=(--event workflow_dispatch --target research-source-test --rebuild false --source-test-sha "$main_sha" --current-ref refs/heads/codex/example --current-sha "$other_sha" --current-run-id 5678) ;;
    source-test-untrusted-sha) args=(--event workflow_dispatch --target research-source-test --rebuild false --source-test-sha "$main_sha" --current-ref refs/heads/main --current-sha "$other_sha" --current-run-id 5678) ;;
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

repo="$tmp_dir/repo"
release="$tmp_dir/release"
mkdir -p "$repo/prediction-markets" "$release/research-bin"
printf 'root lock\n' >"$repo/Cargo.lock"
printf 'prediction lock\n' >"$repo/prediction-markets/Cargo.lock"
for binary in hft-backtest alpha-harness lob-pit-materializer binance-replay-parquet-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
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
