#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
source "$root/.github/scripts/verify-ack-rust-batch.sh"
cat >"$scratch/scope" <<'SCOPE'
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
SCOPE
jq -n --arg key "$(printf a%.0s {1..64})" '{profile:"ci-rust",terminal_result:"success",public_job_id:789,
  validation:{schema_version:"monday.ack_rust_batch.v1",
    scope:{loop:true,handoff:false,json:false,ondo:false,collector:false,control:false,focused:false,
      toolchain:true,clippy_loop:true,clippy_handoff:false,owning_packages:",,",loop_packages:",alpha-harness,"},
    stages:["loop","clippy_loop"]|map({stage:.,input_sha256:$key,private_run_id:"901",exit_code:0,completed_epoch:123,reused:false})}}' >"$scratch/receipt"
ack_verify_rust_batch "$scratch/receipt" "$scratch/scope"
for mutation in \
  '.validation.stages|=map(select(.stage!="clippy_loop"))' \
  '.validation.stages[1].exit_code=1' \
  '.validation.stages+=[.validation.stages[1]]' \
  '.validation.scope.loop_packages=",alpha-domain,"' \
  '.terminal_result="failure"' \
  'del(.public_job_id)' \
  '.public_job_id="789"' \
  '.public_job_id=0' \
  'del(.validation)'; do
  jq "$mutation" "$scratch/receipt" >"$scratch/mutated"
  if ack_verify_rust_batch "$scratch/mutated" "$scratch/scope"; then
    echo "invalid batch accepted: $mutation" >&2; exit 1
  fi
done

# Exercise cross-workflow producer selection and readback without network or
# signing credentials. The shared reader is stubbed after its own verification.
mkdir -p "$scratch/repo/.github/scripts" "$scratch/bin"
cp "$root/.github/scripts/wait-ack-rust-batch.sh" "$scratch/repo/.github/scripts/"
cp "$root/.github/scripts/verify-ack-rust-batch.sh" "$scratch/repo/.github/scripts/"
export GITHUB_REPOSITORY=proerror77/monday GITHUB_EVENT_NAME=pull_request
export GITHUB_RUN_ID=456 GITHUB_RUN_ATTEMPT=2 GITHUB_JOB=clippy-strict
export GITHUB_EVENT_PATH="$scratch/event" RUNNER_TEMP="$scratch" FIXTURE_ROOT="$scratch"
jq -n '{pull_request:{number:42,head:{sha:("a"*40),repo:{full_name:"proerror77/monday"}},base:{sha:("b"*40)}}}' >"$GITHUB_EVENT_PATH"
cat >"$scratch/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *workflows/ci.yml/runs*)
    count=0
    [[ ! -f $FIXTURE_ROOT/discoveries ]] || count=$(cat "$FIXTURE_ROOT/discoveries")
    count=$((count+1)); echo "$count" >"$FIXTURE_ROOT/discoveries"
    candidate_base=b
    [[ ${STALE_FIRST:-false} != true || $count -gt 1 ]] || candidate_base=d
    jq -n --arg base "$candidate_base" '{workflow_runs:[{id:123,head_sha:("a"*40),head_repository:{full_name:"proerror77/monday"},event:"pull_request",path:".github/workflows/ci.yml",pull_requests:[{number:42,head:{sha:("a"*40)},base:{sha:($base*40)}}]}]}' ;;
  *attempts/3/jobs*)
    jq -n --arg name "${BAD_JOB:-Rust Workspace}" '{jobs:[{id:789,run_id:123,head_sha:("a"*40),name:$name,status:"completed",conclusion:"success"}]}' ;;
  *actions/runs/123*)
    attempt=3
    [[ ! -e $FIXTURE_ROOT/read || ${RERUN:-false} != true ]] || attempt=4
    jq -n --argjson attempt "$attempt" '{id:123,head_sha:("a"*40),event:"pull_request",run_attempt:$attempt,head_repository:{full_name:"proerror77/monday"},path:".github/workflows/ci.yml",status:"in_progress",conclusion:null}' ;;
  *pulls/42*) jq -n '{state:"open",head:{sha:("a"*40),repo:{full_name:"proerror77/monday"}},base:{sha:("b"*40)}}' ;;
  *) echo 'unexpected API' >&2; exit 1 ;;
esac
MOCK
cat >"$scratch/repo/.github/scripts/wait-ack-research-receipt.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == ci-rust && $GITHUB_RUN_ID == 123 && $GITHUB_RUN_ATTEMPT == 3 && $GITHUB_JOB == rust ]]
[[ $2 == cccccccccccccccccccccccccccccccccccccccc ]]
jq -n '{head_sha:("a"*40),base_sha:("b"*40),public_job_id:789,public_run_id:"123",public_run_attempt:3,event:"pull_request"}' >"$3/receipt.json"
touch "$FIXTURE_ROOT/read"
MOCK
cat >"$scratch/bin/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $* == *'/123/3/rust/cccccccccccccccccccccccccccccccccccccccc/receipt.json?'* ]]
if [[ ${MISSING_FIRST:-false} == true && ! -f $FIXTURE_ROOT/missing ]]; then
  touch "$FIXTURE_ROOT/missing"; exit 22
fi
MOCK
printf '#!/usr/bin/env bash\nexit 0\n' >"$scratch/bin/sleep"
chmod +x "$scratch/bin/gh" "$scratch/bin/curl" "$scratch/bin/sleep"
export PATH="$scratch/bin:$PATH"
(cd "$scratch/repo"; bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})")
rm "$scratch/read"
if (cd "$scratch/repo"; RERUN=true bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})"); then exit 1; fi
rm "$scratch/read"
if (cd "$scratch/repo"; BAD_JOB='unrelated check' bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})"); then exit 1; fi
rm "$scratch/read" "$scratch/discoveries"
(cd "$scratch/repo"; STALE_FIRST=true bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})")
[[ $(cat "$scratch/discoveries") == 2 ]]
rm "$scratch/read" "$scratch/discoveries"
(cd "$scratch/repo"; MISSING_FIRST=true bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})")
[[ $(cat "$scratch/discoveries") == 2 ]]

# The common readback verifier rejects mismatches for either public consumer.
for mutation in '.public_job_id=790' '.public_run_attempt=4' '.head_sha=("d"*40)' 'del(.public_job_id)'; do
  jq "$mutation" "$scratch/ack-rust-batch/receipt.json" >"$scratch/bad-receipt"
  if ack_verify_rust_job "$scratch/bad-receipt" "$scratch/ack-rust-batch/current-run.json" "$scratch/ack-rust-batch/jobs.json"; then exit 1; fi
done
rm "$scratch/read" "$scratch/discoveries"
jq '.pull_request.head.repo.full_name="fork/monday"' "$GITHUB_EVENT_PATH" >"$scratch/fork-event"
if (cd "$scratch/repo"; GITHUB_EVENT_PATH="$scratch/fork-event" bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})"); then exit 1; fi
[[ ! -f $scratch/discoveries && ! -f $scratch/read ]]
ruby -ryaml -e '
  jobs=YAML.load_file(ARGV[0])["jobs"]
  fast=jobs.fetch("rust_fast_gates")
  abort "static Fast still waits on ACK" if fast.to_s.include?("ack_research") || fast.to_s.include?("wait-ack")
  abort "static Fast compiles" if fast.to_s.match?(/cargo (test|check|build|clippy)/)
' "$root/.github/workflows/ci.yml"
echo 'PASS: selected test and strict Clippy coverage, source package binding, producer attempt/job readback and static Fast routing'
