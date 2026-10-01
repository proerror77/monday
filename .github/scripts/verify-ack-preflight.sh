#!/usr/bin/env bash
# Pure verifier shared with the public relay. No dispatch, polling or mutation.
ack_verify_preflight() {
  local receipt=$1 signature=$2 key=$3 expected=$4 producer=$5 jobs=$6 now=$7 current_source=$8 finished expires
  [[ -f $receipt && -f $signature && $(stat -c '%s' "$receipt") -le 262144 && $(stat -c '%s' "$signature") -le 1024 ]] || return 1
  [[ $(sha256sum "$receipt" | awk '{print $1}') == "$(jq -er .preflight.receipt_sha256 "$expected")" ]] || return 1
  openssl pkeyutl -verify -pubin -inkey "$key" -rawin -in "$receipt" -sigfile "$signature" >/dev/null 2>&1 || return 1
  jq -e --slurpfile expected "$expected" '
    $expected[0] as $e | $e.preflight as $p |
    .schema_version=="monday.ack_execution_receipt.v2" and .public_repo=="proerror77/monday" and
    .public_run_id==($p.public_run_id|tostring) and .public_run_attempt==$p.public_run_attempt and
    .public_job=="research_preflight" and .public_job_id==$p.public_job_id and
    .profile=="ci-research-preflight" and .execution_host=="ack" and
    .checkout_sha==$e.checkout_sha and .head_sha==$e.head_sha and .base_sha==$e.base_sha and
    .event==$e.event and .source_ref==$e.source_ref and
    .command_manifest_sha256==$e.command_manifest_sha256 and .scope_sha256==$e.scope_sha256 and
    (.private_run_id|type=="string" and test("^[0-9]+$")) and
    (.private_run_attempt|type=="number" and .>=1 and floor==.) and
    .terminal_result=="success" and .phase_results=={preflight:"success",quick:"success"} and
    (.finished_at|test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")) and
    (.expires_at|test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
  ' "$receipt" >/dev/null || return 1
  finished=$(date -u -d "$(jq -r .finished_at "$receipt")" +%s) || return 1
  expires=$(date -u -d "$(jq -r .expires_at "$receipt")" +%s) || return 1
  (( finished <= now && now < expires && expires > finished && expires - finished <= 28800 )) || return 1
  # Run may be in progress while its preflight job is complete. Requiring the
  # whole run to succeed here would make ci-rust wait for itself.
  jq -e --slurpfile expected "$expected" '
    $expected[0] as $e | .id==$e.preflight.public_run_id and .run_attempt==$e.preflight.public_run_attempt and
    .repository.full_name=="proerror77/monday" and .head_repository.full_name=="proerror77/monday" and
    (.path|split("@")[0])==".github/workflows/ci.yml" and .head_sha==$e.head_sha and .event==$e.event and
    ((.status=="in_progress" and .conclusion==null) or (.status=="completed" and .conclusion=="success"))
  ' "$producer" >/dev/null || return 1
  if [[ $(jq -r .event "$expected") == pull_request ]]; then
    jq -e --slurpfile expected "$expected" '$expected[0] as $e | .state=="open" and .head.sha==$e.head_sha and .base.sha==$e.base_sha and .head.repo.full_name=="proerror77/monday"' "$current_source" >/dev/null || return 1
  else
    jq -e --slurpfile expected "$expected" '.object.sha==$expected[0].checkout_sha' "$current_source" >/dev/null || return 1
  fi
  jq -e --slurpfile expected "$expected" '
    $expected[0] as $e | [.jobs[] | select(.id==$e.preflight.public_job_id)] |
    length==1 and .[0].run_id==$e.preflight.public_run_id and .[0].head_sha==$e.head_sha and
    .[0].name=="Research preflight" and .[0].status=="completed" and .[0].conclusion=="success"
  ' "$jobs" >/dev/null
}
