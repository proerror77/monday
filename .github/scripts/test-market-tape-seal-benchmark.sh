#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$script_dir/../.." && pwd)
workflow="$root/.github/workflows/market-tape-seal-benchmark.yml"
harness="$root/rust_hft/tools/collector/src/polymarket_upload.rs"

test -f "$workflow"
test -f "$harness"

trigger_block=$(sed -n '/^on:$/,/^permissions:$/p' "$workflow")
grep -Fqx '  workflow_dispatch:' <<<"$trigger_block"
! grep -Eq '^  (push|pull_request|schedule|workflow_call):' <<<"$trigger_block"
grep -Fqx '          - polymarket-seal-v1' <<<"$trigger_block"
grep -Fqx '      exact_main_sha:' <<<"$trigger_block"

grep -Fqx '  contents: read' "$workflow"
grep -Fqx '    runs-on: ubuntu-latest' "$workflow"
grep -Fqx '    timeout-minutes: 45' "$workflow"
grep -A2 -F '      - name: Compile the exact benchmark test without running it' "$workflow" \
  | grep -Fqx '        timeout-minutes: 12'
grep -A3 -F '      - name: Run exactly one bounded synthetic benchmark' "$workflow" \
  | grep -Fqx '        timeout-minutes: 25'
grep -Fqx '          test "$BENCHMARK_SELECTION" = polymarket-seal-v1' "$workflow"
grep -Fqx '          test "$DISPATCH_REF" = refs/heads/main' "$workflow"
grep -Fqx '          test "$DISPATCH_SHA" = "$EXPECTED_MAIN_SHA"' "$workflow"
grep -Fqx '        uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4.3.1' "$workflow"
grep -Fqx '          ref: refs/heads/main' "$workflow"
grep -Fqx '          persist-credentials: false' "$workflow"
grep -Fqx '        uses: dtolnay/rust-toolchain@4360b52568e2003a75bf9bc1d59f33a8e3fc893c # stable' "$workflow"
grep -Fqx '          test "$(git rev-parse HEAD)" = "$EXPECTED_MAIN_SHA"' "$workflow"
grep -Fqx '          benchmark_root="${expected_prefix}${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"' "$workflow"
grep -Fqx '          } >> "$GITHUB_ENV"' "$workflow"
grep -Fqx '          min_free_bytes=$((6 * 1024 * 1024 * 1024))' "$workflow"
grep -Fq 'synthetic_immutable_fixture_reports_full_scan_and_seal_lookup_phases' "$workflow"
grep -Fq -- '-- --ignored --exact --nocapture' "$workflow"
grep -Fqx '          test "$selected_tests" -eq 1' "$workflow"
grep -Fqx '        if: ${{ always() }}' "$workflow"
grep -Fqx '          rm -rf -- "$benchmark_root"' "$workflow"
grep -Fqx '          test ! -e "$benchmark_root"' "$workflow"

for phase in fixture_generate fixture_sha256 full_scan seal_lookup; do
  grep -Fq "phase=benchmark_${phase}" "$workflow"
done
grep -Fq 'manifest_equivalent=true fixture_sha256=[0-9a-f]{64}' "$workflow"

grep -Fqx '        const TARGET_FIXTURE_BYTES: u64 = 4 * GIB;' "$harness"
grep -Fqx '    const MIN_FIXTURE_BYTES: u64 = 37 * GIB / 10;' "$harness"
grep -Fqx '    const MAX_FIXTURE_BYTES: u64 = 43 * GIB / 10;' "$harness"
grep -Fq 'source_bytes.saturating_add(encoded_bytes) <= MAX_FIXTURE_BYTES' "$harness"
grep -Fq 'assert_eq!(sealed.manifest, full.manifest);' "$harness"
grep -Fq 'IMMUTABLE_FIXTURE_CLEANUP removed=true' "$harness"

workflow_without_safe_checkout=$(sed '/^          persist-credentials: false$/d' "$workflow")
if grep -Eqi 'secrets[.]|ossutil|aliyun|acr|kubectl|kubeconfig|access[_-]?key|credentials|ssh |docker (build|push)|curl ' \
  <<<"$workflow_without_safe_checkout"; then
  printf 'one-shot benchmark must not use credentials, cloud, images, or production connections\n' >&2
  exit 1
fi
