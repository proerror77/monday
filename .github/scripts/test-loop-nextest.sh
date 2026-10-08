#!/usr/bin/env bash
# Contract for the loop nextest archive, four hash shards, and the CI gate.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=loop-nextest-counts.sh
source "$root/.github/scripts/loop-nextest-counts.sh"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail() { printf '%s\n' "$1" >&2; exit 1; }

printf '    Finished test profile [unoptimized] target(s) in 1.00s\n' >"$work/finished.txt"
if loop_nextest_stderr_recompiled "$work/finished.txt"; then
  fail 'a finished line looked like a compile'
fi
printf '   Compiling alpha-harness v0.1.0\n' >"$work/compile.txt"
loop_nextest_stderr_recompiled "$work/compile.txt" || fail 'plain compile line was ignored'
printf '\033[1mCompiling alpha-harness v0.1.0\033[0m\n' >"$work/ansi.txt"
loop_nextest_stderr_recompiled "$work/ansi.txt" || fail 'colored compile line was ignored'

cat >"$work/events.jsonl" <<'EOF'
{"type":"suite","event":"started","test_count":1,"nextest":{"crate":"alpha-harness","test_binary":"alpha_harness","kind":"lib"}}
{"type":"test","event":"started","name":"alpha-harness::alpha_harness$one"}
{"type":"test","event":"ok","name":"alpha-harness::alpha_harness$one","exec_time":0.1}
{"type":"test","event":"started","name":"alpha-engine::llm_live$real_llm_call_writes_hypothesis_artifact"}
{"type":"suite","event":"ok","passed":1,"failed":0,"ignored":1,"measured":0,"filtered_out":0,"exec_time":0.1,"nextest":{"crate":"alpha-harness","test_binary":"alpha_harness","kind":"lib"}}
EOF
cat >"$work/list.json" <<'EOF'
{"rust-suites":{"alpha-harness::alpha_harness (lib)":{"package-name":"alpha-harness","binary-name":"alpha_harness","kind":"lib","status":"listed","testcases":{"one":{"kind":"test","ignored":false,"filter-match":{"status":"matches"}},"skip":{"kind":"test","ignored":true,"filter-match":{"status":"matches"}}}}}}
EOF
loop_nextest_write_shard "$work/events.jsonl" "$work/list.json" "$work/shard.json" 1 4 0 false >/dev/null
[[ $(jq -r '.passed' "$work/shard.json") == 1 ]] || fail 'ignored start was counted as a pass'
jq '.["rust-suites"]["alpha-harness::alpha_harness (lib)"].testcases.extra = {"kind":"test","ignored":false,"filter-match":{"status":"matches"}}' \
  "$work/list.json" >"$work/list-extra.json"
if loop_nextest_write_shard "$work/events.jsonl" "$work/list-extra.json" "$work/bad.json" 1 4 0 false >/dev/null; then
  fail 'a shard matched a different list'
fi

shard_json() {
  local shard=$1 passed=$2
  jq -n --argjson shard "$shard" --argjson passed "$passed" \
    '{shard:$shard,partition:("balanced-v1:" + ($shard|tostring) + "/4"),seconds:($shard*10),exit_status:0,recompiled:false,passed:$passed,failed:0,binaries:{"alpha-harness::alpha_harness (lib)":$passed},failed_tests:[]}'
}
mkdir -p "$work/ok"
jq -n '{build_seconds:3,nextest_runnable:5,alpha_harness:5,binaries:{"alpha-harness::alpha_harness (lib)":5},doctests_listed:0,doctests_passed:0,partition:"balanced-v1",mode:"balanced-v1",shards:4}' \
  >"$work/ok/expected-counts.json"
passed_for=(0 2 1 1 1)
for shard_n in 1 2 3 4; do
  shard_json "$shard_n" "${passed_for[$shard_n]}" >"$work/ok/shard-$shard_n.json"
done
# Five exact identities, assigned once. Keep counts as a separate invariant.
jq '.tests=[["alpha", "a"], ["alpha", "b"], ["alpha", "c"], ["alpha", "d"], ["alpha", "e"]]
  | .shard_tests=[.tests[0:2], .tests[2:3], .tests[3:4], .tests[4:5]]' "$work/ok/expected-counts.json" >"$work/expected.tmp"
