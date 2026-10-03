#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
source .github/scripts/verify-ci-rust-evidence.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
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
export GITHUB_REPOSITORY=proerror77/monday GITHUB_EVENT_NAME=pull_request
export GITHUB_RUN_ID=123 GITHUB_RUN_ATTEMPT=3 RUST_JOB_RESULT=success
export GITHUB_EVENT_PATH="$work/event.json"
jq -n '{repository:{full_name:"proerror77/monday"},pull_request:{head:{sha:("a"*40),repo:{full_name:"fork/monday"}},base:{sha:("b"*40)}}}' >"$GITHUB_EVENT_PATH"
command_sha=$(cat .github/workflows/ci.yml .github/scripts/write-ci-rust-evidence.sh \
  .github/scripts/verify-ci-rust-evidence.sh rust_hft/scripts/cargo-scoped.sh \
  rust_hft/scripts/workspace-metadata.sh rust_hft/workspaces.json | sha256sum | awk '{print $1}')
jq -n --argjson scope "$(ci_expected_scope "$work/scope")" --arg checkout "$(git rev-parse HEAD)" --arg command "$command_sha" \
  '{schema:"monday.ci_rust_evidence.v1",scope:$scope,stages:[{stage:"loop",outcome:"success"},{stage:"clippy_loop",outcome:"success"}],checkout_sha:$checkout,command_sha256:$command,repository:"proerror77/monday",source_repository:"fork/monday",event:"pull_request",head_sha:("a"*40),base_sha:("b"*40),run_id:"123",run_attempt:3,job_id:789}' >"$work/receipt"
bash .github/scripts/verify-ci-rust-same-run.sh "$work/receipt" "$work/scope"
for mutation in '.source_repository="proerror77/monday"' '.repository="fork/monday"' \
  '.head_sha=("c"*40)' '.base_sha=("c"*40)' '.checkout_sha=("c"*40)' \
  '.run_id="999"' '.run_attempt=4' '.job_id=0' '.command_sha256=("d"*64)' \
  '.scope.loop_packages=",alpha-domain,"' '.stages[1].outcome="failure"' '.stages|=map(select(.stage!="clippy_loop"))'; do
  jq "$mutation" "$work/receipt" >"$work/mutated"
  if bash .github/scripts/verify-ci-rust-same-run.sh "$work/mutated" "$work/scope" >"$work/rejected" 2>&1; then
    echo "wrong same-run evidence was accepted: $mutation" >&2; exit 1
  fi
done
if RUST_JOB_RESULT=failure bash .github/scripts/verify-ci-rust-same-run.sh "$work/receipt" "$work/scope" >"$work/rejected" 2>&1; then
  echo 'failed producer was accepted' >&2; exit 1
fi
# A full manual run has no PR comparison base. Bind both ends to its exact SHA.
GITHUB_SHA=$(git rev-parse HEAD)
export GITHUB_EVENT_NAME=workflow_dispatch GITHUB_SHA
jq -n '{repository:{full_name:"proerror77/monday"}}' >"$GITHUB_EVENT_PATH"
jq --arg head "$GITHUB_SHA" '.event="workflow_dispatch" | .source_repository="proerror77/monday" | .head_sha=$head | .base_sha=$head' "$work/receipt" >"$work/manual"
bash .github/scripts/verify-ci-rust-same-run.sh "$work/manual" "$work/scope"
jq '.base_sha=("b"*40)' "$work/manual" >"$work/bad-manual"
if bash .github/scripts/verify-ci-rust-same-run.sh "$work/bad-manual" "$work/scope" >"$work/rejected" 2>&1; then
  echo 'manual source snapshot mismatch was admitted' >&2; exit 1
fi
printf 'same-run Clippy scope/source/fork/checkout/attempt/command and failure contracts passed\n'
