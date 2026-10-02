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
jq -n --arg key "$(printf a%.0s {1..64})" '{profile:"ci-rust",terminal_result:"success",
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
export GITHUB_REPOSITORY=proerror77/monday GITHUB_EVENT_NAME=pull_request
export GITHUB_RUN_ID=456 GITHUB_RUN_ATTEMPT=2 GITHUB_JOB=clippy-strict
export GITHUB_EVENT_PATH="$scratch/event" RUNNER_TEMP="$scratch" FIXTURE_ROOT="$scratch"
jq -n '{pull_request:{number:42,head:{sha:("a"*40)},base:{sha:("b"*40)}}}' >"$GITHUB_EVENT_PATH"
cat >"$scratch/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *workflows/ci.yml/runs*) echo '{"workflow_runs":[{"id":123,"head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","head_repository":{"full_name":"proerror77/monday"},"event":"pull_request","path":".github/workflows/ci.yml"}]}' ;;
  *attempts/3/jobs*)
    jq -n --arg name "${BAD_JOB:-Rust Workspace}" '{jobs:[{id:789,run_id:123,head_sha:("a"*40),name:$name,status:"completed",conclusion:"success"}]}' ;;
  *actions/runs/123*)
    attempt=3
    [[ ! -e $FIXTURE_ROOT/read || ${RERUN:-false} != true ]] || attempt=4
    jq -n --argjson attempt "$attempt" '{head_sha:("a"*40),event:"pull_request",run_attempt:$attempt,head_repository:{full_name:"proerror77/monday"},path:".github/workflows/ci.yml",status:"in_progress",conclusion:null}' ;;
  *pulls/42*) jq -n '{state:"open",head:{sha:("a"*40),repo:{full_name:"proerror77/monday"}},base:{sha:("b"*40)}}' ;;
  *) echo 'unexpected API' >&2; exit 1 ;;
esac
MOCK
cat >"$scratch/repo/.github/scripts/wait-ack-research-receipt.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == ci-rust && $GITHUB_RUN_ID == 123 && $GITHUB_RUN_ATTEMPT == 3 && $GITHUB_JOB == rust ]]
[[ $2 == cccccccccccccccccccccccccccccccccccccccc ]]
jq -n '{head_sha:("a"*40),base_sha:("b"*40),public_job_id:789}' >"$3/receipt.json"
touch "$FIXTURE_ROOT/read"
MOCK
chmod +x "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH"
(cd "$scratch/repo"; bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})")
rm "$scratch/read"
if (cd "$scratch/repo"; RERUN=true bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})"); then exit 1; fi
rm "$scratch/read"
if (cd "$scratch/repo"; BAD_JOB='unrelated check' bash .github/scripts/wait-ack-rust-batch.sh "$(printf c%.0s {1..40})"); then exit 1; fi
ruby -ryaml -e '
  jobs=YAML.load_file(ARGV[0])["jobs"]
  fast=jobs.fetch("rust_fast_gates")
  abort "static Fast still waits on ACK" if fast.to_s.include?("ack_research") || fast.to_s.include?("wait-ack")
  abort "static Fast compiles" if fast.to_s.match?(/cargo (test|check|build|clippy)/)
' "$root/.github/workflows/ci.yml"
echo 'PASS: selected test and strict Clippy coverage, source package binding, producer attempt/job readback and static Fast routing'
