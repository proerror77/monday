#!/usr/bin/env bash
# shellcheck disable=SC1003,SC2016
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workflow="$script_dir/../workflows/acr-publish.yml"
ploy_workflow="$script_dir/../workflows/ploy-ci.yml"
dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.research"
verifier="$script_dir/verify-research-runner-binaries.sh"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT
mode_restore_block=$(sed -n \
  '/^      - name: Restore research runner binary modes$/,/^      - name: Verify research runner binary artifact$/p' \
  "$workflow")
source_command_block=$(sed -n \
  '/^          \.github\/scripts\/select-acr-publish-source\.sh \\/,/^            --current-run-id /p' \
  "$workflow")

grep -Fqx '            [{repository:"research-runner",file:"rust_hft/deployment/docker/Dockerfile.research",target:"prebuilt"},' "$workflow"
grep -Fqx '  workflow_run:' "$workflow"
grep -Fqx '    workflows: ["Prediction Markets CI"]' "$workflow"
grep -Fqx '    branches: [main]' "$workflow"
grep -Fqx "        description: Image target to publish (polymarket-raw-ops uses binance-lob-archiver's full collector bundle)" "$workflow"
grep -Fqx '          - polymarket-raw-ops' "$workflow"
grep -Fqx '  actions: read' "$workflow"
grep -Fqx '      rebuild_research_runner:' "$workflow"
grep -Fqx '          jobs=$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$SOURCE_RUN_ID/jobs" \' "$workflow"
grep -Fqx '          BINARIES_CONCLUSION: ${{ steps.source-jobs.outputs.binaries_conclusion }}' "$workflow"
grep -Fqx '          SMOKE_CONCLUSION: ${{ steps.source-jobs.outputs.smoke_conclusion }}' "$workflow"
grep -Fqx '          .github/scripts/select-acr-publish-source.sh \' "$workflow"
if grep -Fq '${{' <<<"$source_command_block"; then
  printf 'source selector interpolates workflow context directly into shell\n' >&2
  exit 1
fi
grep -Fqx '  research-runner-binaries:' "$workflow"
grep -Fqx "    if: needs.selector.outputs.research_mode == 'rebuild'" "$workflow"
grep -Fqx "    if: always() && needs.selector.result == 'success' && needs.selector.outputs.publish_target != 'none'" "$workflow"
grep -Fqx '    container: rust:1.91-bookworm' "$workflow"
grep -Fqx 'FROM debian:bookworm-slim AS runtime-base' "$dockerfile"
grep -Fqx '    needs: [selector, research-runner-binaries]' "$workflow"
grep -Fqx '      - name: Download research runner binaries' "$workflow"
grep -Fqx '          name: research-image-release-${{ needs.selector.outputs.source_sha }}' "$workflow"
grep -Fqx '          run-id: ${{ needs.selector.outputs.artifact_run_id }}' "$workflow"
grep -Fqx '          github-token: ${{ github.token }}' "$workflow"
grep -Fqx '      - name: Restore research runner binary modes' "$workflow"
grep -Fqx '          target: ${{ matrix.target }}' "$workflow"
grep -Fqx '          ../.github/scripts/research-image-release-artifact.sh create research-release \' "$workflow"
grep -Fqx '          .github/scripts/research-image-release-artifact.sh verify research-release \' "$workflow"
grep -Fqx '            "${{ needs.selector.outputs.source_sha }}" \' "$workflow"
grep -Fqx '            "${{ needs.selector.outputs.artifact_run_id }}" rust_hft' "$workflow"
grep -Fqx '            SOURCE_REVISION=${{ needs.selector.outputs.source_sha }}' "$workflow"
grep -Fqx '            org.opencontainers.image.revision=${{ needs.selector.outputs.source_sha }}' "$workflow"

# research-runner-binaries compiles on the runner and must use the #559/#566
# sccache-action pattern; the publish matrix compiles inside docker, where a
# host-side wrapper does not apply.
grep -Fqx '      RUSTC_WRAPPER: sccache' "$workflow"
grep -Fqx '      SCCACHE_GHA_ENABLED: "true"' "$workflow"
grep -Fqx '        uses: mozilla-actions/sccache-action@v0.0.10' "$workflow"
grep -Fqx '        continue-on-error: true' "$workflow"
! grep -Fq 'sccache --zero-stats' "$workflow"
! grep -Fq 'path: ~/.cache/sccache' "$workflow"
! grep -Fq -- '}}-${{ github.sha }}' "$workflow"

grep -Fqx '          name: research-image-release-${{ github.sha }}' "$ploy_workflow"
grep -Fqx '          retention-days: 1' "$ploy_workflow"
test "$(grep -Fxc '            jq \' "$workflow")" -eq 1
test "$(grep -Fxc '            jq \' "$ploy_workflow")" -eq 1
grep -Fqx '          ../.github/scripts/research-image-release-artifact.sh create \' "$ploy_workflow"
grep -Fqx '          .github/scripts/research-image-release-artifact.sh verify research-release \' "$ploy_workflow"

for binary in hft-backtest alpha-harness lob-pit-materializer binance-replay-parquet-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  grep -Fq "research-release/research-bin/$binary" <<<"$mode_restore_block"
done

for binary in hft-backtest alpha-harness lob-pit-materializer binance-replay-parquet-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  touch "$tmp_dir/$binary"
  chmod 0644 "$tmp_dir/$binary"
done
if "$verifier" "$tmp_dir"; then
  exit 1
fi
chmod 0755 \
  "$tmp_dir/hft-backtest" \
  "$tmp_dir/alpha-harness" \
  "$tmp_dir/lob-pit-materializer" \
  "$tmp_dir/binance-replay-parquet-materializer" \
  "$tmp_dir/monday-prediction-research" \
  "$tmp_dir/monday-prediction-evaluator" \
  "$tmp_dir/monday-prediction-snapshot"
"$verifier" "$tmp_dir"
rm "$tmp_dir/hft-backtest"
if "$verifier" "$tmp_dir"; then
  exit 1
fi
touch "$tmp_dir/hft-backtest"
chmod +x "$tmp_dir/hft-backtest"
touch "$tmp_dir/unexpected"
chmod +x "$tmp_dir/unexpected"
if "$verifier" "$tmp_dir"; then
  exit 1
fi

"$script_dir/test-research-image-release-artifact.sh"

printf 'ACR research-runner prebuilt contract tests passed\n'
