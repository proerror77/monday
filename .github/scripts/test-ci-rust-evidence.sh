#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
source "$root/.github/scripts/verify-ci-rust-evidence.sh"
cat >"$work/scope" <<'SCOPE'
loop=true
handoff=false
json=false
ondo=false
collector=false
control=false
focused=false
toolchain=true
clippy_loop=true
clippy_handoff=false
owning_packages=,,
loop_packages=,alpha-harness,
focused_packages=,,
SCOPE
jq -n --argjson scope "$(ci_expected_scope "$work/scope")" '{schema:"monday.ci_rust_evidence.v1",scope:$scope,stages:[{stage:"loop",outcome:"success"},{stage:"clippy_loop",outcome:"success"}],checkout_sha:("c"*40),command_sha256:("d"*64),repository:"proerror77/monday",source_repository:"proerror77/monday",event:"pull_request",head_sha:("a"*40),base_sha:("b"*40),run_id:"123",run_attempt:3,job_id:789}' >"$work/receipt"
jq -n '{pull_request:{number:42,head:{sha:("a"*40),repo:{full_name:"proerror77/monday"}},base:{sha:("b"*40)}}}' >"$work/event"
jq -n '{id:123,run_attempt:3,head_sha:("a"*40),event:"pull_request",repository:{full_name:"proerror77/monday"},head_repository:{full_name:"proerror77/monday"},path:".github/workflows/ci.yml",status:"in_progress",pull_requests:[{number:42,head:{sha:("a"*40)},base:{sha:("b"*40)}}]}' >"$work/run"
jq -n '{jobs:[{id:789,run_id:123,run_attempt:3,head_sha:("a"*40),name:"Rust Workspace",status:"completed",conclusion:"success"}]}' >"$work/jobs"
checkout=$(printf c%.0s {1..40})
ci_verify_stages "$work/receipt" "$work/scope"
ci_verify_producer "$work/receipt" "$work/run" "$work/jobs" "$work/event" "$checkout"
for mutation in '.stages|=map(select(.stage!="clippy_loop"))' '.stages[0].outcome="failure"' '.stages+=[.stages[0]]' '.scope.loop_packages=",alpha-domain,"' '.checkout_sha=("d"*40)' '.source_repository="fork/monday"' '.run_attempt=4' '.job_id=0'; do
  jq "$mutation" "$work/receipt" >"$work/bad"
  if ci_verify_stages "$work/bad" "$work/scope" && ci_verify_producer "$work/bad" "$work/run" "$work/jobs" "$work/event" "$checkout"; then echo "invalid evidence accepted: $mutation" >&2;exit 1;fi
done
for mutation in '.jobs[0].name="unrelated check"' '.jobs[0].conclusion="failure"' '.jobs[0].status="in_progress"' '.jobs[0].id=790' '.jobs[0].run_attempt=4'; do
  jq "$mutation" "$work/jobs" >"$work/bad-jobs"
  if ci_verify_producer "$work/receipt" "$work/run" "$work/bad-jobs" "$work/event" "$checkout";then exit 1;fi
done
# Fork validation binds the actual fork, while publication remains main-only.
jq '.source_repository="fork/monday"' "$work/receipt" >"$work/fork-receipt"
jq '.head_repository.full_name="fork/monday"' "$work/run" >"$work/fork-run"
jq '.pull_request.head.repo.full_name="fork/monday"' "$work/event" >"$work/fork-event"
ci_verify_producer "$work/fork-receipt" "$work/fork-run" "$work/jobs" "$work/fork-event" "$checkout"

