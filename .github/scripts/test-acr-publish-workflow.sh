#!/usr/bin/env bash
# shellcheck disable=SC1003,SC2016
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workflow="$script_dir/../workflows/acr-publish.yml"
dockerignore="$script_dir/../../.dockerignore"
ploy_workflow="$script_dir/../workflows/ploy-ci.yml"
ci_workflow="$script_dir/../workflows/ci.yml"
dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.research"
controller_dockerfile="$script_dir/../../deployment/aliyun/research/Dockerfile.campaign-cycle-controller"
controller_job="$script_dir/../../deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml"
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
  '/^          \.github\/scripts\/select-acr-publish-source\.sh \\/,/^            --security-conclusion /p' \
  "$workflow")
acr_publish_block=$(sed -n \
  '/^      - name: Build and push$/,/^      - name: Record immutable image$/p' \
  "$workflow")
ci_push_block=$(sed -n '/^  push:$/,/^  pull_request:$/p' "$ci_workflow")
ploy_push_block=$(sed -n '/^  push:$/,/^  workflow_dispatch:$/p' "$ploy_workflow")

grep -Fqx '            [{repository:"research-runner",context:"rust_hft",file:"rust_hft/deployment/docker/Dockerfile.research",target:"prebuilt",research_artifact:true},' "$workflow"
grep -Fqx '             {repository:"campaign-cycle-controller",context:".",file:"deployment/aliyun/research/Dockerfile.campaign-cycle-controller",target:"prebuilt",research_artifact:true},' "$workflow"
grep -Fqx '                or ($target == "research-runner" and .repository == "campaign-cycle-controller")' "$workflow"
grep -Fqx '  workflow_run:' "$workflow"
grep -Fqx '    workflows: ["Prediction Markets CI"]' "$workflow"
grep -Fqx '    branches: [main]' "$workflow"
grep -Fqx '        description: Image target (research-runner also publishes its paired Campaign controller)' "$workflow"
grep -Fqx '          - polymarket-raw-ops' "$workflow"
grep -Fqx '          - research-source-test' "$workflow"
grep -Fqx '  actions: read' "$workflow"
grep -Fqx '  checks: read' "$workflow"
grep -Fqx 'concurrency:' "$workflow"
grep -Fqx '  group: acr-publish-${{ github.ref }}' "$workflow"
grep -Fqx '  cancel-in-progress: false' "$workflow"
if grep -Eq '^    paths(-ignore)?:' <<<"$ci_push_block$ploy_push_block"; then
  printf 'required CI workflow can skip a main SHA by path\n' >&2
  exit 1
fi
grep -Fqx '      rebuild_research_runner:' "$workflow"
grep -Fqx '      source_test_source_sha:' "$workflow"
grep -Fqx '          jobs=$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$SOURCE_RUN_ID/jobs" \' "$workflow"
grep -Fqx '          BINARIES_CONCLUSION: ${{ steps.source-jobs.outputs.binaries_conclusion }}' "$workflow"
grep -Fqx '          SMOKE_CONCLUSION: ${{ steps.source-jobs.outputs.smoke_conclusion }}' "$workflow"
grep -Fqx '      - name: Read authenticated release admission' "$workflow"
grep -Fqx '            main_sha=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq '\''.object.sha'\'')' "$workflow"
grep -Fqx '            gh api --paginate --slurp \' "$workflow"
grep -Fqx '              "repos/$GITHUB_REPOSITORY/commits/$admission_sha/check-runs?filter=latest&per_page=100" > "$checks_json"' "$workflow"
grep -Fqx '            .github/scripts/read-acr-required-checks.sh "$checks_json" "$evidence"' "$workflow"
grep -Fqx '          deadline=$((SECONDS + 900))' "$workflow"
grep -Fqx '          .github/scripts/select-acr-publish-source.sh \' "$workflow"
grep -Fqx '          CURRENT_REF: ${{ github.ref }}' "$workflow"
grep -Fqx '          MAIN_SHA: ${{ steps.admission.outputs.main_sha }}' "$workflow"
grep -Fqx '          MONOREPO_CONCLUSION: ${{ steps.admission.outputs.monorepo_conclusion }}' "$workflow"
grep -Fqx '          PREDICTION_CONCLUSION: ${{ steps.admission.outputs.prediction_conclusion }}' "$workflow"
grep -Fqx '          SECURITY_CONCLUSION: ${{ steps.admission.outputs.security_conclusion }}' "$workflow"
grep -Fqx '            --current-ref "$CURRENT_REF" \' "$workflow"
grep -Fqx '            --main-sha "$MAIN_SHA" \' "$workflow"
grep -Fqx '            --monorepo-conclusion "$MONOREPO_CONCLUSION" \' "$workflow"
grep -Fqx '            --prediction-conclusion "$PREDICTION_CONCLUSION" \' "$workflow"
grep -Fqx '            --security-conclusion "$SECURITY_CONCLUSION"' "$workflow"
grep -Fqx '      - name: Revalidate current main before publication' "$workflow"
grep -Fqx '          current_main=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq '\''.object.sha'\'')' "$workflow"
grep -Fqx '          test "$SOURCE_REVISION" = "$current_main"' "$workflow"
test "$(grep -n '^      - name: Revalidate current main before publication$' "$workflow" | cut -d: -f1)" \
  -lt "$(grep -n '^      - name: Build and push$' "$workflow" | cut -d: -f1)"
