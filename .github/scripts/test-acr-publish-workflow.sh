#!/usr/bin/env bash
# shellcheck disable=SC1003,SC2016
set -euo pipefail

case ${1:-} in
  ''|--public-policy) ;;
  *) printf 'unknown test scope: %s\n' "$1" >&2; exit 2 ;;
esac

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
binance_lob_dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.binance-lob-archiver"
market_data_dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.market-data"
sentinel_dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.sentinel"
hft_live_dockerfile="$script_dir/../../rust_hft/ops/hft-live.Dockerfile"
emergency_collector="$script_dir/../../rust_hft/tools/collector/local-emergency-collector.sh"
tmp_dir=$(mktemp -d)
source_test_tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir" "$source_test_tmp_dir"' EXIT

# Public policy validation must work before tools, artifacts or credentials exist.
policy_tools="$tmp_dir/policy-tools"
policy_state="$tmp_dir/policy-state"
mkdir "$policy_tools" "$policy_state"
ln -s "$(command -v jq)" "$policy_tools/jq"
ln -s "$(command -v dirname)" "$policy_tools/dirname"
bash_command=$(command -v bash)
jq -n '
  def oss($product): {
    bucket:"fixture-bucket", region:"fixture-region", endpoint:"https://fixture.invalid/",
    role_arn:("fixture-role/" + $product), oidc_provider_arn:"fixture-provider",
    audience:"fixture-audience", subject:"fixture-subject", repository_id:1, owner_id:2,
    role_prefixes:[("research/builds/" + ("b" * 64) + "/"), ("research/sources/" + ("a" * 40) + "/")]
  };
  {trust:{schema:1,repository:"fixture/repo",producer_workflow_path:".github/workflows/acr-publish.yml",keys:{fixture:("a" * 64)}},
   key_id:"fixture",builder_image:("fixture/builder@sha256:" + ("b" * 64)),
   image_repositories:{"cex-runner":"fixture/cex","prediction-runner":"fixture/prediction",controller:"fixture/controller"},
   oss_by_product:{"cex-runner":oss("cex"),"prediction-runner":oss("prediction"),controller:oss("controller")}}