# Exercise the actual authenticated-artifact consumer, including an attempt
# changing between download and final API readback. No network or secret used.
mkdir -p "$work/repo/.github/scripts" "$work/repo/.github/workflows" "$work/bin"
cp "$root/.github/scripts/"{wait-ci-rust-evidence,verify-ci-rust-evidence,write-ci-rust-evidence}.sh "$work/repo/.github/scripts/"
cp "$root/.github/workflows/ci.yml" "$work/repo/.github/workflows/"
command_sha=$(cat "$work/repo/.github/workflows/ci.yml" "$work/repo/.github/scripts/write-ci-rust-evidence.sh" "$work/repo/.github/scripts/verify-ci-rust-evidence.sh" | sha256sum | awk '{print $1}')
jq --arg sha "$command_sha" '.command_sha256=$sha' "$work/receipt" >"$work/rust-batch.json"
(cd "$work" && zip -jq evidence.zip rust-batch.json)
cat >"$work/repo/.github/scripts/select-rust-ci-scope.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
while (($#));do if [[ $1 == --output ]];then cp "$TEST_ROOT/scope" "$2";exit;fi;shift;done
exit 1
MOCK
cat >"$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
 *workflows/ci.yml/runs*)
   count=0;[[ ! -f $TEST_ROOT/discoveries ]] || count=$(cat "$TEST_ROOT/discoveries")
   echo $((count+1)) >"$TEST_ROOT/discoveries"
   if [[ ${STALE_FIRST:-false} == true && $count == 0 ]];then jq '.pull_requests[0].base.sha=("d"*40)|{workflow_runs:[.]}' "$TEST_ROOT/run";else jq '{workflow_runs:[.]}' "$TEST_ROOT/run";fi ;;
 *attempts/3/jobs*) if [[ ${STALE_LIST:-false} == true && -f $TEST_ROOT/downloaded ]];then jq '.jobs[0].status="in_progress"' "$TEST_ROOT/jobs";else cat "$TEST_ROOT/jobs";fi ;;
 *actions/jobs/789*) jq '.jobs[0]' "$TEST_ROOT/jobs" ;;
 *actions/runs/123/artifacts*) jq -n '{artifacts:[{id:555,name:("ci-rust-evidence-"+("c"*40)+"-3"),expired:false,workflow_run:{head_sha:("a"*40)}}]}' ;;
 *actions/artifacts/555/zip*) cat "$TEST_ROOT/evidence.zip";touch "$TEST_ROOT/downloaded" ;;
 *actions/runs/123*) if [[ ${RERUN:-false} == true && -f $TEST_ROOT/downloaded ]];then jq '.run_attempt=4' "$TEST_ROOT/run";else cat "$TEST_ROOT/run";fi ;;
 *pulls/42*) jq '.pull_request|{state:"open",head:.head,base:.base}' "$TEST_ROOT/event" ;;
 *) echo 'unexpected API' >&2;exit 1 ;;
esac
MOCK
printf '#!/usr/bin/env bash\nexit 0\n' >"$work/bin/sleep"
chmod +x "$work/bin/gh" "$work/bin/sleep"
export TEST_ROOT="$work" GITHUB_REPOSITORY=proerror77/monday GITHUB_EVENT_NAME=pull_request GITHUB_EVENT_PATH="$work/event"
export PATH="$work/bin:$PATH"
(cd "$work/repo" && bash .github/scripts/wait-ci-rust-evidence.sh "$checkout")
rm "$work/downloaded" "$work/discoveries"
if (cd "$work/repo" && RERUN=true bash .github/scripts/wait-ci-rust-evidence.sh "$checkout");then echo 'stale attempt consumed' >&2;exit 1;fi
rm "$work/downloaded" "$work/discoveries"
(cd "$work/repo" && STALE_FIRST=true bash .github/scripts/wait-ci-rust-evidence.sh "$checkout")
[[ $(cat "$work/discoveries") == 2 ]]
rm "$work/downloaded" "$work/discoveries"
(cd "$work/repo" && STALE_LIST=true bash .github/scripts/wait-ci-rust-evidence.sh "$checkout")
# Readback is still fail-closed for the actual producer, even if discovery passed.
rm "$work/downloaded" "$work/discoveries"
cp "$work/jobs" "$work/good-jobs"
jq '.jobs[0].run_attempt=4' "$work/good-jobs" >"$work/jobs"
if (cd "$work/repo" && bash .github/scripts/wait-ci-rust-evidence.sh "$checkout") >"$work/attempt-error" 2>&1;then echo 'cross-attempt job admitted' >&2;exit 1;fi
grep -Fq 'CI Rust evidence failed at exact-job-readback' "$work/attempt-error"
mv "$work/good-jobs" "$work/jobs"
printf 'PASS: selected stages, exact checkout/source/fork/run/attempt/job and shared consumer recovery\n'