mv "$work/expected.tmp" "$work/ok/expected-counts.json"
for shard_n in 1 2 3 4; do
  jq --argjson index "$((shard_n - 1))" --slurpfile expected "$work/ok/expected-counts.json" \
    '.tests=$expected[0].shard_tests[$index]' "$work/ok/shard-$shard_n.json" >"$work/shard.tmp"
  mv "$work/shard.tmp" "$work/ok/shard-$shard_n.json"
done
summary=$(loop_nextest_verify_dir "$work/ok")
grep -Fqx 'LOOP_NEXTEST_RUNNABLE=5' <<<"$summary" || fail 'verify did not report the archive total'
grep -Fqx 'LOOP_NEXTEST_ALPHA_HARNESS=5' <<<"$summary" || fail 'verify did not report alpha_harness'
grep -Fqx 'LOOP_NEXTEST_SHARD_PASSED=2,1,1,1' <<<"$summary" || fail 'verify did not report shard passes'

# A duplicate can preserve every per-binary count while dropping another test.
cp -R "$work/ok" "$work/duplicate"
jq '.tests=[["alpha", "c"]]' "$work/ok/shard-4.json" >"$work/duplicate/shard-4.json"
if loop_nextest_verify_dir "$work/duplicate" >/dev/null; then
  fail 'same-count duplicate and omitted identity was accepted'
fi
# shellcheck disable=SC2016 # The dollar sign is part of nextest's event name.
sed 's/alpha_harness$one/alpha_harness$replacement/' "$work/events.jsonl" >"$work/wrong-events.jsonl"
if loop_nextest_write_shard "$work/wrong-events.jsonl" "$work/list.json" "$work/bad.json" 1 4 0 false >/dev/null; then
  fail 'same-count wrong test identity was accepted'
fi

cp -R "$work/ok" "$work/recompiled"
jq '.recompiled = true' "$work/ok/shard-2.json" >"$work/recompiled/shard-2.json"
if loop_nextest_verify_dir "$work/recompiled" >/dev/null; then
  fail 'a recompiled shard was accepted'
fi
cp -R "$work/ok" "$work/mismatch"
jq '.binaries["alpha-harness::alpha_harness (lib)"] = 9 | .passed = 9' "$work/ok/shard-4.json" >"$work/mismatch/shard-4.json"
if loop_nextest_verify_dir "$work/mismatch" >/dev/null; then
  fail 'shard totals were allowed to disagree with the archive list'
fi
cp -R "$work/ok" "$work/doctest"
jq '.doctests_listed = 1' "$work/ok/expected-counts.json" >"$work/doctest/expected-counts.json"
if loop_nextest_verify_dir "$work/doctest" >/dev/null; then
  fail 'a doctest gap was accepted'
fi

run_gate() {
  local expect=$1
  shift
  local actual=0
  env "$@" bash "$root/.github/scripts/loop-nextest-gate.sh" >/dev/null 2>&1 || actual=$?
  [[ $actual -eq $expect ]] || fail "gate $(printf '%q ' "$@") exited $actual, expected $expect"
}
run_gate 0 LOOP=true SELECTED_JOBS=',ci/rust,' ARCHIVE_RESULT=success SHARD_RESULT=success
run_gate 1 LOOP=true SELECTED_JOBS=',ci/rust,' ARCHIVE_RESULT=failure SHARD_RESULT=success
run_gate 1 LOOP=true SELECTED_JOBS=',ci/rust,' ARCHIVE_RESULT=success SHARD_RESULT=skipped
run_gate 1 LOOP=true SELECTED_JOBS=',ci/rust,' ARCHIVE_RESULT=skipped SHARD_RESULT=skipped
run_gate 0 LOOP=false SELECTED_JOBS=',ci/rust,' ARCHIVE_RESULT=skipped SHARD_RESULT=skipped
run_gate 1 LOOP=false SELECTED_JOBS=',ci/rust,' ARCHIVE_RESULT=failure SHARD_RESULT=skipped
run_gate 0 LOOP=true SELECTED_JOBS=',ci/ci-contracts,' ARCHIVE_RESULT=skipped SHARD_RESULT=skipped
run_gate 1 LOOP=true SELECTED_JOBS=',ci/rust,' ARCHIVE_RESULT=missing SHARD_RESULT=missing

jobs_success=$(jq -n '{total_count:5,jobs:[
  {run_attempt:1,name:"Loop nextest archive",status:"completed",conclusion:"success"},
  {run_attempt:1,name:"Loop nextest shard (1)",status:"completed",conclusion:"success"},
  {run_attempt:1,name:"Loop nextest shard (2)",status:"completed",conclusion:"success"},
  {run_attempt:1,name:"Loop nextest shard (3)",status:"completed",conclusion:"success"},
  {run_attempt:1,name:"Loop nextest shard (4)",status:"completed",conclusion:"success"}
]}')