if grep -Fq '${{' <<<"$source_command_block"; then
  printf 'source selector interpolates workflow context directly into shell\n' >&2
  exit 1
fi
grep -Fqx '  research-runner-binaries:' "$workflow"
grep -Fqx "    if: needs.selector.outputs.research_mode == 'rebuild'" "$workflow"
grep -Fqx "    if: always() && needs.selector.result == 'success' && needs.selector.outputs.publish_target != 'none' && needs.selector.outputs.publish_target != 'research-source-test'" "$workflow"
grep -Fqx '    container: rust:1.91-bookworm' "$workflow"
grep -Fqx 'FROM debian:bookworm-slim AS runtime-base' "$dockerfile"
grep -Fqx 'ARG ALIYUN_CLI_VERSION=3.4.6' "$controller_dockerfile"
grep -Fqx 'ARG KUBECTL_VERSION=v1.35.3' "$controller_dockerfile"
grep -Fq 'aliyun_sha256=9f7c993bd1b16c530f219bc1976bf78057879db4b1bae857b2952676eb7466f6' "$controller_dockerfile"
grep -Fq 'kubectl_sha256=fd31c7d7129260e608f6faf92d5984c3267ad0b5ead3bced2fe125686e286ad6' "$controller_dockerfile"
grep -Fqx 'COPY --chmod=0755 rust_hft/research-bin/alpha-harness /usr/local/bin/alpha-harness' "$controller_dockerfile"
grep -Fqx 'COPY --chmod=0755 deployment/aliyun/research/scripts/campaign-cycle-controller.sh \' "$controller_dockerfile"
grep -Fqx 'COPY --chmod=0644 deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml \' "$controller_dockerfile"
grep -Fqx 'RUN chmod 0755 /opt/monday/deployment/aliyun/research/k8s' "$controller_dockerfile"
grep -Fqx 'USER research' "$controller_dockerfile"
grep -Fqx 'ENTRYPOINT ["/usr/bin/tini", "--", "/bin/bash", "/opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh"]' "$controller_dockerfile"
grep -Fqx '          image: crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/campaign-cycle-controller@sha256:REPLACE_WITH_IMMUTABLE_DIGEST' "$controller_job"
if grep -Eq '^[[:space:]]+command:' "$controller_job"; then
  printf 'ACK controller Job bypasses the image entrypoint\n' >&2
  exit 1