' >"$tmp_dir/public-policy.json"
run_public_policy() {
  local policy=$1 product=$2 mode=${3:-check-public-policy}
  PATH="$policy_tools" TMPDIR="$policy_state" RUNNER_TEMP="$policy_state" \
    MONDAY_RELEASE_POLICY_JSON="$(<"$policy")" PRODUCT="$product" \
    MONDAY_RELEASE_SIGNING_KEY=EXAMPLE_private-key-must-not-appear \
    MONDAY_RELEASE_GATEWAY_TOKEN=EXAMPLE_private-token-must-not-appear \
    "$bash_command" "$script_dir/publish-research-build-release.sh" "$mode" \
      >"$tmp_dir/public-policy.out" 2>"$tmp_dir/public-policy.err"
}
assert_public_policy_is_offline() {
  local files=()
  shopt -s nullglob dotglob
  files=("$policy_state"/*)
  shopt -u nullglob dotglob
  ((${#files[@]} == 0)) || { echo 'public policy check wrote preparation files' >&2; exit 1; }
  if grep -Eq 'private-(key|token)-must-not-appear' "$tmp_dir/public-policy.out" "$tmp_dir/public-policy.err"; then
    echo 'public policy diagnostic exposed a credential' >&2; exit 1
  fi
}
reject_public_policy() {
  if run_public_policy "$1" "${2:-controller}"; then
    echo 'invalid public policy was admitted' >&2; exit 1
  fi
  test ! -s "$tmp_dir/public-policy.out"
  assert_public_policy_is_offline
}
for product in cex-runner prediction-runner controller; do
  run_public_policy "$tmp_dir/public-policy.json" "$product"
  grep -Fq 'native signing, OIDC and TLS validation remain required' "$tmp_dir/public-policy.out"
  assert_public_policy_is_offline
done
for expression in \
  'null' '[]' \
  'del(.oss_by_product)' '.oss_by_product={}' 'del(.oss_by_product.controller)' \
  '.oss_by_product.foreign=.oss_by_product.controller' \
  '.oss_by_product.controller.role_arn=.oss_by_product["cex-runner"].role_arn' \
  '.trust.schema=2' '.trust.keys.fixture="invalid-public-key"' \
  '.builder_image="fixture/builder:latest"' '.image_repositories=[]' \
  'del(.image_repositories.controller)' '.oss_by_product.controller.bucket=1' \
  '.oss_by_product.controller.role_arn=""' '.oss_by_product.controller.oidc_provider_arn=""'; do
  jq "$expression" "$tmp_dir/public-policy.json" >"$tmp_dir/invalid-public-policy.json"
  reject_public_policy "$tmp_dir/invalid-public-policy.json"
done
printf '{invalid JSON\n' >"$tmp_dir/invalid-public-policy.json"
reject_public_policy "$tmp_dir/invalid-public-policy.json"
cat "$tmp_dir/public-policy.json" "$tmp_dir/public-policy.json" >"$tmp_dir/invalid-public-policy.json"
reject_public_policy "$tmp_dir/invalid-public-policy.json"
reject_public_policy "$tmp_dir/public-policy.json" foreign
for mode in check-config publish; do
  if run_public_policy "$tmp_dir/public-policy.json" controller "$mode"; then
    echo 'public policy bypassed the native issuer gate' >&2; exit 1
  fi
  assert_public_policy_is_offline
done
printf 'Public policy structure, product mapping and pre-issuer failure contracts passed\n'
if [[ ${1:-} == --public-policy ]]; then exit 0; fi

ruby -ryaml - "$workflow" "$ploy_workflow" "$ci_workflow" "$script_dir/../workflows/security-enabled.yml" <<'RUBY'
acr, ploy, ci, security = ARGV.map { |path| YAML.safe_load(File.read(path)) }
abort 'ACR queue changed' unless acr.fetch('concurrency') == {'group'=>'acr-publish-${{ github.ref }}','queue'=>'max','cancel-in-progress'=>false}
selector = ploy.fetch('jobs').fetch('image-smoke-selector')
carry = selector.fetch('steps').find { |step| step['id']=='cumulative' }
abort 'carry deferral escaped main pushes' unless carry.fetch('if') == "github.event_name == 'push' && github.ref == 'refs/heads/main'"
expected_carry = {
  'GH_TOKEN'=>'${{ github.token }}',
  'SELECTED_JOBS'=>'${{ steps.scope.outputs.jobs }}',
  'SELECTED_RESEARCH_PRODUCT'=>'${{ steps.scope.outputs.research_product }}',
  'RESEARCH_CARRY_MODE'=>'defer-unconfigured',
  'MONDAY_RELEASE_POLICY_JSON'=>'${{ vars.MONDAY_RESEARCH_RELEASE_POLICY }}'
}
abort 'carry deferral lost direct scope or public-only policy input' unless carry.fetch('env') == expected_carry
%w[research_pending_product research_deferred_product].each do |name|
  abort 'carry scheduling evidence is missing' unless selector.fetch('outputs').fetch(name) == "${{ steps.cumulative.outputs.#{name} }}"
end
acr_selector = acr.fetch('jobs').fetch('selector')
checkout = acr_selector.fetch('steps').find { |step| step.fetch('uses','').start_with?('actions/checkout@') }
abort 'pending publication needs complete main history' unless checkout.fetch('with') == {'ref'=>'refs/heads/main','fetch-depth'=>0}
abort 'ACR source planning compiles software' if acr_selector.to_s.match?(/\bcargo\s+(build|test|check|clippy|run)\b/)
[acr,ploy,ci,security].each do |doc|
  doc.fetch('jobs').each do |id,job|
    abort "public ACK runner exposure: #{id}" if job.fetch('runs-on','').to_s.match?(/self-hosted|monday-ack-research/)
  end
end
[acr,ploy,ci,security].each do |doc|
  abort 'retired private ACK wait remains in active CI' if doc.to_s.include?('wait-ack-research-receipt.sh')
end
fast=ci.fetch('jobs').fetch('rust_fast_gates')
abort 'static Fast dispatches compute' if fast.to_s.include?('wait-ack') || fast.to_s.include?('ack_research')
abort 'static Fast compiles research' if fast.to_s.match?(/\bcargo\s+(build|test|check|clippy)\b/)
repo_root = File.expand_path('../..', File.dirname(ARGV[0]))
[[ploy,'research-image-binaries'],[acr,'research-runner-binaries']].each do |doc,id|
  job = doc.fetch('jobs').fetch(id)
  abort 'release builder lost bookworm ABI binding' unless job.fetch('container').fetch('image') == 'rust:1.98.1-bookworm@sha256:c49256cbe5ea0188bc658a689500d70c41eb51f009a7a7be209caf60a944f3ec'
  dependencies = job.fetch('steps').find { |step| step.fetch('name','') == 'Install build dependencies' }.fetch('run')
  abort 'bookworm release uses Ubuntu package sources' if dependencies.include?('install-ubuntu-packages.sh')
  %w[gh jq ruby binutils].each { |tool| abort "bookworm release dependency missing: #{tool}" unless dependencies.split.include?(tool) }
  abort 'release compiles whole workspace' if job.to_s.include?('--workspace') || job.to_s.include?('--all-features')
  abort 'release lost bounded native builder' unless job.fetch('steps').any? { |s|s.fetch('run','').include?('build-research-release.sh') }
  job.fetch('steps').each do |step|
    cwd = step['working-directory'] || job.dig('defaults','run','working-directory') || doc.dig('defaults','run','working-directory') || '.'
    step.fetch('run','').scan(/^\s*(?:bash\s+)?((?:\.\.\/)*\.github\/scripts\/[a-zA-Z0-9._-]+\.sh)\b/).flatten.each do |script|
      path = File.expand_path(script, File.expand_path(cwd, repo_root))
      abort "native producer script missing in effective working directory: #{id} #{cwd} #{script}" unless File.file?(path)
    end
  end
end
abort 'cross-run source readback missing' unless acr.fetch('jobs').fetch('publish').fetch('steps').any? { |s|s.fetch('run','').include?('download-research-release.sh') }
abort 'release relationship changed' unless acr.fetch('jobs').fetch('research-release-complete').fetch('needs') == ['selector','publish']
RUBY
bash "$script_dir/test-research-runtime-abi.sh"
bash "$script_dir/test-research-checkout-ownership.sh"
# Preserve authenticated exact-source native three-workflow admission.
grep -Fq 'Read authenticated release admission' "$workflow"
grep -Fq '.github/scripts/read-acr-publish-source.sh "$SOURCE_SHA" "$GITHUB_RUN_ID"' "$workflow"
grep -Fq -- '--monorepo-conclusion "$MONOREPO_CONCLUSION"' "$workflow"
grep -Fq -- '--prediction-conclusion "$PREDICTION_CONCLUSION"' "$workflow"
grep -Fq -- '--security-conclusion "$SECURITY_CONCLUSION"' "$workflow"
grep -Fq 'Revalidate current main before publication' "$workflow"
grep -Fq '.github/scripts/wait-release-required-checks.sh "$SOURCE_REVISION" current-main' "$workflow"
if grep -Fq 'research-data-service' "$workflow"; then echo 'release refers to unimplemented data service image' >&2; exit 1; fi
grep -Fq 'research-image-smoke.sh' "$ploy_workflow"
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
# A native producer owns immutable bytes once; smoke and publication consume it.
ruby -ryaml - "$workflow" "$ploy_workflow" <<'RUBY'
acr,ploy=ARGV.map { |path| YAML.safe_load(File.read(path)) }
[[ploy,'research-image-binaries','github.sha'],[acr,'research-runner-binaries','needs.selector.outputs.source_sha']].each do |doc,id,sha|
  steps=doc.fetch('jobs').fetch(id).fetch('steps')
  expected="research-image-release-${{ #{sha} }}-${{ #{id=='research-image-binaries' ? 'needs.image-smoke-scope' : 'needs.selector'}.outputs.research_product }}"
  validate = lambda do |entries|
    uploads=entries.select { |step| step.fetch('uses','').include?('actions/upload-artifact@') && step.fetch('with',{}).fetch('name','')==expected }
    raise 'missing or duplicate immutable software upload' unless uploads.length==1
    upload=uploads.fetch(0)
    raise 'incorrect release boundary' unless upload.fetch('with').fetch('path')=='${{ runner.temp }}/research-image-release.tar' && upload.fetch('with').fetch('if-no-files-found')=='error'
    upload
  end
  upload=validate.call(steps)
  timing={'uses'=>'actions/upload-artifact@fixture','with'=>{'name'=>'recipe-timing','path'=>'timings.jsonl'}}
  validate.call([timing,upload])
  validate.call([upload,timing])
  wrong_name=Marshal.load(Marshal.dump(upload)); wrong_name['with']['name']='other-release'
  wrong_path=Marshal.load(Marshal.dump(upload)); wrong_path['with']['path']='timings.jsonl'
  warning=Marshal.load(Marshal.dump(upload)); warning['with']['if-no-files-found']='warn'
  [[timing], [timing,wrong_name], [timing,wrong_path], [upload,upload], [warning,timing]].each do |invalid|
    rejected=false
    begin; validate.call(invalid); rescue RuntimeError; rejected=true; end
    abort 'invalid immutable software upload accepted' unless rejected
  end
end
publication=acr.fetch('jobs').fetch('publish')
abort 'release job cannot obtain its own OIDC identity' unless publication.fetch('permissions')=={'actions'=>'read','checks'=>'read','contents'=>'read','id-token'=>'write','pull-requests'=>'read'}
abort 'binary predecessor removed' unless publication.fetch('needs')==['selector','research-runner-binaries']
steps=publication.fetch('steps')
presence=steps.index { |s|s.fetch('name','')=='Require research publication settings before preparation' }
public_policy=steps.index { |s|s.fetch('name','')=='Validate public research publication policy before preparation' }
download=steps.index { |s|s.fetch('name','')=='Download authenticated research release' }
preflight=steps.index { |s|s.fetch('name','')=='Require independent Build signer configuration' }
compile=steps.index { |s|s.fetch('name','')=='Compile independent release issuer before secret injection' }
login=steps.index { |s|s.fetch('name','')=='Log in to ACR' }
push=steps.index { |s|s.fetch('name','')=='Build and push' }
abort 'issuer policy/TLS validation occurs after registry mutation' unless preflight && login && push && preflight<login && preflight<push
abort 'issuer compilation can access injected release credentials' unless compile && compile<preflight && steps.fetch(compile).fetch('if')=='matrix.research_artifact' && steps.fetch(compile).fetch('env').keys==['CARGO_TARGET_DIR'] && steps.fetch(compile).fetch('run').include?('cargo build')
abort 'capability exchange was not compiled before secrets' unless steps.fetch(compile).fetch('run').include?('--bin research-release-capability')
abort 'missing settings can reach expensive publication preparation' unless presence && download && presence<download && presence<compile && steps.fetch(presence).fetch('if')=='matrix.research_artifact' && steps.fetch(presence).fetch('run').include?('publish-research-build-release.sh check-presence')
presence_env=steps.fetch(presence).fetch('env')
expected_presence={
  'MONDAY_RELEASE_POLICY_PRESENT'=>"${{ vars.MONDAY_RESEARCH_RELEASE_POLICY != '' }}",
  'MONDAY_RELEASE_SIGNING_KEY_PRESENT'=>"${{ secrets.MONDAY_RESEARCH_RELEASE_SIGNING_KEY != '' }}",
  'MONDAY_RELEASE_OSS_PRESENT'=>"${{ vars.MONDAY_RESEARCH_RELEASE_POLICY != '' }}"
}
abort 'cheap configuration check receives credentials or loses OSS binding' unless presence_env==expected_presence
abort 'invalid public policy can reach expensive preparation' unless public_policy && presence<public_policy && public_policy<download && public_policy<compile
policy_step=steps.fetch(public_policy)
abort 'public policy check receives private credentials or loses product binding' unless policy_step.fetch('if')=='matrix.research_artifact' && policy_step.fetch('env')=={
  'MONDAY_RELEASE_POLICY_JSON'=>'${{ vars.MONDAY_RESEARCH_RELEASE_POLICY }}',
  'PRODUCT'=>'${{ matrix.product }}'
} && policy_step.fetch('run')=='.github/scripts/publish-research-build-release.sh check-public-policy'
wrapper=File.read(File.join(File.dirname(ARGV[0]),'../scripts/publish-research-build-release.sh'))
abort 'issuer wrapper compiles while holding release credentials' if wrapper.match?(/\bcargo\s+(?:build|run)\b/)
abort 'static gateway credential still authorizes publication' if steps.any? { |s| s.fetch('env',{}).values.any? { |v| v.to_s.include?('secrets.MONDAY_RESEARCH_RELEASE_GATEWAY_TOKEN') } }
abort 'OSS source preflight missing' unless wrapper.include?('"$capability" oss-source')
abort 'CI still imports to PG or requires a release broker' if wrapper.include?('MONDAY_RELEASE_BROKER') || wrapper.include?('MONDAY_RELEASE_IMPORT_DATABASE_URL') || steps.any? { |s|s.fetch('name','')=='Project independently verified Build releases to PG' }
plan=wrapper.index('"${native[@]}" plan')
exchange=wrapper.index('"$capability" oss-publish')
sign=wrapper.index('"${native[@]}" oss-publish')
abort 'Build writing is not scoped from the actual native plan before signing' unless plan && exchange && sign && plan<exchange && exchange<sign
config=steps.fetch(preflight)
abort 'issuer preflight does not bind the selected image product' unless config.fetch('if')=='matrix.research_artifact' && config.fetch('env').fetch('PRODUCT')=='${{ matrix.product }}' && config.fetch('env').fetch('PUBLISH_IMAGE_REPOSITORY')=='${{ vars.ACR_REGISTRY }}/wildcard0923/${{ matrix.repository }}' && config.fetch('run').include?('publish-research-build-release.sh check-config')
readback=publication.fetch('steps').find { |s|s.fetch('name','')=='Read back research image source and executable bytes' }
abort 'independent executable readback missing' unless readback && readback.fetch('if')=='matrix.research_artifact' && readback.fetch('run').include?('docker pull') && readback.fetch('run').include?('verify-research-product-image.sh')
logout=publication.fetch('steps').index { |s|s.fetch('name','')=='Remove ACR credentials' }
abort 'registry identity removed before image readback' unless logout && logout>publication.fetch('steps').index(readback)
source=acr.fetch('jobs').fetch('publish-source-test')
abort 'source test lost offline fixed profile' unless source.fetch('steps').any? { |s|s.fetch('run','').include?('docker run --rm --network none') }
RUBY
# Missing names fail before files, external tools or the native issuer are used.
# Presence alone must never satisfy the later native configuration gate.
ruby -ropen3 -rtmpdir - "$script_dir/publish-research-build-release.sh" <<'RUBY'
wrapper=File.expand_path(ARGV.fetch(0))
Dir.mktmpdir('release-presence-contract') do |sandbox|
  base={'PATH'=>sandbox,'TMPDIR'=>sandbox,'RUNNER_TEMP'=>sandbox,
    'MONDAY_RELEASE_SIGNING_KEY'=>'private-key-must-not-appear',
    'MONDAY_RELEASE_GATEWAY_TOKEN'=>'private-token-must-not-appear'}
  present=%w[POLICY SIGNING_KEY OSS].to_h { |name| ["MONDAY_RELEASE_#{name}_PRESENT",'true'] }
  run=lambda do |env,mode|
    stdout,stderr,status=Open3.capture3(base.merge(env),'/bin/bash',wrapper,mode,unsetenv_others:true)
    abort 'configuration preflight wrote private files' unless Dir.children(sandbox).empty?
    abort 'configuration diagnostic exposed a credential' if (stdout+stderr).include?('private-key-must-not-appear') || (stdout+stderr).include?('private-token-must-not-appear')
    [stdout+stderr,status.success?]
  end
  text,ok=run.call({},'check-presence')
  abort 'missing release settings were admitted' if ok
  %w[POLICY SIGNING_KEY OSS].each do |name|
    abort "operator diagnostic omitted repository setting #{name}" unless text.include?(name == "OSS" ? "OSS OIDC" : "MONDAY_RESEARCH_RELEASE_#{name}")
  end
  _,ok=run.call(present,'check-presence')
  abort 'configured publication requires a compiler or credentials during cheap preflight' unless ok
  _,ok=run.call(present.merge('MONDAY_RELEASE_OSS_PRESENT'=>'false','MONDAY_RELEASE_GATEWAY_TOKEN_PRESENT'=>'true'),'check-presence')
  abort 'old static gateway secret bypassed missing OSS configuration' if ok
  _,ok=run.call(present.merge('MONDAY_RELEASE_POLICY_PRESENT'=>'yes'),'check-presence')
  abort 'invalid presence flag was treated as present' if ok
  _,ok=run.call(present,'check-config')
  abort 'presence booleans bypassed native credential validation' if ok
end
RUBY
# Exercise the actual target matrix together with the controller step predicate.
# This catches a valid-looking predicate that is false for every selected row.
ruby -ryaml - "$workflow" "$tmp_dir" <<'RUBY'
acr=YAML.safe_load(File.read(ARGV[0]))
selector=acr.fetch('jobs').fetch('selector').fetch('steps').find { |s|s['id']=='select' }
File.write(File.join(ARGV[1],'select-matrix.sh'),selector.fetch('run'))
controller=acr.fetch('jobs').fetch('publish').fetch('steps').find { |s|s['name']=='Verify Campaign cycle controller image' }
File.write(File.join(ARGV[1],'controller-condition.txt'),controller.fetch('if'))
abort 'controller verifier is not used by publication' unless controller.fetch('run').include?('verify-research-controller-image.sh')
complete=acr.fetch('jobs').fetch('research-release-complete')
abort 'completion marker can bypass failed publication' unless complete.fetch('needs').include?('publish') && complete.fetch('if').include?("needs.publish.result == 'success'")
RUBY
for publish_target in all research-runner prediction-research-runner campaign-cycle-controller research-products; do
  : >"$tmp_dir/matrix-output"
  TARGET="$publish_target" PUBLISHED_PRODUCTS=controller,prediction-runner GITHUB_OUTPUT="$tmp_dir/matrix-output" bash "$tmp_dir/select-matrix.sh"
  sed 's/^matrix=//' "$tmp_dir/matrix-output" >"$tmp_dir/matrix.json"
  ruby -rjson - "$tmp_dir/matrix.json" "$tmp_dir/controller-condition.txt" "$publish_target" <<'RUBY'
rows=JSON.parse(File.read(ARGV[0])).fetch('include')
expected = case ARGV[2]
when 'all' then %w[research-runner prediction-research-runner campaign-cycle-controller hft-trading binance-lob-archiver polymarket-evidence-compiler polymarket-market-recorder]
when 'research-products' then %w[campaign-cycle-controller prediction-research-runner]
else [ARGV[2]]
end
abort 'product publication matrix contains another domain' unless rows.map { |row| row.fetch('repository') }.sort == expected.sort
controller=rows.find { |row|row['repository']=='campaign-cycle-controller' }
abort 'actual publish matrix misconfigured controller' if controller && controller['research_artifact']!=true
condition=File.read(ARGV[1]).strip.sub(/\A\$\{\{\s*/,'').sub(/\s*\}\}\z/,'')
tokens=condition.scan(/matrix\.(?:repository|research_artifact)|'[^']*'|true|false|==|!=|&&|\|\||[!()]|\s+/)
abort 'unrecognized controller predicate' unless tokens.join==condition
expression=condition.gsub('matrix.repository','row.fetch("repository")').gsub('matrix.research_artifact','row.fetch("research_artifact")')
rows.each do |row|
  actual=eval(expression,binding)
  abort "controller verification unreachable or misselected: #{row['repository']}" unless actual==(row['repository']=='campaign-cycle-controller')
