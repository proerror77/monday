#!/usr/bin/env bash
# Synthetic transport/provenance fixtures; no signing, registry or PG access.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export MOCK_WORK=$work GITHUB_REPOSITORY=fixture/monday
source_sha=1111111111111111111111111111111111111111
ruby -rjson -ryaml -rfileutils - "$root" "$work" "$source_sha" <<'RUBY'
root, work, source = ARGV
helper = File.join(root, '.github/scripts/research-release-tools-artifact.rb')
bundle = File.join(root, '.github/scripts/research-release-bundle.rb')
recipe = JSON.parse(File.read(File.join(root, '.github/scripts/research-release-tools.json')))
release = File.join(work, 'release')
FileUtils.mkdir_p(File.join(release, 'release-tools'))
recipe.fetch('binaries').each do |name|
  file = File.join(release, 'release-tools', name)
  File.write(file, "synthetic executable fixture: #{name}\n")
  File.chmod(0o755, file)
end
workflow = '.github/workflows/ploy-ci.yml'
job = YAML.safe_load(File.read(File.join(root, workflow))).fetch('jobs').fetch('research-release-tools')
evidence = File.join(work, 'compiler.json')
File.write(evidence, JSON.generate({builder_image: job.fetch('container').fetch('image'),
  compiler: {rustc_version: 'rustc 1.98.1 fixture', cargo_version: 'cargo 1.98.1 fixture', rustc_sha256: 'a'*64,
    cargo_sha256: 'b'*64, sysroot_sha256: 'c'*64}, flags_sha256: 'd'*64, native_sha256: 'e'*64}))
identity = [source, '1234', '2', '567', workflow]
raise 'fixture create failed' unless system('ruby', helper, 'create', release, *identity, evidence)
raise 'fixture verify failed' unless system('ruby', helper, 'verify', release, *identity)
raise 'fixture pack failed' unless system('ruby', bundle, 'pack-tools', File.join(work, 'research-release-tools.tar'), release)
manual = File.join(work, 'manual-release')
FileUtils.cp_r(release, manual)
File.unlink(File.join(manual, 'research-release-tools.json'))
raise 'manual fixture create failed' unless system('ruby', helper, 'create', manual, *identity[0...4], '.github/workflows/acr-publish.yml', evidence)
FileUtils.mkdir_p(File.join(work, 'manual'))
raise 'manual fixture pack failed' unless system('ruby', bundle, 'pack-tools', File.join(work, 'manual/research-release-tools.tar'), manual)
%w[source run attempt job workflow profile compiler lock recipe hash missing extra symlink mode].each do |failure|
  directory = File.join(work, failure)
  FileUtils.cp_r(release, directory)
  manifest = File.join(directory, 'research-release-tools.json')
  data = JSON.parse(File.read(manifest))
  first = File.join(directory, 'release-tools', recipe.fetch('binaries').first)
  case failure
  when 'source' then data['source_sha'] = '2'*40
  when 'run' then data['workflow_run_id'] = '99'
  when 'attempt' then data['workflow_run_attempt'] = 3
  when 'job' then data['workflow_job_id'] = 568
  when 'workflow' then data['workflow_path'] = '.github/workflows/ci.yml'
  when 'profile' then data['profile'] = 'dev'
  when 'compiler' then data['build_inputs']['compiler']['rustc_sha256'] = 'not a hash'
  when 'lock' then data['build_inputs']['inputs']['rust_hft/research-core/platform/Cargo.lock'] = 'f'*64
  when 'recipe' then data['build_inputs']['recipe']['features'] = 'control'
  when 'hash' then File.write(first, 'tampered executable')
  when 'missing' then File.unlink(first)
  when 'extra' then File.write(File.join(directory, 'release-tools', 'researchctl'), 'foreign executable')
  when 'symlink' then File.unlink(first); File.symlink('/bin/sh', first)
  when 'mode' then File.chmod(0o644, first)
  end
  File.write(manifest, JSON.generate(data))
  raise "accepted invalid tools #{failure}" if system('ruby', helper, 'verify', directory, *identity, out: File::NULL, err: File::NULL)
end
# A tools bundle cannot enter the image product transport, or vice versa.
raise 'tools admitted as a fourth image product' if system('ruby', bundle, 'unpack', File.join(work, 'research-release-tools.tar'), File.join(work, 'wrong-product'), 'controller', out: File::NULL, err: File::NULL)
%w[ploy-ci acr-publish].each do |name|
  document = YAML.safe_load(File.read(File.join(root, ".github/workflows/#{name}.yml")))
  producer = document.fetch('jobs').fetch('research-release-tools')
  raise 'tools producer has extra authority' unless producer.fetch('permissions') == {'contents'=>'read','actions'=>'read'}
  raise 'tools producer gained signing credentials' if producer.to_s.match?(/secrets\.|id-token|MONDAY_RELEASE_SIGNING/)
  raise 'missing immutable tools upload' unless producer.fetch('steps').any? { |step| step.fetch('with', {})['path'] == '${{ runner.temp }}/research-release-tools.tar' }