fi
grep -Fqx '      - name: Build the Campaign cycle controller image' "$ploy_workflow"
grep -Fqx '          file: deployment/aliyun/research/Dockerfile.campaign-cycle-controller' "$ploy_workflow"
grep -Fqx '          tags: monday-campaign-cycle-controller-smoke:local' "$ploy_workflow"
grep -Fqx '    needs: [selector, research-runner-binaries]' "$workflow"
grep -Fqx '      - name: Download research runner binaries' "$workflow"
test "$(grep -Fxc '        if: matrix.research_artifact' "$workflow")" -eq 4
grep -Fqx '          name: research-image-release-${{ needs.selector.outputs.source_sha }}' "$workflow"
grep -Fqx '          run-id: ${{ needs.selector.outputs.artifact_run_id }}' "$workflow"
grep -Fqx '          github-token: ${{ github.token }}' "$workflow"
grep -Fqx '      - name: Restore research runner binary modes' "$workflow"
grep -Fqx '          target: ${{ matrix.target }}' "$workflow"
grep -Fqx '          context: ${{ matrix.context }}' "$workflow"
grep -Fqx '          ../.github/scripts/research-image-release-artifact.sh create research-release \' "$workflow"
grep -Fqx '          .github/scripts/research-image-release-artifact.sh verify research-release \' "$workflow"
grep -Fqx '            "${{ needs.selector.outputs.source_sha }}" \' "$workflow"
grep -Fqx '            "${{ needs.selector.outputs.artifact_run_id }}" rust_hft' "$workflow"
grep -Fqx '            SOURCE_REVISION=${{ needs.selector.outputs.source_sha }}' "$workflow"
grep -Fqx '            org.opencontainers.image.revision=${{ needs.selector.outputs.source_sha }}' "$workflow"
grep -Fqx '      - name: Verify Campaign cycle controller image' "$workflow"
grep -Fq '          provenance: false' <<<"$acr_publish_block"
grep -Fq '          sbom: false' <<<"$acr_publish_block"
grep -Fqx '  publish-source-test:' "$workflow"
grep -Fqx "    if: needs.selector.outputs.publish_target == 'research-source-test'" "$workflow"
grep -Fqx '      source_test_profile: ${{ steps.source.outputs.source_test_profile }}' "$workflow"
grep -Fqx '      source_test_tag: ${{ steps.source.outputs.source_test_tag }}' "$workflow"
grep -Fqx '          SOURCE_TEST_SOURCE_SHA: ${{ inputs.source_test_source_sha }}' "$workflow"
grep -Fqx '          SOURCE_TEST_PROFILE: ${{ inputs.source_test_profile }}' "$workflow"
grep -Fqx '            --source-test-sha "$SOURCE_TEST_SOURCE_SHA" \' "$workflow"
grep -Fqx '            --source-test-profile "$SOURCE_TEST_PROFILE" \' "$workflow"

source_test_block=$(sed -n '/^  publish-source-test:$/,$p' "$workflow")
grep -Fq 'ref: ${{ github.sha }}' <<<"$source_test_block"
grep -Fq 'ref: ${{ needs.selector.outputs.source_sha }}' <<<"$source_test_block"
grep -Fq 'path: source' <<<"$source_test_block"
grep -Fq 'sparse-checkout: rust_hft' <<<"$source_test_block"
test "$(grep -Fc 'persist-credentials: false' <<<"$source_test_block")" -eq 2
grep -Fq 'rm -rf -- source/.git' <<<"$source_test_block"
grep -Fq 'context: .' <<<"$source_test_block"
grep -Fq 'file: rust_hft/deployment/docker/Dockerfile.source-test' <<<"$source_test_block"
grep -Fq 'load: true' <<<"$source_test_block"
grep -Fq 'push: false' <<<"$source_test_block"
grep -Fq 'provenance: false' <<<"$source_test_block"
grep -Fq 'research-source-test@${{ steps.push.outputs.digest }}' <<<"$source_test_block"
grep -Fq 'IMAGE_TAG: ${{ vars.ACR_REGISTRY }}/wildcard0923/research-source-test:${{ needs.selector.outputs.source_test_tag }}' <<<"$source_test_block"
grep -Fq '${{ vars.ACR_REGISTRY }}/wildcard0923/research-source-test:${{ needs.selector.outputs.source_test_tag }}' <<<"$source_test_block"
grep -Fq 'com.monday.image.source-test-profile=${{ needs.selector.outputs.source_test_profile }}' <<<"$source_test_block"
grep -Fq 'com.monday.image.source-test-identity=${{ needs.selector.outputs.source_test_tag }}' <<<"$source_test_block"
grep -Fq 'SOURCE_TEST_TAG: ${{ needs.selector.outputs.source_test_tag }}' <<<"$source_test_block"
grep -Fq 'docker run --rm --network none --read-only' <<<"$source_test_block"
grep -Fq -- '--tmpfs /tmp:rw,nosuid,nodev,size=16g' <<<"$source_test_block"
grep -Fq -- '--tmpfs /tmp/monday-source-test-target:rw,exec,nosuid,nodev,mode=0700,uid=1000,gid=1000,size=16g' <<<"$source_test_block"
grep -Fq 'com.monday.image.retention=single-ack-test' <<<"$source_test_block"
grep -Fq 'Refuse source-test tag overwrite' <<<"$source_test_block"
grep -Fq 'if probe_output=$(docker manifest inspect "$IMAGE_TAG" 2>&1); then' <<<"$source_test_block"
grep -Fq '*"manifest unknown"*|*"no such manifest"*) ;;' <<<"$source_test_block"
grep -Fq 'docker buildx imagetools inspect "$IMAGE_TAG" --format' <<<"$source_test_block"
grep -Fq 'test "$actual_source_test_profile" = "$TEST_PROFILE"' <<<"$source_test_block"
grep -Fq 'test "$actual_source_test_identity" = "$SOURCE_TEST_TAG"' <<<"$source_test_block"
grep -Fq 'echo "publication_identity=$SOURCE_REVISION/$TEST_PROFILE"' <<<"$source_test_block"
grep -Fq 'echo "publication_tag=$IMAGE_TAG"' <<<"$source_test_block"
test "$(grep -n '^      - name: Verify source-test image before publication$' "$workflow" | cut -d: -f1)" \
  -lt "$(grep -n '^      - name: Push verified source-test image$' "$workflow" | cut -d: -f1)"