prepare_zips() {
  local dest=$1 shard_n
  rm -rf "$dest"
  mkdir -p "$dest/expected" "$dest/bin"
  cp "$work/ok/expected-counts.json" "$dest/expected/expected-counts.json"
  (cd "$dest/expected" && zip -q "$dest/11.zip" expected-counts.json)
  for shard_n in 1 2 3 4; do
    mkdir -p "$dest/shard-$shard_n"
    cp "$work/ok/shard-$shard_n.json" "$dest/shard-$shard_n/shard-$shard_n.json"
    (cd "$dest/shard-$shard_n" && zip -q "$dest/2$shard_n.zip" "shard-$shard_n.json")
  done
  jq -n '{total_count:5,artifacts:[
    {id:11,name:"loop-nextest-expected-99-1",expired:false},
    {id:21,name:"loop-nextest-shard-1-99-1",expired:false},
    {id:22,name:"loop-nextest-shard-2-99-1",expired:false},
    {id:23,name:"loop-nextest-shard-3-99-1",expired:false},
    {id:24,name:"loop-nextest-shard-4-99-1",expired:false}
  ]}' >"$dest/artifacts.json"
  cat >"$dest/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=
for arg in "$@"; do url=$arg; done
case $url in
  *"/jobs?filter=all&per_page=100") jq -s . "$GH_FIXTURE/jobs.json" ;;
  *"/artifacts?per_page=100") jq -s . "$GH_FIXTURE/artifacts.json" ;;
  *"/artifacts/"*"/zip")
    id=${url#*"/artifacts/"}
    id=${id%"/zip"}
    cat "$GH_FIXTURE/${id}.zip" ;;
  *) printf 'unexpected gh api %s\n' "$url" >&2; exit 1 ;;
esac
EOF
  chmod +x "$dest/bin/gh"
}
prepare_zips "$work/wait"
printf '%s\n' "$jobs_success" >"$work/wait/jobs.json"
wait_out=$(
  PATH="$work/wait/bin:$PATH" \
  GH_FIXTURE="$work/wait" \
  GITHUB_WORKSPACE="$root" \
  GITHUB_RUN_ID=99 \
  GITHUB_RUN_ATTEMPT=1 \
  GITHUB_REPOSITORY=proerror77/monday \
  bash "$root/.github/scripts/loop-nextest-wait.sh"
)
grep -Fqx 'LOOP_NEXTEST_RUNNABLE=5' <<<"$wait_out" || fail 'waiter did not check the shard sum'
grep -Fqx 'LOOP_NEXTEST_ALPHA_HARNESS=5' <<<"$wait_out" || fail 'waiter did not report alpha_harness'

# Partial retry: archive and three shards remain successful on attempt 1,
# while shard 1 is replaced by its successful attempt 2 evidence.
jq '.jobs += [.jobs[1] | .run_attempt=2] | .total_count=6' <<<"$jobs_success" >"$work/wait/jobs.json"
jq '(.artifacts[]|select(.id==21)|.name)="loop-nextest-shard-1-99-2"' "$work/wait/artifacts.json" >"$work/retry-artifacts"
mv "$work/retry-artifacts" "$work/wait/artifacts.json"
PATH="$work/wait/bin:$PATH" GH_FIXTURE="$work/wait" GITHUB_WORKSPACE="$root" \
  GITHUB_RUN_ID=99 GITHUB_RUN_ATTEMPT=2 GITHUB_REPOSITORY=proerror77/monday \
  bash "$root/.github/scripts/loop-nextest-wait.sh" >/dev/null
jq '.jobs[-1].conclusion="failure"' "$work/wait/jobs.json" >"$work/failed-retry"
mv "$work/failed-retry" "$work/wait/jobs.json"
if PATH="$work/wait/bin:$PATH" GH_FIXTURE="$work/wait" GITHUB_WORKSPACE="$root" \
  GITHUB_RUN_ID=99 GITHUB_RUN_ATTEMPT=2 GITHUB_REPOSITORY=proerror77/monday \
  bash "$root/.github/scripts/loop-nextest-wait.sh" >/dev/null 2>&1; then
  fail 'latest failed retry fell back to the earlier successful shard'
