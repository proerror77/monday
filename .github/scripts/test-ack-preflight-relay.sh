#!/usr/bin/env bash
set -euo pipefail
scripts=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
export FIXTURE="$fixture" RELAY_CASE=success
mkdir -p "$fixture/repo/.github/scripts" "$fixture/repo/.github/ack-ci" "$fixture/software"
cp "$scripts/wait-ack-research-receipt.sh" "$scripts/verify-ack-preflight.sh" "$fixture/repo/.github/scripts/"
cat >"$fixture/repo/.github/scripts/select-rust-ci-scope.sh" <<'SCOPE'
printf 'collector=%s\n' "${FIXTURE_COLLECTOR:-true}" >>"$GITHUB_OUTPUT"
SCOPE
git -C "$fixture/repo" init -q
openssl genpkey -algorithm ED25519 -out "$fixture/key" 2>/dev/null
openssl pkey -in "$fixture/key" -pubout -out "$fixture/repo/.github/ack-ci/receipt-public-key.pub" 2>/dev/null
head_sha=$(printf a%.0s {1..40}); base_sha=$(printf b%.0s {1..40}); source_sha=$(printf c%.0s {1..40}); hash=$(printf d%.0s {1..64})
finished=$(date -u -d '5 seconds ago' +%FT%TZ); expires=$(date -u -d '10 minutes' +%FT%TZ)
jq -n --arg head "$head_sha" --arg base "$base_sha" '{pull_request:{head:{sha:$head,repo:{full_name:"proerror77/monday"}},base:{sha:$base}}}' >"$fixture/event"
jq -n --arg head "$head_sha" --arg base "$base_sha" --arg source "$source_sha" --arg hash "$hash" --arg finished "$finished" --arg expires "$expires" '
 {schema_version:"monday.ack_execution_receipt.v2",public_repo:"proerror77/monday",public_run_id:"101",public_run_attempt:2,
  public_job:"research_preflight",public_job_id:501,checkout_sha:$source,head_sha:$head,base_sha:$base,event:"pull_request",source_ref:"refs/pull/1262/merge",
  profile:"ci-research-preflight",execution_host:"ack",private_run_id:"301",private_run_attempt:1,command_manifest_sha256:$hash,scope_sha256:$hash,
  terminal_result:"success",phase_results:{preflight:"success",quick:"success"},finished_at:$finished,expires_at:$expires}' >"$fixture/proof.json"
openssl pkeyutl -sign -inkey "$fixture/key" -rawin -in "$fixture/proof.json" -out "$fixture/proof.sig"
printf 'reviewed fixture\n' >"$fixture/software/proof.txt"
tar -czf "$fixture/software.tar.gz" -C "$fixture/software" .
jq --arg proof_hash "$(sha256sum "$fixture/proof.json"|awk '{print $1}')" --arg bundle_hash "$(sha256sum "$fixture/software.tar.gz"|awk '{print $1}')" \
  --argjson bytes "$(stat -c '%s' "$fixture/software.tar.gz")" '
 .public_run_id="202"|.public_run_attempt=3|.public_job="research-image-binaries"|.profile="research-image-binaries"|
 .preflight={public_run_id:101,public_run_attempt:2,public_job_id:501,receipt_sha256:$proof_hash}|
 .software_bundle={url:"https://example.invalid/verified-software.tar.gz",sha256:$bundle_hash,bytes:$bytes}' "$fixture/proof.json" >"$fixture/heavy.json"