end
RUBY
done
# Selector still owns approved source-test SHA/profile/tag; no public job may
# accept a free-form compiler command or recreate a hosted source-test build.
grep -Fqx '      source_test_profile: ${{ steps.source.outputs.source_test_profile }}' "$workflow"
grep -Fqx '      source_test_tag: ${{ steps.source.outputs.source_test_tag }}' "$workflow"
grep -Fqx '          SOURCE_TEST_SOURCE_SHA: ${{ inputs.source_test_source_sha }}' "$workflow"
grep -Fqx '          SOURCE_TEST_PROFILE: ${{ inputs.source_test_profile }}' "$workflow"
grep -Fqx '            --source-test-sha "$SOURCE_TEST_SOURCE_SHA" \' "$workflow"
grep -Fqx '            --source-test-profile "$SOURCE_TEST_PROFILE" \' "$workflow"
grep -Fqx 'FROM rust:1.98.1-bookworm@sha256:9a73a5088750b4c95158ab26629c854c3d6fc4b173cb7bc8079ad252d8ed7bfa AS source-test' "$source_test_dockerfile"
grep -Fq 'groupadd --gid 1000 research' "$source_test_dockerfile"
grep -Fqx '    && useradd --create-home --uid 1000 --gid 1000 research' "$source_test_dockerfile"
grep -Fqx 'COPY --chown=research:research source/rust_hft/ /work/' "$source_test_dockerfile"
grep -Fqx 'RUN cargo fetch --manifest-path runtime/Cargo.toml --locked && chown -R research:research "$CARGO_HOME"' "$source_test_dockerfile"
grep -Fqx 'USER 1000:1000' "$source_test_dockerfile"
grep -Fqx '    CARGO_HOME=/opt/monday-source-test-cargo \' "$source_test_dockerfile"
grep -Fqx 'ENTRYPOINT ["/usr/local/bin/monday-source-test"]' "$source_test_dockerfile"
grep -Fqx 'export CARGO_BUILD_JOBS=2' "$source_test_entrypoint"
grep -Fqx 'export CARGO_TARGET_DIR=/tmp/monday-source-test-target' "$source_test_entrypoint"
test "$(grep -n -F 'RUN cargo fetch --manifest-path runtime/Cargo.toml --locked && chown -R research:research "$CARGO_HOME"' "$source_test_dockerfile" | cut -d: -f1)" \
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
diff -u <(printf '%s\n' 'test --manifest-path runtime/Cargo.toml --offline --locked -p hft-runtime --lib tokenized_security_requires_runtime_owned_attestation') \
  "$source_test_tmp_dir/binance-source-test.out"