if grep -Fq 'research-source-test:run-' <<<"$source_test_block" || grep -Fq 'cache-to: type=gha,mode=max,scope=acr-research-source-test' <<<"$source_test_block"; then
  printf 'source-test image contract retains a mutable tag or persistent build cache\n' >&2
  exit 1
fi
if grep -Fq 'research-source-test:${{ needs.selector.outputs.source_sha }}' <<<"$source_test_block" || \
  grep -Fq '${{ inputs.source_test_profile }}' <<<"$source_test_block"; then
  printf 'source-test image contract bypasses its selected SHA/profile identity\n' >&2
  exit 1
fi

grep -Fqx 'FROM rust:1.91-bookworm@sha256:c1e5f19e773b7878c3f7a805dd00a495e747acbdc76fb2337a4ebf0418896b33 AS source-test' "$source_test_dockerfile"
grep -Fq 'groupadd --gid 1000 research' "$source_test_dockerfile"
grep -Fqx '    && useradd --create-home --uid 1000 --gid 1000 research' "$source_test_dockerfile"
grep -Fqx 'COPY --chown=research:research source/rust_hft/ /work/' "$source_test_dockerfile"
grep -Fqx 'RUN cargo fetch --locked && chown -R research:research "$CARGO_HOME"' "$source_test_dockerfile"
grep -Fqx 'USER 1000:1000' "$source_test_dockerfile"
grep -Fqx '    CARGO_HOME=/opt/monday-source-test-cargo \' "$source_test_dockerfile"
grep -Fqx 'ENTRYPOINT ["/usr/local/bin/monday-source-test"]' "$source_test_dockerfile"
grep -Fqx 'export CARGO_BUILD_JOBS=2' "$source_test_entrypoint"
grep -Fqx 'export CARGO_TARGET_DIR=/tmp/monday-source-test-target' "$source_test_entrypoint"
test "$(grep -n -F 'RUN cargo fetch --locked && chown -R research:research "$CARGO_HOME"' "$source_test_dockerfile" | cut -d: -f1)" \
  -lt "$(grep -n '^ENV CARGO_NET_OFFLINE=true$' "$source_test_dockerfile" | cut -d: -f1)"
grep -Fqx 'source/rust_hft/config/secrets.yaml' "$dockerignore"
grep -Fqx 'source/rust_hft/clickhouse_credentials.txt' "$dockerignore"
if grep -Eqi 'credential|secret|api[_-]?key|password|access[_-]?token' "$source_test_dockerfile" "$source_test_entrypoint"; then
  printf 'source-test image contract mentions a credential surface\n' >&2
  exit 1
fi

