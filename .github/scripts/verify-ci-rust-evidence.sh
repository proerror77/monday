#!/usr/bin/env bash
# Pure validators; API metadata must be fetched independently by the consumer.
ci_expected_scope() {
  local selected=$1 result='{}' flag value
  for flag in loop handoff json ondo collector control focused toolchain clippy_loop clippy_handoff owning_packages loop_packages; do
    value=$(awk -F= -v key="$flag" '$1==key {print $2}' "$selected")
    if [[ $flag == *_packages ]]; then
      [[ $value =~ ^(,,|,([A-Za-z0-9_-]+,)+)$ ]] || return 1
      result=$(jq -c --arg key "$flag" --arg value "$value" '.+{($key):$value}' <<<"$result")
    else
      [[ $value == true || $value == false ]] || return 1
      result=$(jq -c --arg key "$flag" --argjson value "$value" '.+{($key):$value}' <<<"$result")
    fi
  done
  printf '%s\n' "$result"
}
ci_verify_stages() {
  local receipt=$1 selected=$2 expected required
  expected=$(ci_expected_scope "$selected") || return 1
  required=$(jq -cn --argjson scope "$expected" '["collector","loop","owning","handoff","json","ondo","control","focused","clippy_loop","clippy_handoff"]|map(. as $s|select(if $s=="owning" then $scope.owning_packages!=",," else $scope[$s]==true end))')
  jq -e --argjson scope "$expected" --argjson required "$required" '
    .schema=="monday.ci_rust_evidence.v1" and .scope==$scope and
    ([.stages[].stage]|sort)==($required|sort) and
    all(.stages[];.outcome=="success") and
    (.checkout_sha|test("^[0-9a-f]{40}$")) and
    (.command_sha256|test("^[0-9a-f]{64}$"))' "$receipt" >/dev/null
}
ci_verify_producer() {
  local receipt=$1 run=$2 jobs=$3 event=$4 checkout=$5
  jq -e --slurpfile run "$run" --slurpfile jobs "$jobs" --slurpfile event "$event" --arg checkout "$checkout" '
    . as $r | $run[0] as $run | $event[0] as $e |
    ($e.pull_request.head.sha // $e.after) as $head |
    ($e.pull_request.base.sha // $e.before) as $base |
    ($e.pull_request.head.repo.full_name // $e.repository.full_name) as $source |
    ($r.run_attempt|type=="number" and .>0 and floor==.) and
    ($r.job_id|type=="number" and .>0 and floor==.) and
    $r.checkout_sha==$checkout and $r.head_sha==$head and $r.base_sha==$base and $r.source_repository==$source and
    $run.id==($r.run_id|tonumber) and $run.run_attempt==$r.run_attempt and
    $run.head_sha==$head and $run.event==$r.event and
    $run.path==".github/workflows/ci.yml" and $run.repository.full_name==$r.repository and
    $run.head_repository.full_name==$source and
    ($run.status=="in_progress" or $run.status=="completed") and
    ($r.event!="pull_request" or any($run.pull_requests[];.number==$e.pull_request.number and .head.sha==$head and .base.sha==$base)) and
    ([$jobs[0].jobs[]|select(.id==$r.job_id and .run_id==$run.id and .head_sha==$head and
      .name=="Rust Workspace" and .status=="completed" and .conclusion=="success")]|length==1)' "$receipt" >/dev/null
}
