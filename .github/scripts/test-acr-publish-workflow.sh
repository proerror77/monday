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
root_toolchain="$script_dir/../../rust-toolchain.toml"
binance_lob_dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.binance-lob-archiver"
market_data_dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.market-data"
sentinel_dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.sentinel"
hft_live_dockerfile="$script_dir/../../rust_hft/ops/hft-live.Dockerfile"
emergency_collector="$script_dir/../../rust_hft/tools/collector/local-emergency-collector.sh"
verifier="$script_dir/verify-research-runner-binaries.sh"
tmp_dir=$(mktemp -d)
source_test_tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir" "$source_test_tmp_dir"' EXIT
ruby -ryaml - "$workflow" "$ploy_workflow" "$ci_workflow" "$script_dir/../workflows/security-enabled.yml" <<'RUBY'
acr, ploy, ci, security = ARGV.map { |path| YAML.safe_load(File.read(path)) }
abort 'ACR queue changed' unless acr.fetch('concurrency') == {'group'=>'acr-publish-${{ github.ref }}','queue'=>'max','cancel-in-progress'=>false}
[acr,ploy,ci,security].each do |doc|
  doc.fetch('jobs').each do |id,job|
    abort "public ACK runner exposure: #{id}" if job.fetch('runs-on','').to_s.match?(/self-hosted|monday-ack-research/)
  end
end
[[ploy,%w[research-image-binaries research-image-smoke rust-format rust-research-heavy]], [acr,%w[research-runner-binaries publish-source-test]]].each do |doc,ids|
  ids.each do |id|
    steps=doc.fetch('jobs').fetch(id).fetch('steps')
    abort "missing signed ACK relay: #{id}" unless steps.any? { |step| step.fetch('run','').include?('wait-ack-research-receipt.sh') }
    steps.each do |step|
      abort "hosted research execution: #{id}" if step.fetch('run','').match?(/\bcargo\s|\bdocker\s/) || step.fetch('uses','').match?(/docker\/|rust-toolchain|rust-cache|sccache/)
    end
  end
end
[[ci,'rust'],[security,'clippy-strict']].each do |doc,id|
  doc.fetch('jobs').fetch(id).fetch('steps').each do |step|
    next if step.fetch('uses','').include?('actions/checkout@') || step.fetch('run','').include?('wait-ack-research-receipt.sh')
    abort "unguarded mixed research job #{id}" unless step.fetch('if','').include?("outputs.ack_research != 'true'")
  end