end
puts 'PASS: tools fixed file set, source/producer/profile/compiler/lock/hash and producer permissions'
RUBY
(cd "$work" && zip -q "$work/release.zip" research-release-tools.tar)
(cd "$work/manual" && zip -q "$work/manual.zip" research-release-tools.tar)
mkdir -p "$work/bin"
cat >"$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
endpoint=${!#}
mode=${MOCK_MODE:-ok}
sha=1111111111111111111111111111111111111111
for arg in "$@"; do [[ $arg != --slurp ]] || exit 91; done
case "$endpoint" in
  */actions/runs/1234)
    count=0; [[ ! -f $MOCK_WORK/count ]] || count=$(cat "$MOCK_WORK/count")
    printf '%s\n' "$((count+1))" >"$MOCK_WORK/count"
    attempt=2; [[ $mode != rerun || $count == 0 ]] || attempt=3
    path=.github/workflows/ploy-ci.yml; [[ $mode != wrong-workflow ]] || path=.github/workflows/ci.yml
    [[ $mode != wrong-source ]] || sha=2222222222222222222222222222222222222222
    conclusion=success; [[ $mode != failed-run ]] || conclusion=failure
    event=push; run_state=completed
    if [[ $mode == manual ]]; then path=.github/workflows/acr-publish.yml; event=workflow_dispatch; run_state=in_progress; fi
    jq -n --arg event "$event" --arg state "$run_state" --arg sha "$sha" --arg path "$path" --arg conclusion "$conclusion" --argjson attempt "$attempt" \
      '{id:1234,head_sha:$sha,head_branch:"main",head_repository:{full_name:"fixture/monday"},path:$path,event:$event,status:$state,conclusion:$conclusion,run_attempt:$attempt}' ;;
  */attempts/2/jobs\?*)
    [[ $mode != malformed-pages ]] || { printf '{bad-json}\n'; exit; }
    if [[ $mode == missing-job ]]; then printf '{"jobs":[]}\n'; exit; fi
    id=567; [[ $mode != wrong-job ]] || id=999
    jq -n --argjson id "$id" '{jobs:[{id:$id,name:"Research release tools",run_id:1234,run_attempt:2,status:"completed",conclusion:"success"}]}' ;;
  */artifacts\?*)
    if [[ $mode == missing-artifact ]]; then printf '{"artifacts":[]}\n'; exit; fi
    expired=false; [[ $mode != expired ]] || expired=true
    jq -n --arg name "research-release-tools-$sha" --arg sha "$sha" --argjson expired "$expired" \
      '{artifacts:[{id:890,name:$name,expired:$expired,size_in_bytes:4096,workflow_run:{id:1234,head_sha:$sha}}]}' ;;
  */artifacts/890/zip) if [[ $mode == manual ]]; then cat "$MOCK_WORK/manual.zip"; else cat "$MOCK_WORK/release.zip"; fi ;;
  */actions/jobs/567)
    conclusion=success; [[ $mode != job-drift ]] || conclusion=failure
    jq -n --arg conclusion "$conclusion" '{id:567,run_id:1234,run_attempt:2,status:"completed",conclusion:$conclusion}' ;;
  *) exit 92 ;;
esac
MOCK
cat >"$work/bin/git" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ " $* " == *" diff --quiet HEAD -- . " ]]; then
  [[ ${MOCK_MODE:-} != dirty-checkout ]]; exit
fi
[[ ${!#} == HEAD ]] || exit 93
if [[ ${MOCK_MODE:-} == checkout-drift ]]; then
  printf '2222222222222222222222222222222222222222\n'
else
  printf '1111111111111111111111111111111111111111\n'
fi
MOCK
chmod 0755 "$work/bin/gh" "$work/bin/git"
download() {
  rm -f "$work/count"
  PATH="$work/bin:$PATH" bash "$root/.github/scripts/download-research-release-tools.sh" 1234 "$source_sha" "$work/download-${MOCK_MODE:-ok}"
}
download
MOCK_MODE=manual download
for failure in checkout-drift dirty-checkout missing-artifact expired missing-job wrong-job wrong-source wrong-workflow failed-run malformed-pages rerun job-drift; do
  if MOCK_MODE=$failure download >"$work/rejection" 2>&1; then
    printf 'accepted untrusted tools transport: %s\n' "$failure" >&2; exit 1
  fi
  test ! -e "$work/download-$failure"
done
printf 'PASS: tools authenticated run/attempt/job/artifact and post-download readback failures\n'