openssl pkeyutl -sign -inkey "$fixture/key" -rawin -in "$fixture/heavy.json" -out "$fixture/heavy.sig"
jq -n --arg head "$head_sha" '{id:101,run_attempt:2,repository:{full_name:"proerror77/monday"},head_repository:{full_name:"proerror77/monday"},path:".github/workflows/ci.yml",head_sha:$head,event:"pull_request",status:"in_progress",conclusion:null}' >"$fixture/run.json"
jq -n --arg head "$head_sha" '{jobs:[{id:501,run_id:101,head_sha:$head,name:"Research preflight",status:"completed",conclusion:"success"}]}' >"$fixture/jobs.json"
jq -n --arg head "$head_sha" --arg base "$base_sha" '{state:"open",head:{sha:$head,repo:{full_name:"proerror77/monday"}},base:{sha:$base}}' >"$fixture/current.json"
curl() {
  local url='' output=''
  [[ $RELAY_CASE != timeout ]] || return 22
  while (($#)); do case "$1" in -o) output=$2; shift 2 ;; https://*) url=$1; shift ;; *) shift ;; esac; done
  case "$url" in
    */202/research-image-binaries/*/receipt.json\?*) cp "$FIXTURE/legacy.json" "$output" ;;
    */202/research-image-binaries/*/receipt.sig\?*) cp "$FIXTURE/legacy.sig" "$output" ;;
    */202/3/research-image-binaries/*/receipt.json\?*) cp "$FIXTURE/heavy.json" "$output" ;;
    */202/3/research-image-binaries/*/receipt.sig\?*) cp "$FIXTURE/heavy.sig" "$output" ;;
    */101/2/research_preflight/*/receipt.json*) cp "$FIXTURE/proof.json" "$output" ;;
    */101/2/research_preflight/*/receipt.sig*) cp "$FIXTURE/proof.sig" "$output" ;;
    */actions/runs/101) cp "$FIXTURE/run.json" "$output" ;;
    */actions/runs/101/attempts/2/jobs\?per_page=100) cp "$FIXTURE/jobs.json" "$output" ;;
    */pulls/1262) cp "$FIXTURE/current.json" "$output" ;;
    https://example.invalid/verified-software.tar.gz) cp "$FIXTURE/software.tar.gz" "$output" ;;
    *) echo "unexpected HTTP fixture: $url" >&2; return 22 ;;
  esac
}
date() {
  if [[ $RELAY_CASE == timeout && ${1:-} == +%s ]]; then
    local count=0; [[ ! -f $FIXTURE/clock ]] || count=$(cat "$FIXTURE/clock")
    count=$((count+40)); printf '%s' "$count" >"$FIXTURE/clock"; printf '%s\n' "$count"
  else command date "$@"; fi
}
sleep(){ :; }
export -f curl date sleep
export GITHUB_REPOSITORY=proerror77/monday GITHUB_EVENT_NAME=pull_request GITHUB_EVENT_PATH="$fixture/event" ACK_RECEIPT_TIMEOUT_SECONDS=60
cd "$fixture/repo"
GITHUB_RUN_ID=101 GITHUB_RUN_ATTEMPT=2 GITHUB_JOB=research_preflight bash .github/scripts/wait-ack-research-receipt.sh ci-research-preflight "$source_sha" "$fixture/quick-out"
GITHUB_RUN_ID=202 GITHUB_RUN_ATTEMPT=3 GITHUB_JOB=research-image-binaries bash .github/scripts/wait-ack-research-receipt.sh research-image-binaries "$source_sha" "$fixture/heavy-out"
cmp "$fixture/software/proof.txt" "$fixture/heavy-out/software/proof.txt"
jq '.run_attempt=3' "$fixture/run.json" >"$fixture/new-run"; mv "$fixture/new-run" "$fixture/run.json"
if GITHUB_RUN_ID=202 GITHUB_RUN_ATTEMPT=3 GITHUB_JOB=research-image-binaries bash .github/scripts/wait-ack-research-receipt.sh research-image-binaries "$source_sha" "$fixture/stale-out"; then exit 1; fi
if RELAY_CASE=timeout GITHUB_RUN_ID=202 GITHUB_RUN_ATTEMPT=4 GITHUB_JOB=research-image-binaries bash .github/scripts/wait-ack-research-receipt.sh research-image-binaries "$source_sha" "$fixture/timeout-out"; then exit 1; fi
jq '.schema_version="monday.ack_execution_receipt.v1"|del(.preflight)' "$fixture/heavy.json" >"$fixture/legacy.json"
openssl pkeyutl -sign -inkey "$fixture/key" -rawin -in "$fixture/legacy.json" -out "$fixture/legacy.sig"
FIXTURE_COLLECTOR=false GITHUB_RUN_ID=202 GITHUB_RUN_ATTEMPT=3 GITHUB_JOB=research-image-binaries bash .github/scripts/wait-ack-research-receipt.sh research-image-binaries "$source_sha" "$fixture/legacy-out"
printf 'PASS: non-collector keeps existing v1 receipt and binary download without preflight\n'
printf 'PASS: actual public v2 relay accepts quick/heavy proof, downloads verified software, rejects producer rerun and bounded missing-receipt timeout\n'

# Signed failures have no success-only phase or software requirements and end
# immediately even when the producer proof is no longer usable.
jq '.terminal_result="failure"|.phase_results=null|.preflight=null|.software_bundle=null' "$fixture/heavy.json" >"$fixture/failed.json"
mv "$fixture/failed.json" "$fixture/heavy.json"
openssl pkeyutl -sign -inkey "$fixture/key" -rawin -in "$fixture/heavy.json" -out "$fixture/heavy.sig"
if GITHUB_RUN_ID=202 GITHUB_RUN_ATTEMPT=3 GITHUB_JOB=research-image-binaries bash .github/scripts/wait-ack-research-receipt.sh research-image-binaries "$source_sha" "$fixture/failed-out" >"$fixture/failure.log"; then exit 1; fi
grep -Fq '"terminal_result": "failure"' "$fixture/failure.log"
[[ ! -e $fixture/failed-out/preflight.json && ! -e $fixture/failed-out/software.tar.gz ]]
printf 'PASS: a signed negative terminal ends the public wait without success-only proof or artifact lookups\n'
