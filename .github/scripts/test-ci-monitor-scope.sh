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
prediction = YAML.safe_load(File.read("#{repo}/.github/workflows/ploy-ci.yml")).fetch('jobs')
ancestors = lambda do |id|
  Array(prediction.fetch(id)['needs']).flat_map { |parent| [parent] + ancestors.call(parent) }.uniq
end
%w[workflow-lint commit-hygiene rust-format safety-scans audit rust-control-plane rust-runner-lean
   rust-runner-full rust-market-data rust-research-heavy frontend integration-regressions
   strategy-config-contracts architecture-contracts sqlx-cli-equivalence].each do |id|
  abort "#{id} waits for publication history" if ancestors.call(id).include?('research-image-scope')
end
%w[research-image-binaries research-image-smoke prediction-markets-gate].each do |id|
  abort "#{id} bypasses publication recovery" unless ancestors.call(id).include?('research-image-scope')
end
abort 'direct selector consults publication history' if prediction.fetch('image-smoke-selector').to_s.include?('select-main-research-scope')
abort 'gate ignores carried jobs' unless prediction.fetch('prediction-markets-gate').to_s.include?('needs.research-image-scope.outputs.jobs')
diagnostic = "github.event_name == 'workflow_dispatch' && inputs.research_cache_probe"
sqlx_diagnostic = "github.event_name == 'workflow_dispatch' && inputs.sqlx_cli_probe"
abort 'probe can replace the required release gate' unless prediction.fetch('prediction-markets-gate').fetch('name') ==
  "${{ #{sqlx_diagnostic} && 'SQLx CLI diagnostic gate' || #{diagnostic} && 'Research build A/B diagnostic gate' || 'Prediction Markets CI gate' }}"
document = YAML.safe_load(File.read("#{repo}/.github/workflows/ploy-ci.yml"))
abort 'probe can cancel ordinary CI' unless document.fetch('concurrency').fetch('group').include?("#{diagnostic} && 'research-cache-probe-'")
abort 'SQLx probe can cancel ordinary CI' unless document.fetch('concurrency').fetch('group').include?("#{sqlx_diagnostic} && 'sqlx-cli-probe-'")
abort 'SQLx contract missing from aggregate gate' unless prediction.fetch('prediction-markets-gate').fetch('needs').include?('sqlx-cli-equivalence')
File.write("#{work}/diagnostic-plan.sh", prediction.fetch('image-smoke-selector').fetch('steps').find { |step| step['id']=='scope' }.fetch('run'))
{
  'ci.yml'=>'scope', 'ploy-ci.yml'=>'image-smoke-scope', 'security-enabled.yml'=>'security-scope'
}.each do |file, job|
  jobs = YAML.safe_load(File.read("#{repo}/.github/workflows/#{file}")).fetch('jobs')
  run = jobs.fetch(job).fetch('steps').find { |s| s['id']=='scope' }.fetch('run')
  File.write("#{work}/#{file}.sh", run)
end
RUBY

# Execute the real workflow plan. Probe dispatch must never invoke the broad planner.
probe_repo="$work/probe-repo"
mkdir -p "$probe_repo/.github/scripts" "$probe_repo/rust_hft/prediction-markets"
cp "$script_dir/research-release-products.sh" "$script_dir/research-release-products.json" "$probe_repo/.github/scripts/"
cat >"$probe_repo/.github/scripts/select-rust-ci-scope.sh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"${ORDINARY_PLAN_CALLED:?}"
printf 'jobs=,ploy/workflow-lint,\nresearch_product=none\nselection_complete=true\n' >>"$GITHUB_OUTPUT"
STUB
chmod +x "$probe_repo/.github/scripts/select-rust-ci-scope.sh"
for probe in true false '' invalid; do
  for event in workflow_dispatch push pull_request; do
    output="$work/probe-$event-${probe:-absent}.out"
    called="$output.ordinary"
    code=0
    (cd "$probe_repo/rust_hft/prediction-markets" &&
      SQLX_CLI_PROBE=false RESEARCH_CACHE_PROBE=$probe GITHUB_EVENT_NAME=$event GITHUB_OUTPUT="$output" \
      ORDINARY_PLAN_CALLED="$called" BASE_SHA=base HEAD_SHA=head bash "$work/diagnostic-plan.sh") \
      >"$work/probe.stdout" 2>"$work/probe.stderr" || code=$?
    if [[ $probe == true && $event == workflow_dispatch ]]; then
      [[ $code == 0 && ! -e $called ]]
      grep -Fqx 'jobs=,ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke,' "$output"
      grep -Fqx 'research_product=cex-runner,controller,prediction-runner' "$output"
      grep -Fqx selection_complete=true "$output"
    elif [[ $probe == false || -z $probe ]]; then
      [[ $code == 0 ]]
      grep -Fqx -- "--event $event --base base --head head" "$called"
    else
      [[ $code != 0 && ! -e $called && ! -s $output ]]
    fi
  done