fi
prepare_zips "$work/wait"
printf '%s\n' "$jobs_success" | jq '.jobs[1].conclusion = "failure"' >"$work/wait/jobs.json"
if PATH="$work/wait/bin:$PATH" GH_FIXTURE="$work/wait" GITHUB_WORKSPACE="$root" \
  GITHUB_RUN_ID=99 GITHUB_RUN_ATTEMPT=1 GITHUB_REPOSITORY=proerror77/monday \
  bash "$root/.github/scripts/loop-nextest-wait.sh" >"$work/wait/failed.out" 2>"$work/wait/failed.err"; then
  fail 'waiter accepted a failed shard'
fi
if grep -q 'LOOP_NEXTEST_RUNNABLE=' "$work/wait/failed.out"; then
  fail 'waiter reported a total after a failed shard'
fi

jq -n '{total_count:2,jobs:[
  {run_attempt:1,name:"Loop nextest archive",status:"completed",conclusion:"success"},
  {run_attempt:1,name:"Loop nextest archive",status:"completed",conclusion:"success"}
]}' >"$work/wait/jobs.json"
if PATH="$work/wait/bin:$PATH" GH_FIXTURE="$work/wait" GITHUB_WORKSPACE="$root" \
  GITHUB_RUN_ID=99 GITHUB_RUN_ATTEMPT=1 GITHUB_REPOSITORY=proerror77/monday \
  bash "$root/.github/scripts/loop-nextest-wait.sh" >/dev/null 2>&1; then
  fail 'waiter accepted a duplicate job name'
fi

jq -n '{total_count:101,jobs:[{run_attempt:1,name:"Loop nextest archive",status:"queued",conclusion:null}]}' >"$work/wait/jobs.json"
if PATH="$work/wait/bin:$PATH" GH_FIXTURE="$work/wait" GITHUB_WORKSPACE="$root" \
  GITHUB_RUN_ID=99 GITHUB_RUN_ATTEMPT=1 GITHUB_REPOSITORY=proerror77/monday \
  bash "$root/.github/scripts/loop-nextest-wait.sh" >/dev/null 2>&1; then
  fail 'waiter accepted an incomplete jobs page'
fi

printf '%s\n' "$jobs_success" >"$work/wait/jobs.json"
jq -n '{total_count:0,artifacts:[]}' >"$work/wait/artifacts.json"
if PATH="$work/wait/bin:$PATH" GH_FIXTURE="$work/wait" GITHUB_WORKSPACE="$root" \
  GITHUB_RUN_ID=99 GITHUB_RUN_ATTEMPT=1 GITHUB_REPOSITORY=proerror77/monday \
  LOOP_NEXTEST_ARTIFACT_TRIES=2 LOOP_NEXTEST_ARTIFACT_WAIT_SECONDS=0 \
  bash "$root/.github/scripts/loop-nextest-wait.sh" >/dev/null 2>&1; then
  fail 'waiter accepted missing artifacts'
fi

printf '%s\n' .github/scripts/test-loop-nextest.sh >"$work/paths"
bash "$root/.github/scripts/select-rust-ci-scope.sh" --event pull_request \
  --changed-files "$work/paths" \
  --metadata "$root/.github/scripts/fixtures/rust-ci-scope/metadata.fixture" \
  --output "$work/scope"
grep -q ',ci/ci-contracts,' "$work/scope" || fail 'script edit did not select ci contracts'
grep -Fqx 'loop=false' "$work/scope" || fail 'script edit selected loop tests'
if grep -q ',ci/rust,' "$work/scope"; then
  fail 'script edit selected the rust workspace'
fi

