#!/usr/bin/env bash
# Called only after the receipt signature and producer identity are verified.
ack_verify_rust_batch() {
  local receipt=$1 selected=$2 expected='{}' flag value required
  for flag in loop handoff json ondo collector control focused toolchain clippy_loop clippy_handoff owning_packages loop_packages; do
    value=$(awk -F= -v key="$flag" '$1==key {print $2}' "$selected")
    if [[ $flag == *_packages ]]; then
      [[ $value =~ ^(,,|,([A-Za-z0-9_-]+,)+)$ ]] || return 1
      expected=$(jq -c --arg key "$flag" --arg value "$value" '.+{($key):$value}' <<<"$expected")
    else
      [[ $value == true || $value == false ]] || return 1
      expected=$(jq -c --arg key "$flag" --argjson value "$value" '.+{($key):$value}' <<<"$expected")
    fi
  done
  required=$(jq -c '["collector","loop","owning","handoff","json","ondo","control","focused","clippy_loop","clippy_handoff"] |
    map(. as $stage | select(if $stage=="owning" then $scope.owning_packages!=",," else $scope[$stage]==true end))' --argjson scope "$expected" <<<null)
  jq -e --argjson expected "$expected" --argjson required "$required" '
    .profile=="ci-rust" and .terminal_result=="success" and
    (.public_job_id|type=="number" and .>0 and floor==.) and
    .validation.schema_version=="monday.ack_rust_batch.v1" and
    .validation.scope==$expected and ($required|length)>0 and
    ([.validation.stages[].stage]|sort)==($required|sort) and
    all(.validation.stages[]; .exit_code==0 and
      (.input_sha256|type=="string" and test("^[0-9a-f]{64}$")) and
      (.private_run_id|type=="string" and test("^[0-9]+$")) and
      (.completed_epoch|type=="number" and floor==.) and (.reused|type=="boolean"))
  ' "$receipt" >/dev/null
}

# Metadata is fetched independently of the signed receipt. Both public
# consumers bind success to the exact producing attempt and numeric job.
ack_verify_rust_job() {
  local receipt=$1 run=$2 jobs=$3
  jq -e --slurpfile run "$run" --slurpfile jobs "$jobs" '
    . as $r | $run[0] as $run |
    ($r.public_job_id|type=="number" and .>0 and floor==.) and
    ($r.public_run_attempt|type=="number" and .>0 and floor==.) and
    $run.id==($r.public_run_id|tonumber) and $run.run_attempt==$r.public_run_attempt and
    $run.head_sha==$r.head_sha and $run.event==$r.event and
    $run.head_repository.full_name=="proerror77/monday" and $run.path==".github/workflows/ci.yml" and
    (($run.status=="in_progress" and $run.conclusion==null) or
      ($run.status=="completed" and $run.conclusion=="success")) and
    ([$jobs[0].jobs[]|select(.id==$r.public_job_id and .run_id==$run.id and
      .head_sha==$r.head_sha and .name=="Rust Workspace" and
      ((.status=="in_progress" and .conclusion==null) or (.status=="completed" and .conclusion=="success")))]|length==1)
  ' "$receipt" >/dev/null
}