done

# SQLx diagnostics use the same fail-closed plan boundary without broad research.
for probe in true false '' invalid; do
  for event in workflow_dispatch push pull_request; do
    output="$work/sqlx-$event-${probe:-absent}.out"
    called="$output.ordinary"
    code=0
    (cd "$probe_repo/rust_hft/prediction-markets" &&
      SQLX_CLI_PROBE=$probe RESEARCH_CACHE_PROBE=false GITHUB_EVENT_NAME=$event GITHUB_OUTPUT="$output" \
      ORDINARY_PLAN_CALLED="$called" BASE_SHA=base HEAD_SHA=head bash "$work/diagnostic-plan.sh") \
      >"$work/probe.stdout" 2>"$work/probe.stderr" || code=$?
    if [[ $probe == true && $event == workflow_dispatch ]]; then
      [[ $code == 0 && ! -e $called ]]
      grep -Fqx 'jobs=,ploy/sqlx-cli-equivalence,' "$output"
      grep -Fqx research_product=none "$output"
      grep -Fqx selection_complete=true "$output"
    elif [[ $probe == false || -z $probe ]]; then
      [[ $code == 0 ]]
      grep -Fqx -- "--event $event --base base --head head" "$called"
    else
      [[ $code != 0 && ! -e $called && ! -s $output ]]
    fi
  done
done
for other_probe in true invalid; do
  if (cd "$probe_repo/rust_hft/prediction-markets" &&
    SQLX_CLI_PROBE=true RESEARCH_CACHE_PROBE=$other_probe GITHUB_EVENT_NAME=workflow_dispatch \
    GITHUB_OUTPUT="$work/conflicting.out" ORDINARY_PLAN_CALLED="$work/conflicting.ordinary" \
    bash "$work/diagnostic-plan.sh"); then exit 1; fi
  [[ ! -s $work/conflicting.out && ! -e $work/conflicting.ordinary ]]
done

# Diagnostic success cannot satisfy the production publication check reader.
for name in 'Research build A/B diagnostic gate' 'SQLx CLI diagnostic gate'; do
  jq -n --arg name "$name" '{check_runs:[{id:1,name:$name,status:"completed",conclusion:"success",app:{slug:"github-actions",id:15368}}]}' >"$work/probe-checks.json"
  bash "$script_dir/read-release-required-checks.sh" "$work/probe-checks.json" "$work/probe-release-state"
  grep -Fqx prediction_conclusion=missing "$work/probe-release-state"
done
sqlx_needs='{"image-smoke-selector":{"result":"success"},"image-smoke-scope":{"result":"success"},"research-image-scope":{"result":"success"},"sqlx-cli-equivalence":{"result":"success"}}'
printf '%s' "$sqlx_needs" | bash "$script_dir/verify-ci-gate.sh" --job-prefix ploy --expected-jobs ',ploy/sqlx-cli-equivalence,'
for result in skipped failure cancelled missing; do
  payload=$(jq -c --arg result "$result" 'if $result=="missing" then del(."sqlx-cli-equivalence") else ."sqlx-cli-equivalence".result=$result end' <<<"$sqlx_needs")
  if printf '%s' "$payload" | bash "$script_dir/verify-ci-gate.sh" --job-prefix ploy --expected-jobs ',ploy/sqlx-cli-equivalence,' >"$work/sqlx-gate.log" 2>&1; then exit 1; fi
