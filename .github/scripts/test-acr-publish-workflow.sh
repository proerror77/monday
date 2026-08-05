#!/usr/bin/env bash
# shellcheck disable=SC1003,SC2016
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workflow="$script_dir/../workflows/acr-publish.yml"
dockerignore="$script_dir/../../.dockerignore"
ploy_workflow="$script_dir/../workflows/ploy-ci.yml"
dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.research"
source_test_dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.source-test"
source_test_entrypoint="$script_dir/../../rust_hft/deployment/docker/source-test-entrypoint.sh"
source_test_job="$script_dir/../../deployment/aliyun/research/k8s/source-test-job.example.yaml"
verifier="$script_dir/verify-research-runner-binaries.sh"
tmp_dir=$(mktemp -d)
source_test_tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir" "$source_test_tmp_dir"' EXIT
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
grep -Fqx '          - research-source-test' "$workflow"
grep -Fqx '  actions: read' "$workflow"
grep -Fqx '      rebuild_research_runner:' "$workflow"
grep -Fqx '      source_test_source_sha:' "$workflow"
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
grep -Fqx "    if: always() && needs.selector.result == 'success' && needs.selector.outputs.publish_target != 'none' && needs.selector.outputs.publish_target != 'research-source-test'" "$workflow"
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
grep -Fqx '  publish-source-test:' "$workflow"
grep -Fqx "    if: needs.selector.outputs.publish_target == 'research-source-test'" "$workflow"
grep -Fqx '          SOURCE_TEST_SOURCE_SHA: ${{ inputs.source_test_source_sha }}' "$workflow"
grep -Fqx '            --source-test-sha "$SOURCE_TEST_SOURCE_SHA" \' "$workflow"

source_test_block=$(sed -n '/^  publish-source-test:$/,$p' "$workflow")
grep -Fq 'ref: ${{ github.sha }}' <<<"$source_test_block"
grep -Fq 'ref: ${{ needs.selector.outputs.source_sha }}' <<<"$source_test_block"
grep -Fq 'path: source' <<<"$source_test_block"
grep -Fq 'sparse-checkout: rust_hft' <<<"$source_test_block"
test "$(grep -Fc 'persist-credentials: false' <<<"$source_test_block")" -eq 2
grep -Fq 'rm -rf -- source/.git' <<<"$source_test_block"
grep -Fq 'context: .' <<<"$source_test_block"
grep -Fq 'file: rust_hft/deployment/docker/Dockerfile.source-test' <<<"$source_test_block"
grep -Fq 'research-source-test@${{ steps.build.outputs.digest }}' <<<"$source_test_block"
grep -Fq 'docker run --rm --network none --read-only' <<<"$source_test_block"
grep -Fq -- '--tmpfs /tmp:rw,nosuid,nodev,size=16g' <<<"$source_test_block"
grep -Fq 'provenance: mode=max' <<<"$source_test_block"
grep -Fq 'com.monday.image.retention=single-ack-test' <<<"$source_test_block"
grep -Fq 'Refuse source-test tag overwrite' <<<"$source_test_block"
grep -Fq 'docker manifest inspect "$IMAGE_TAG"' <<<"$source_test_block"
if grep -Fq 'research-source-test:run-' <<<"$source_test_block" || grep -Fq 'cache-to: type=gha,mode=max,scope=acr-research-source-test' <<<"$source_test_block"; then
  printf 'source-test image contract retains a mutable tag or persistent build cache\n' >&2
  exit 1
fi

grep -Fqx 'FROM rust:1.91-bookworm AS source-test' "$source_test_dockerfile"
grep -Fqx 'COPY --chown=research:research source/rust_hft/ /work/' "$source_test_dockerfile"
grep -Fqx 'RUN cargo fetch --locked' "$source_test_dockerfile"
grep -Fqx '    CARGO_HOME=/opt/monday-source-test-cargo \' "$source_test_dockerfile"
grep -Fqx 'ENTRYPOINT ["/usr/local/bin/monday-source-test"]' "$source_test_dockerfile"
grep -Fqx 'export CARGO_BUILD_JOBS=2' "$source_test_entrypoint"
test "$(rg -n '^RUN cargo fetch --locked$' "$source_test_dockerfile" | cut -d: -f1)" \
  -lt "$(rg -n '^ENV CARGO_NET_OFFLINE=true$' "$source_test_dockerfile" | cut -d: -f1)"