mkdir -p "$source_test_tmp_dir/bin"
mkdir -p "$source_test_tmp_dir/cargo-home"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'if [[ "$*" == *" -- --list" ]]; then' \
  '  if [[ "${SOURCE_TEST_EMPTY_LIST:-}" == true ]]; then exit 0; fi' \
  '  printf "%s\\n" "approved::test: test"' \
  'else' \
  '  printf "%s\\n" "$*"' \
  'fi' >"$source_test_tmp_dir/bin/cargo"
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
if SOURCE_TEST_EMPTY_LIST=true CARGO_HOME="$source_test_tmp_dir/cargo-home" XDG_RUNTIME_DIR="$source_test_tmp_dir" \
  PATH="$source_test_tmp_dir/bin:$PATH" sh "$source_test_entrypoint" binance-bstocks-attestation >/dev/null 2>&1; then
  printf 'source-test entrypoint accepted a profile with no matching tests\n' >&2
  exit 1
fi
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
test "$(grep -Fxc '        runAsUser: 1000' "$source_test_job")" -eq 1
test "$(grep -Fxc '        runAsGroup: 1000' "$source_test_job")" -eq 1
test "$(grep -Fxc '            runAsUser: 1000' "$source_test_job")" -eq 1
test "$(grep -Fxc '            runAsGroup: 1000' "$source_test_job")" -eq 1
grep -Fq 'type: RuntimeDefault' "$source_test_job"
grep -Fq 'allowPrivilegeEscalation: false' "$source_test_job"
grep -Fq 'readOnlyRootFilesystem: true' "$source_test_job"
grep -Fq 'emptyDir:' "$source_test_job"
grep -Fq 'research-source-test@sha256:' "$source_test_job"
if grep -Eq 'command:|nodeName:|tolerations:|secretKeyRef:|env:|envFrom:|persistentVolumeClaim:|configMap:|hostPath:' "$source_test_job"; then
  printf 'source-test Job template widens its execution or storage boundary\n' >&2
  exit 1
fi

# research-runner-binaries compiles on the runner and uses per-job local
# sccache; the publish matrix compiles inside docker, where a host-side wrapper
# does not apply. Neither path may write the shared GHA object cache.
grep -Fqx '      RUSTC_WRAPPER: sccache' "$workflow"
grep -Fqx '      SCCACHE_GHA_ENABLED: "false"' "$workflow"
grep -Fqx '        uses: mozilla-actions/sccache-action@v0.0.10' "$workflow"
grep -Fqx '        continue-on-error: true' "$workflow"
if grep -Fq 'sccache --zero-stats' "$workflow" || \
  grep -Fq 'path: ~/.cache/sccache' "$workflow" || \
  grep -Fq -- '}}-${{ github.sha }}' "$workflow"; then
  exit 1
fi

grep -Fqx '          key: research-image-bookworm-${{ steps.cache-info.outputs.rust }}-sccache-${{ steps.cache-info.outputs.sccache }}' "$workflow"
grep -Fqx '          key: research-image-bookworm-${{ steps.cache-info.outputs.rust }}-sccache-${{ steps.cache-info.outputs.sccache }}' "$ploy_workflow"
grep -Fqx '          key: rust_hft-ci-rust-${{ steps.cache-info.outputs.rust }}-sccache-${{ steps.cache-info.outputs.sccache }}' "$ci_workflow"
# rust-cache@v2 hashes the Rust environment and lockfiles by default. Keep the
# user prefix stable so that its restore key survives Cargo.lock changes.
if grep -Eq "key: (research-image-bookworm|rust_hft-ci-rust)-.*hashFiles\\(.*Cargo[.]lock" "$workflow" "$ploy_workflow" "$ci_workflow"; then
  printf 'Rust cache user key duplicates the action Cargo.lock environment hash\n' >&2
  exit 1
fi

grep -Fqx '          name: research-image-release-${{ github.sha }}' "$ploy_workflow"
grep -Fqx '          retention-days: 1' "$ploy_workflow"
test "$(grep -Fxc '            jq \' "$workflow")" -eq 1
test "$(grep -Fxc '            jq \' "$ploy_workflow")" -eq 1
grep -Fqx '          ../.github/scripts/research-image-release-artifact.sh create \' "$ploy_workflow"
grep -Fqx '          .github/scripts/research-image-release-artifact.sh verify research-release \' "$ploy_workflow"

for binary in hft-backtest alpha-harness lob-pit-materializer binance-market-tape-slicer binance-replay-parquet-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  grep -Fq "research-release/research-bin/$binary" <<<"$mode_restore_block"
done

for binary in hft-backtest alpha-harness lob-pit-materializer binance-market-tape-slicer binance-replay-parquet-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
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
  "$tmp_dir/binance-market-tape-slicer" \
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
