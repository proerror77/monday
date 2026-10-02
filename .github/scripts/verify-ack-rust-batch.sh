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
    .validation.schema_version=="monday.ack_rust_batch.v1" and
    .validation.scope==$expected and ($required|length)>0 and
    ([.validation.stages[].stage]|sort)==($required|sort) and
    all(.validation.stages[]; .exit_code==0 and
      (.input_sha256|type=="string" and test("^[0-9a-f]{64}$")) and
      (.private_run_id|type=="string" and test("^[0-9]+$")) and
      (.completed_epoch|type=="number" and floor==.) and (.reused|type=="boolean"))
  ' "$receipt" >/dev/null
}