grep -Fqx 'source/rust_hft/config/secrets.yaml' "$dockerignore"
grep -Fqx 'source/rust_hft/clickhouse_credentials.txt' "$dockerignore"
if rg -qi 'credential|secret|api[_-]?key|password|access[_-]?token' "$source_test_dockerfile" "$source_test_entrypoint"; then
  printf 'source-test image contract mentions a credential surface\n' >&2
  exit 1
fi

mkdir -p "$source_test_tmp_dir/bin"
mkdir -p "$source_test_tmp_dir/cargo-home"
printf '%s\n' '#!/usr/bin/env bash' 'printf "%s\\n" "$*"' >"$source_test_tmp_dir/bin/cargo"
chmod 0755 "$source_test_tmp_dir/bin/cargo"
CARGO_HOME="$source_test_tmp_dir/cargo-home" XDG_RUNTIME_DIR="$source_test_tmp_dir" \
  PATH="$source_test_tmp_dir/bin:$PATH" sh "$source_test_entrypoint" binance-bstocks-attestation \
  >"$source_test_tmp_dir/binance-source-test.out"
diff -u <(printf '%s\n' 'test --offline --locked -p hft-runtime --lib tokenized_security_requires_runtime_owned_attestation') \
  "$source_test_tmp_dir/binance-source-test.out"
CARGO_HOME="$source_test_tmp_dir/cargo-home" XDG_RUNTIME_DIR="$source_test_tmp_dir" \
  PATH="$source_test_tmp_dir/bin:$PATH" sh "$source_test_entrypoint" bybit-spot \
  >"$source_test_tmp_dir/bybit-source-test.out"
diff -u <(printf '%s\n' 'test --offline --locked -p hft-execution-adapter-bybit --lib') \
  "$source_test_tmp_dir/bybit-source-test.out"
if CARGO_HOME="$source_test_tmp_dir/cargo-home" XDG_RUNTIME_DIR="$source_test_tmp_dir" \
  PATH="$source_test_tmp_dir/bin:$PATH" sh "$source_test_entrypoint" arbitrary-profile >/dev/null 2>&1; then
  printf 'source-test entrypoint accepted an unapproved profile\n' >&2
  exit 1
fi
if CARGO_HOME="$source_test_tmp_dir/cargo-home" XDG_RUNTIME_DIR="$source_test_tmp_dir" \
  PATH="$source_test_tmp_dir/bin:$PATH" sh "$source_test_entrypoint" bybit-spot extra >/dev/null 2>&1; then
  printf 'source-test entrypoint accepted extra arguments\n' >&2
  exit 1
fi

grep -Fq 'namespace: monday-research' "$source_test_job"
grep -Fq 'suspend: true' "$source_test_job"
grep -Fq 'backoffLimit: 0' "$source_test_job"
grep -Fq 'activeDeadlineSeconds: 1800' "$source_test_job"
grep -Fq 'ttlSecondsAfterFinished: 900' "$source_test_job"
grep -Fq 'imagePullPolicy: Always' "$source_test_job"
grep -Fq 'automountServiceAccountToken: false' "$source_test_job"
grep -Fq 'kubernetes.io/arch: amd64' "$source_test_job"
grep -Fq 'workload: backtest' "$source_test_job"
grep -Fq 'name: monday-acr' "$source_test_job"
grep -Fq 'runAsNonRoot: true' "$source_test_job"
grep -Fq 'type: RuntimeDefault' "$source_test_job"
grep -Fq 'allowPrivilegeEscalation: false' "$source_test_job"
grep -Fq 'readOnlyRootFilesystem: true' "$source_test_job"
grep -Fq 'emptyDir:' "$source_test_job"
grep -Fq 'research-source-test@sha256:' "$source_test_job"
if rg -q 'command:|nodeName:|tolerations:|secretKeyRef:|env:|envFrom:|persistentVolumeClaim:|configMap:|hostPath:' "$source_test_job"; then
  printf 'source-test Job template widens its execution or storage boundary\n' >&2
  exit 1
fi

# research-runner-binaries compiles on the runner and must use the #559/#566
# sccache-action pattern; the publish matrix compiles inside docker, where a
# host-side wrapper does not apply.
grep -Fqx '      RUSTC_WRAPPER: sccache' "$workflow"
grep -Fqx '      SCCACHE_GHA_ENABLED: "true"' "$workflow"
grep -Fqx '        uses: mozilla-actions/sccache-action@v0.0.10' "$workflow"
grep -Fqx '        continue-on-error: true' "$workflow"
if grep -Fq 'sccache --zero-stats' "$workflow" || \
  grep -Fq 'path: ~/.cache/sccache' "$workflow" || \
  grep -Fq -- '}}-${{ github.sha }}' "$workflow"; then
  exit 1
fi

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
