#!/usr/bin/env bash
# Pure validators; API metadata must be fetched independently by the consumer.
ci_expected_scope() {
  local selected=$1 result='{}' flag value
  for flag in loop handoff json ondo collector control focused toolchain clippy_loop clippy_handoff owning_packages loop_packages focused_packages; do
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
  local checks
  checks=$(jq -c --argjson scope "$expected" --argjson required "$required" '{
    schema:(.schema=="monday.ci_rust_evidence.v1"), scope:(.scope==$scope),
    exact_stages:(([.stages[].stage]|sort)==($required|sort)),
    stage_outcomes:all(.stages[];.outcome=="success"),
    checkout_format:(.checkout_sha|test("^[0-9a-f]{40}$")),
    command_format:(.command_sha256|test("^[0-9a-f]{64}$"))}' "$receipt") || return 1
  ci_assert_checks "$checks" 'Rust evidence stages'
}
ci_assert_checks() {
  local checks=$1 context=$2 failures
  failures=$(jq -r 'to_entries|map(select(.value!=true)|.key)|join(",")' <<<"$checks") || return 1
  if [[ -n $failures ]]; then
    printf '%s rejected: %s\n' "$context" "$failures" >&2
    return 1
  fi
}
ci_verify_producer() {
  local receipt=$1 run=$2 jobs=$3 event=$4 checkout=$5
  local checks
  checks=$(jq -c --slurpfile run "$run" --slurpfile jobs "$jobs" --slurpfile event "$event" --arg checkout "$checkout" '
    . as $r | $run[0] as $run | $event[0] as $e |
    ($e.pull_request.head.sha // $e.after) as $head |
    ($e.pull_request.base.sha // $e.before) as $base |
    ($e.pull_request.head.repo.full_name // $e.repository.full_name) as $source |
    {
      attempt_format:($r.run_attempt|type=="number" and .>0 and floor==.),
      job_format:($r.job_id|type=="number" and .>0 and floor==.),
      checkout:($r.checkout_sha==$checkout),head:($r.head_sha==$head),base:($r.base_sha==$base),source:($r.source_repository==$source),
      run_id:($run.id==($r.run_id|tonumber)),attempt:($run.run_attempt==$r.run_attempt),
      run_head:($run.head_sha==$head),event:($run.event==$r.event),
      workflow:($run.path==".github/workflows/ci.yml"),repository:($run.repository.full_name==$r.repository),
      source_repository:($run.head_repository.full_name==$source),
      run_status:($run.status=="in_progress" or $run.status=="completed"),
      pull_request:($r.event!="pull_request" or any($run.pull_requests[];.number==$e.pull_request.number and .head.sha==$head and .base.sha==$base)),
      successful_job:([$jobs[0].jobs[]|select(.id==$r.job_id and .run_id==$run.id and .run_attempt==$r.run_attempt and .head_sha==$head and
        .name=="Rust Workspace" and .status=="completed" and .conclusion=="success")]|length==1)
    }' "$receipt") || return 1
  ci_assert_checks "$checks" 'Rust evidence producer'
}

# Bind the complete executable CI surface. Include paths and file boundaries.
ci_rust_command_sha() {
  (
    cd "${1:-.}" || return 1
    sha256sum .github/workflows/ci.yml \
      .github/scripts/write-ci-rust-evidence.sh .github/scripts/verify-ci-rust-evidence.sh \
      .github/scripts/verify-ci-rust-same-run.sh .github/scripts/wait-ci-rust-evidence.sh \
      rust_hft/scripts/cargo-scoped.sh rust_hft/scripts/workspace-metadata.sh rust_hft/workspaces.json \
      .github/scripts/install-cargo-nextest.sh .github/scripts/loop-nextest-archive.sh \
      .github/scripts/loop-nextest-doctests.sh .github/scripts/loop-nextest-plan.rb \
      .github/scripts/loop-nextest-shard.sh .github/scripts/loop-nextest-counts.sh \
      .github/scripts/loop-nextest-wait.sh .github/scripts/loop-nextest-gate.sh \
      rust_hft/research-core/.config/nextest.toml | sha256sum | awk '{print $1}'
  )
}