CARGO_HOME="$source_test_tmp_dir/cargo-home" XDG_RUNTIME_DIR="$source_test_tmp_dir" \
  PATH="$source_test_tmp_dir/bin:$PATH" sh "$source_test_entrypoint" bybit-spot \
  >"$source_test_tmp_dir/bybit-source-test.out"
diff -u <(printf '%s\n' 'test --manifest-path runtime/Cargo.toml --offline --locked -p hft-execution-adapter-bybit --lib') \
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
"$script_dir/test-download-research-release.sh"
bash "$script_dir/test-research-controller-image.sh"
printf 'ACR native build, fixed domain tests and release metadata contracts passed\n'

# Exercise the exact policy selector used by CI, including multiple products.
selector="$script_dir/select-research-oss-policy.jq"
jq -n '{trust:{schema:1},oss_by_product:{"cex-runner":{role_arn:"role/cex",role_prefixes:["cex-exact"]},"prediction-runner":{role_arn:"role/prediction",role_prefixes:["prediction-exact"]},controller:{role_arn:"role/controller",role_prefixes:["controller-exact"]}}}' >"$tmp_dir/oss-products.json"
for product in cex-runner prediction-runner controller; do
  jq -e --arg product "$product" -f "$selector" "$tmp_dir/oss-products.json" >"$tmp_dir/oss-selected.json"
  jq -e --arg product "$product" --slurpfile approved "$tmp_dir/oss-products.json" '.oss==$approved[0].oss_by_product[$product] and (.oss.role_prefixes|length)==1 and (has("oss_by_product")|not) and .trust.schema==1' "$tmp_dir/oss-selected.json" >/dev/null
done
if jq -e --arg product foreign -f "$selector" "$tmp_dir/oss-products.json" >/dev/null 2>&1; then exit 1; fi
jq '.oss_by_product.controller.role_arn=.oss_by_product["cex-runner"].role_arn' "$tmp_dir/oss-products.json" >"$tmp_dir/oss-shared-role.json"
if jq -e --arg product controller -f "$selector" "$tmp_dir/oss-shared-role.json" >/dev/null 2>&1; then exit 1; fi
bash "$script_dir/test-migrate-research-oss-policy.sh"