ruby -ryaml - "$root/.github/workflows/ci.yml" <<'RUBY'
workflow = ARGV.fetch(0)
text = File.read(workflow)
token = ['py', 'thon3'].join
abort 'workflow contains a tracked command' if text.match?(/#{Regexp.escape(token)}[[:space:]]/)
jobs = YAML.safe_load(text).fetch('jobs')
rust = jobs.fetch('rust')
abort 'rust job must keep depending only on scope' unless rust.fetch('needs') == 'scope'
steps = rust.fetch('steps')
ids = steps.map { |step| step['id'] }.compact
%w[collector handoff focused clippy_loop clippy_handoff].each do |id|
  abort "loop step is not after #{id}" unless ids.index('loop') && ids.index(id) && ids.index('loop') > ids.index(id)
end
abort 'loop step is not before evidence' unless ids.index('loop') < ids.index('rust_evidence')
loop = steps.find { |step| step['id'] == 'loop' }
abort 'loop step still runs cargo test' if loop.fetch('run').include?('cargo-scoped')
abort 'loop step does not wait for shards' unless loop.fetch('run').include?('loop-nextest-wait.sh')
archive = jobs.fetch('rust_loop_nextest_archive')
abort 'archive must depend only on scope' unless archive.fetch('needs') == 'scope'
cache = archive.fetch('steps').find { |step| step['uses'].to_s.include?('Swatinem/rust-cache@') }
abort 'archive cache key drifted' unless cache && cache.dig('with', 'key') == 'rust_hft-ci-rust-${{ steps.cache-info.outputs.rust }}'
abort 'archive cache save drifted' unless cache.dig('with', 'save-if') == "${{ github.ref == 'refs/heads/main' }}"
shard = jobs.fetch('rust_loop_nextest_shard')
abort 'shard must depend only on the archive' unless shard.fetch('needs') == 'rust_loop_nextest_archive'
abort 'shard fail-fast must stay off' unless shard.dig('strategy', 'fail-fast') == false
abort 'shard matrix drifted' unless shard.dig('strategy', 'matrix', 'shard') == [1, 2, 3, 4]
abort 'shard restored a target cache' if shard.fetch('steps').any? { |step| step['uses'].to_s.include?('rust-cache') }
needs = jobs.fetch('ci-gate').fetch('needs')
%w[rust rust_loop_nextest_archive rust_loop_nextest_shard].each do |id|
  abort "#{id} missing from the monorepo gate" unless needs.include?(id)
end
gate = jobs.fetch('ci-gate').fetch('steps').map { |step| step['run'].to_s }.join("\n")
abort 'gate dropped the shared verifier' unless gate.include?('verify-ci-gate.sh')
abort 'gate dropped the loop shard check' unless gate.include?('loop-nextest-gate.sh')
RUBY

# Exercise the planner with outliers, ignored tests, and duplicate membership.
mkdir -p "$work/plan"
ruby -rjson - "$work/plan" <<'RUBY'
work = ARGV.fetch(0)
heavy = %w[
  mission_campaign::tests::execute_retains_paired_mlp_training_diagnostics
  mission_campaign::tests::execute_native_prepared_development_retains_exact_round_readbacks
  representation_plan::tests::review_production_tool_closure_routes_readers_and_model_contracts
]
names = heavy + %w[a b c d]
write = lambda do |path, selected|
  cases = (names + ['ignored']).to_h do |name|
    [name, {'ignored' => name == 'ignored', 'filter-match' => {'status' => selected.include?(name) ? 'matches' : 'mismatch'}}]
  end
  File.write(path, JSON.generate({'rust-suites' => {'alpha-harness' => {'status' => 'listed', 'testcases' => cases}}}))
end
write.call("#{work}/nextest-list.json", names + ['ignored'])
[['a'], heavy + ['b'], ['c'], ['d']].each_with_index { |set, i| write.call("#{work}/hash-#{i+1}.json", set) }
File.write("#{work}/expected-counts.json", '{}')
RUBY
ruby "$root/.github/scripts/loop-nextest-plan.rb" "$work/plan"
jq -e '.partition == "balanced-v1" and (.tests|length) == 7
  and (.shard_tests|map(length)) == [2,1,2,2]
  and ([.shard_tests[][]]|sort) == (.tests|sort)' "$work/plan/expected-counts.json" >/dev/null
LOOP_PARTITION_MODE="hash" ruby "$root/.github/scripts/loop-nextest-plan.rb" "$work/plan"
jq -e '.partition=="hash" and .mode=="hash" and (.shard_tests|map(length))==[1,4,1,1]' "$work/plan/expected-counts.json" >/dev/null
# A corrupted partition retains its count but repeats another shard's test.
jq '."rust-suites"["alpha-harness"].testcases.d["filter-match"].status="mismatch"
  | ."rust-suites"["alpha-harness"].testcases.c["filter-match"].status="matches"' \
  "$work/plan/hash-4.json" >"$work/bad-hash.json"
mv "$work/bad-hash.json" "$work/plan/hash-4.json"
if ruby "$root/.github/scripts/loop-nextest-plan.rb" "$work/plan" >/dev/null 2>&1; then
  fail 'planner accepted a duplicate identity and missing test'
fi

printf 'loop nextest contract passed\n'
