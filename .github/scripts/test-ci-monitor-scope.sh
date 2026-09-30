#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd -- "$script_dir/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

for path in .github/workflows/monitor-collector-host.yml deployment/aliyun/monday-collector-health.sh .github/workflows/ci.yml .github/scripts/select-rust-ci-scope.sh; do
  printf '%s\n' "$path" >"$work/paths"
  result="$work/result-${path##*/}"
  bash "$script_dir/select-rust-ci-scope.sh" --event pull_request --changed-files "$work/paths" \
    --metadata "$script_dir/fixtures/rust-ci-scope/metadata.fixture" --output "$result"
  for flag in loop collector control toolchain; do grep -Fqx "$flag=false" "$result"; done
  if grep -Eq 'jobs=.*(ci/rust,|ploy/rust-|ploy/frontend|image)' "$result"; then
    printf 'unrelated workload selected for %s\n' "$path" >&2; exit 1
  fi
done

ruby -ryaml - "$repo" "$work" <<'RUBY'
repo, work = ARGV
ci = YAML.safe_load(File.read("#{repo}/.github/workflows/ci.yml")).fetch('jobs')
scope = ci.fetch('scope')
abort 'scope cannot be cancelled' unless scope.fetch('if').include?('!cancelled()')
abort 'scope performs verification instead of planning' if scope.fetch('steps').any? { |s| s['run']&.match?(/test-.*\.sh|shellcheck|cargo /) }
%w[control_contracts monitor_contracts ci_contracts].each do |job|
  abort "#{job} missing from aggregate gate" unless ci.fetch('ci-gate').fetch('needs').include?(job)
  abort "#{job} cannot be cancelled" unless ci.fetch(job).fetch('if').include?('!cancelled()')
end
{
  'ci.yml'=>'scope', 'ploy-ci.yml'=>'image-smoke-scope', 'security-enabled.yml'=>'security-scope'
}.each do |file, job|
  jobs = YAML.safe_load(File.read("#{repo}/.github/workflows/#{file}")).fetch('jobs')
  run = jobs.fetch(job).fetch('steps').find { |s| s['id']=='scope' }.fetch('run')
  File.write("#{work}/#{file}.sh", run)
end
RUBY

for file in ci.yml ploy-ci.yml security-enabled.yml; do
  for result in success failure; do
    output="$work/$file-$result.out"
    code=0
    SELECTOR_RESULT=$result SELECTED_COMPLETE=true SELECTED_JOBS=',ci/monitor-contracts,' \
      SELECTED_SECURITY_JOBS=',security/secret-presence,' SELECTED_OWNING_PACKAGES=',,' \
      SELECTED_LOOP=false SELECTED_HANDOFF=false SELECTED_JSON=false SELECTED_ONDO=false \
      SELECTED_COLLECTOR=false SELECTED_CONTROL=false SELECTED_FOCUSED=false SELECTED_TOOLCHAIN=false \
      CLIPPY_LOOP=false CLIPPY_HANDOFF=false GITHUB_OUTPUT="$output" \
      bash -e "$work/$file.sh" >"$work/stdout" 2>"$work/stderr" || code=$?
    if [[ $result == success ]]; then [[ $code == 0 && -s $output ]];
    else [[ $code != 0 && ! -s $output ]]; fi
  done
done

# Selected tasks cannot be acknowledged by skipping, cancellation or absence.
for result in success skipped failure cancelled missing; do
  payload=$(jq -cn --arg result "$result" '{selector:{result:"success"},scope:{result:"success"}}+
    (if $result=="missing" then {} else {monitor_contracts:{result:$result}} end)')
  code=0
  printf '%s' "$payload" | bash "$script_dir/verify-ci-gate.sh" \
    --job-prefix ci --expected-jobs ',ci/monitor-contracts,' >"$work/gate.out" 2>&1 || code=$?
  if [[ $result == success ]]; then [[ $code == 0 ]]; else [[ $code != 0 ]]; fi
done
printf 'Monitoring selection, cancellable jobs, failed plans and mandatory selected-task checks passed\n'