end
fast=ci.fetch('jobs').fetch('rust_fast_gates')
abort 'static Fast dispatches ACK' if fast.to_s.include?('wait-ack') || fast.to_s.include?('ack_research')
abort 'static Fast compiles research' if fast.to_s.match?(/\bcargo\s+(build|test|check|clippy)\b/)
acr.fetch('jobs').fetch('publish').fetch('steps').each do |step|
  next unless step.fetch('run','').match?(/\bdocker\s/) || step.fetch('uses','').match?(/docker\//)
  abort 'public research image build, smoke or publication' unless step.fetch('if','').include?('!matrix.research_artifact')
end
abort 'release relationship changed' unless acr.fetch('jobs').fetch('research-release-complete').fetch('needs') == ['selector','publish']
RUBY
# Preserve authenticated exact-source native three-workflow admission.
grep -Fq 'Read authenticated release admission' "$workflow"
grep -Fq '.github/scripts/read-acr-publish-source.sh "$SOURCE_SHA" "$GITHUB_RUN_ID"' "$workflow"
grep -Fq -- '--monorepo-conclusion "$MONOREPO_CONCLUSION"' "$workflow"
grep -Fq -- '--prediction-conclusion "$PREDICTION_CONCLUSION"' "$workflow"
grep -Fq -- '--security-conclusion "$SECURITY_CONCLUSION"' "$workflow"
grep -Fq 'Revalidate current main before publication' "$workflow"
grep -Fq '.github/scripts/wait-release-required-checks.sh "$SOURCE_REVISION" current-main' "$workflow"
grep -Fq 'research-data-service' "$workflow"
grep -Fq 'verify-metadata' "$ploy_workflow"
grep -Fqx 'FROM rust:1.98.1-bookworm AS builder' "$market_data_dockerfile"
grep -Fqx 'FROM rust:1.98.1-bookworm AS builder' "$sentinel_dockerfile"
grep -Fqx 'FROM rust:1.98.1-slim-bookworm AS builder' "$hft_live_dockerfile"
for bullseye_builder in "$binance_lob_dockerfile" "$emergency_collector"; do
  grep -Fqx 'FROM rust:1.98-bullseye@sha256:4730e387a220a08a365c77da3096544dde214f9d796c16284d4be45438cad4a9 AS builder' "$bullseye_builder"
  grep -Fqx 'ENV RUST_VERSION=1.98.1' "$bullseye_builder"
  grep -Fq 'rustup toolchain install 1.98.1 --profile minimal --no-self-update \' "$bullseye_builder"
  grep -Fqx '    && rustup default 1.98.1 \' "$bullseye_builder"
  grep -Fqx "    && rustc --version | grep -E '^rustc 1\\.98\\.1 '" "$bullseye_builder"
done
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
controller_container_block=$(sed -n '/^      containers:$/,/^      initContainers:$/p' "$controller_job")
if grep -Eq '^[[:space:]]+command:' <<<"$controller_container_block"; then
  printf 'ACK controller Job bypasses the image entrypoint\n' >&2
  exit 1
fi
# Research build/smoke/publication commands belong to fixed private profiles.
# Public checks bind each terminal receipt to the actual checkout; binary
# software metadata stays bound to the public producer run before upload.
ruby -ryaml - "$workflow" "$ploy_workflow" <<'RUBY'
acr,ploy=ARGV.map { |path| YAML.safe_load(File.read(path)) }
profiles={
  'research-image-binaries'=>'research-image-binaries',
  'research-image-smoke'=>'research-image-smoke',
  'rust-format'=>'prediction-research-format',
  'rust-research-heavy'=>'prediction-research-heavy'
}
[[ploy,profiles],[acr,{'research-runner-binaries'=>'research-release-binaries','publish-source-test'=>'research-source-test','publish'=>'research-release-publish'}]].each do |doc,mapping|
  mapping.each do |id,profile|
    job=doc.fetch('jobs').fetch(id)
    relay=job.fetch('steps').select { |s| s.fetch('run','').include?('wait-ack-research-receipt.sh') }
    abort "ambiguous relay #{id}" unless relay.length==1
    command=relay.first.fetch('run')
    abort "wrong private profile or source #{id}" unless command.include?("wait-ack-research-receipt.sh #{profile} \"$(git rev-parse HEAD)\"")
    if doc.equal?(acr)
      checkout=job.fetch('steps').find { |s| s.fetch('uses','').include?('actions/checkout@') }
      abort "unbound publication source #{id}" unless checkout.fetch('with').fetch('ref')=='${{ needs.selector.outputs.source_sha }}'
    end
  end
end
[[ploy,'research-image-binaries','github.sha'],[acr,'research-runner-binaries','needs.selector.outputs.source_sha']].each do |doc,id,sha|
  steps=doc.fetch('jobs').fetch(id).fetch('steps')
  verify=steps.find { |s| s.fetch('run','').include?('verify-metadata') }
  abort "unbound software producer #{id}" unless verify && verify.fetch('run').include?('"$(git rev-parse HEAD)" "$GITHUB_RUN_ID" rust_hft')
  upload=steps.find { |s| s.fetch('uses','').include?('actions/upload-artifact@') }
  abort "missing public release upload #{id}" unless upload
  with=upload.fetch('with')
  abort "wrong release artifact identity #{id}" unless with.fetch('name')=="research-image-release-${{ #{sha} }}"
  abort "software boundary changed #{id}" unless with.fetch('path')=='${{ runner.temp }}/ack-receipt/software/' && with.fetch('if-no-files-found')=='error'
end
publication=acr.fetch('jobs').fetch('publish')
abort 'binary predecessor removed' unless publication.fetch('needs')==['selector','research-runner-binaries']
readback=publication.fetch('steps').find { |s| s.fetch('name','')=='Read back signed research image identity' }
abort 'research publication readback absent' unless readback && readback.fetch('if')=='matrix.research_artifact'
%w[.repository .source_sha .smoke_result .digest .image_ref].each do |field|
  abort "publication proof omits #{field}" unless readback.fetch('run').include?(field)
end
abort 'publication proof allows ambiguous images' unless readback.fetch('run').include?('length == 1')
RUBY
# Selector still owns approved source-test SHA/profile/tag; no public job may
# accept a free-form compiler command or recreate a hosted source-test build.
grep -Fqx '      source_test_profile: ${{ steps.source.outputs.source_test_profile }}' "$workflow"
grep -Fqx '      source_test_tag: ${{ steps.source.outputs.source_test_tag }}' "$workflow"
grep -Fqx '          SOURCE_TEST_SOURCE_SHA: ${{ inputs.source_test_source_sha }}' "$workflow"
grep -Fqx '          SOURCE_TEST_PROFILE: ${{ inputs.source_test_profile }}' "$workflow"
grep -Fqx '            --source-test-sha "$SOURCE_TEST_SOURCE_SHA" \' "$workflow"
grep -Fqx '            --source-test-profile "$SOURCE_TEST_PROFILE" \' "$workflow"
# Signed bridge refuses forks, unapproved profiles and unverifiable receipts;
# source identity includes run/job/profile, not just a success status string.
relay_script="$script_dir/wait-ack-research-receipt.sh"
grep -Fq 'openssl pkeyutl -verify -pubin' "$relay_script"
grep -Fq '.public_run_id == $run and .public_job == $job' "$relay_script"
grep -Fq '.checkout_sha == $source and .profile == $profile and' "$relay_script"
# Success requires ACK; signed negative results may terminate an unverified wait.
grep -Fq '(.execution_host == "ack" or (.terminal_result=="failure" and .execution_host=="unverified" and' "$relay_script"
grep -Fq '(.execution_state|IN("unverified","not_admitted")))) and' "$relay_script"
grep -Fq 'Unknown private ACK execution profile' "$relay_script"
grep -Fq 'Fork research jobs require independent source admission' "$relay_script"
grep -Fq 'sha256sum -c -' "$relay_script"
grep -Fq 'Selected' "$script_dir/classify-ack-research-job.sh"

grep -Fqx 'FROM rust:1.98.1-bookworm@sha256:9a73a5088750b4c95158ab26629c854c3d6fc4b173cb7bc8079ad252d8ed7bfa AS source-test' "$source_test_dockerfile"
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


"$script_dir/test-research-image-release-artifact.sh"
"$script_dir/test-acr-publish-source-readback.sh"
"$script_dir/test-classify-ack-research-job.sh"
printf 'ACR ACK execution and release metadata contracts passed\n'