done

for file in ci.yml ploy-ci.yml security-enabled.yml; do
  for result in success failure; do
    output="$work/$file-$result.out"
    code=0
    SELECTOR_RESULT=$result SELECTED_COMPLETE=true SELECTED_JOBS=',ci/monitor-contracts,' \
      SELECTED_SECURITY_JOBS=',security/secret-presence,' SELECTED_OWNING_PACKAGES=',,' SELECTED_LOOP_PACKAGES=',,' SELECTED_FOCUSED_PACKAGES=',,' \
      SELECTED_CLIPPY_LOOP=false SELECTED_CLIPPY_HANDOFF=false \
      SELECTED_LOOP=false SELECTED_HANDOFF=false SELECTED_JSON=false SELECTED_ONDO=false \
      SELECTED_COLLECTOR=false SELECTED_CONTROL=false SELECTED_FOCUSED=false SELECTED_TOOLCHAIN=false \
      SELECTED_PRODUCTION_TRADING_IMAGE=false SELECTED_PRODUCTION_COLLECTOR_IMAGE=false \
      CLIPPY_LOOP=false CLIPPY_HANDOFF=false GITHUB_OUTPUT="$output" \
      bash -e "$work/$file.sh" >"$work/stdout" 2>"$work/stderr" || code=$?
    if [[ $result == success ]]; then [[ $code == 0 && -s $output ]];
    else [[ $code != 0 && ! -s $output ]]; fi
  done
done

# A complete diagnostic plan still requires every selected job and planner.
probe_jobs=',ploy/workflow-lint,ploy/research-image-binaries,ploy/research-image-smoke,'
for complete in false ''; do
  output="$work/incomplete-${complete:-missing}.out"
  if SELECTOR_RESULT=success SELECTED_COMPLETE=$complete SELECTED_JOBS="$probe_jobs" GITHUB_OUTPUT="$output" \
      bash -e "$work/ploy-ci.yml.sh" >"$work/incomplete.log" 2>&1; then
    echo 'incomplete diagnostic plan was accepted' >&2; exit 1
  fi
  [[ ! -s $output ]]
done
probe_needs='{"image-smoke-selector":{"result":"success"},"image-smoke-scope":{"result":"success"},"research-image-scope":{"result":"success"},"workflow-lint":{"result":"success"},"research-image-binaries":{"result":"success"},"research-image-smoke":{"result":"success"}}'
printf '%s' "$probe_needs" | bash "$script_dir/verify-ci-gate.sh" --job-prefix ploy --expected-jobs "$probe_jobs"
for job in image-smoke-selector image-smoke-scope research-image-scope workflow-lint research-image-binaries research-image-smoke; do
  for result in skipped failure cancelled missing; do
    payload=$(jq -c --arg job "$job" --arg result "$result" \
      'if $result=="missing" then del(.[$job]) else .[$job].result=$result end' <<<"$probe_needs")
    # Planner absence is represented as a failed dependency by Actions. Test
    # selected job absence separately through the expected plan.
    if [[ $result == missing && $job == *scope || $result == missing && $job == *selector ]]; then continue; fi
    if printf '%s' "$payload" | bash "$script_dir/verify-ci-gate.sh" --job-prefix ploy --expected-jobs "$probe_jobs" >"$work/probe-gate.out" 2>&1; then
      echo "diagnostic gate accepted $job=$result" >&2; exit 1
    fi
  done
done

# Recovery failure blocks the aggregate even when independent lint succeeded.
for result in success skipped failure cancelled; do
  payload=$(jq -cn --arg result "$result" '{"image-smoke-selector":{result:"success"},
    "image-smoke-scope":{result:"success"},"research-image-scope":{result:$result},
    "workflow-lint":{result:"success"}}')
  code=0
  printf '%s' "$payload" | bash "$script_dir/verify-ci-gate.sh" \
    --job-prefix ploy --expected-jobs ',ploy/workflow-lint,' >"$work/recovery-gate.out" 2>&1 || code=$?
  if [[ $result == success ]]; then [[ $code == 0 ]]; else [[ $code != 0 ]]; fi
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
