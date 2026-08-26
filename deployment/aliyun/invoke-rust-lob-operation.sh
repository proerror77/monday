#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf '%s\n' \
    'Usage: ACTION=gate-preflight|gate|cutover|restore INSTANCE_ID=i-... ARTIFACT_SHA256=<64 hex> invoke-rust-lob-operation.sh' \
    '' \
    'The command always targets ap-northeast-1 and uses Alibaba Cloud Assistant.'
}

for command in aliyun base64 jq seq sleep tr; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$command" >&2
    exit 2
  fi
done

: "${ACTION:?set ACTION to gate-preflight, gate, cutover, or restore}"
: "${INSTANCE_ID:?set INSTANCE_ID}"
: "${ARTIFACT_SHA256:?set ARTIFACT_SHA256}"

REGION_ID=${REGION_ID:-ap-northeast-1}
ALIYUN_LOCAL_PROFILE=${ALIYUN_LOCAL_PROFILE:-default}
if [[ "$REGION_ID" != 'ap-northeast-1' ]]; then
  printf 'refusing non-Tokyo region: %s\n' "$REGION_ID" >&2
  exit 2
fi
if [[ ! "$INSTANCE_ID" =~ ^i-[a-z0-9]+$ ]]; then
  usage >&2
  exit 2
fi
if [[ ! "$ARTIFACT_SHA256" =~ ^[A-Fa-f0-9]{64}$ ]]; then
  printf 'ARTIFACT_SHA256 must contain exactly 64 hexadecimal characters\n' >&2
  exit 2
fi
ARTIFACT_SHA256=$(printf '%s' "$ARTIFACT_SHA256" | tr '[:upper:]' '[:lower:]')

case "$ACTION" in
  gate-preflight)
    host_script=host-rust-lob-shadow-gate.sh
    timeout_seconds=300
    command_name=monday-rust-lob-gate-preflight
    ;;
  gate)
    host_script=host-rust-lob-shadow-gate.sh
    timeout_seconds=7200
    command_name=monday-rust-lob-shadow-gate
    ;;
  cutover)
    host_script=host-rust-lob-cutover.sh
    timeout_seconds=3600
    command_name=monday-rust-lob-cutover
    ;;
  restore)
    host_script=host-rust-lob-restore.sh
    timeout_seconds=3600
    command_name=monday-rust-lob-restore
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac

polls=$((timeout_seconds / 5))
cancel_polls=12
if [[ -n ${MONDAY_OPERATION_TEST_POLLS:-} || -n ${MONDAY_OPERATION_TEST_CANCEL_POLLS:-} ]]; then
  [[ ${MONDAY_ALLOW_SHORT_OPERATION_TEST:-0} == 1 ]] || {
    printf 'short operation polling requires MONDAY_ALLOW_SHORT_OPERATION_TEST=1\n' >&2
    exit 2
  }
  polls=${MONDAY_OPERATION_TEST_POLLS:-$polls}
  cancel_polls=${MONDAY_OPERATION_TEST_CANCEL_POLLS:-$cancel_polls}
  [[ $polls =~ ^[1-9][0-9]*$ && $cancel_polls =~ ^[1-9][0-9]*$ ]] || {
    printf 'test poll counts must be positive integers\n' >&2
    exit 2
  }
fi

aliyun_profile_args=()
if [[ -n "$ALIYUN_LOCAL_PROFILE" ]]; then
  aliyun_profile_args=(--profile "$ALIYUN_LOCAL_PROFILE")
fi

host_path="/opt/monday/releases/binance-lob-archiver/$ARTIFACT_SHA256/deployment/$host_script"
if [[ $ACTION == gate-preflight ]]; then
  printf -v remote_script '#!/usr/bin/env bash\nset -euo pipefail\nexec %q --resource-preflight %q\n' \
    "$host_path" "$ARTIFACT_SHA256"
else
  printf -v remote_script '#!/usr/bin/env bash\nset -euo pipefail\nexec %q %q\n' \
    "$host_path" "$ARTIFACT_SHA256"
fi
command_content=$(printf '%s' "$remote_script" | base64 | tr -d '\n')

run_json=$(aliyun ecs RunCommand \
  --RegionId "$REGION_ID" \
  --InstanceId.1 "$INSTANCE_ID" \
  --Type RunShellScript \
  --ContentEncoding Base64 \
  --CommandContent "$command_content" \
  --KeepCommand false \
  --Name "$command_name" \
  --Timeout "$timeout_seconds" \
  "${aliyun_profile_args[@]}")
invoke_id=$(printf '%s' "$run_json" | jq -er '.InvokeId')
if [[ $ACTION == gate-preflight ]]; then
  printf 'Cloud Assistant invocation: %s (%s)\n' "$invoke_id" "$ACTION" >&2
else
  printf 'Cloud Assistant invocation: %s (%s)\n' "$invoke_id" "$ACTION"
fi

result_json=''
for _ in $(seq 1 "$polls"); do
  if ! result_json=$(aliyun ecs DescribeInvocationResults \
    --RegionId "$REGION_ID" \
    --InvokeId "$invoke_id" \
    --InstanceId "$INSTANCE_ID" \
    "${aliyun_profile_args[@]}"); then
    sleep 5
    continue
  fi
  status=$(printf '%s' "$result_json" \
    | jq -r '[.. | objects | .InvocationStatus? // empty][0] // empty')
  exit_code=$(printf '%s' "$result_json" \
    | jq -r '[.. | objects | .ExitCode? // empty][0] // empty')
  case "$status" in
    Success|Finished)
      output=$(printf '%s' "$result_json" \
        | jq -r '[.. | objects | .Output? // empty][0] // empty')
      if [[ $ACTION == gate-preflight && $exit_code == 0 ]]; then
        [[ -n $output ]] || {
          printf 'gate-preflight returned no JSON output\n' >&2
          exit 1
        }
        decoded_output=$(printf '%s' "$output" | base64 --decode) || {
          printf 'gate-preflight returned invalid base64 output\n' >&2
          exit 1
        }
        jq -e --arg artifact "$ARTIFACT_SHA256" \
          '.schema == "monday.rust_lob_gate_resource_preflight.v1"
            and .candidate_sha256 == $artifact
            and (.runtime_contract_sha256 | type) == "string"
            and (.runtime_contract_sha256 | test("^[a-f0-9]{64}$"))
            and (.deployment_bundle_sha256 | type) == "string"
            and (.deployment_bundle_sha256 | test("^[a-f0-9]{64}$"))
            and (.deployment_source_revision | type) == "string"
            and (.deployment_source_revision | test("^[a-f0-9]{40,64}$"))
            and (.host_memory_total_bytes | type) == "number"
            and .host_memory_total_bytes == (.host_memory_total_bytes | floor)
            and .host_memory_total_bytes > 0
            and (.host_swap_total_bytes | type) == "number"
            and .host_swap_total_bytes == (.host_swap_total_bytes | floor)
            and .host_swap_total_bytes >= 0
            and (.maximum_sequential_phase_memory_bytes | type) == "number"
            and .maximum_sequential_phase_memory_bytes
              == (.maximum_sequential_phase_memory_bytes | floor)
            and .maximum_sequential_phase_memory_bytes > 0
            and (.production_memory_current_bytes | type) == "object"
            and (.production_memory_current_bytes | keys | sort) == ["spot","usdm"]
            and all(.production_memory_current_bytes[];
              (.active_state == "active" or .active_state == "inactive")
              and (if .active_state == "active" then
                all([.current_bytes,.peak_bytes,.memory_max_bytes,.growth_target_bytes][];
                  type == "number" and . == floor and . >= 0)
                and .current_bytes <= .peak_bytes
                and .peak_bytes <= .growth_target_bytes
                and .growth_target_bytes <= .memory_max_bytes
              else
                (.current_bytes == null or
                  ((.current_bytes | type) == "number"
                    and .current_bytes == (.current_bytes | floor)
                    and .current_bytes >= 0))
                and .peak_bytes == null
                and .memory_max_bytes == null
                and .growth_target_bytes == null
              end))
            and .resource_preflight.phase == "resource-preflight"
            and (.resource_preflight.sampled_at | type) == "string"
            and (.resource_preflight.sampled_at
              | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
            and (.resource_preflight.host_memory_available_bytes | type) == "number"
            and .resource_preflight.host_memory_available_bytes
              == (.resource_preflight.host_memory_available_bytes | floor)
            and .resource_preflight.host_memory_reserve_bytes == 1073741824
            and .resource_preflight.phase_memory_max_bytes
              == .maximum_sequential_phase_memory_bytes
            and .resource_preflight.production_memory_growth_margin_bytes == 268435456
            and (.resource_preflight.production_memory_growth_headroom_bytes
              | type) == "number"
            and .resource_preflight.production_memory_growth_headroom_bytes
              == (.resource_preflight.production_memory_growth_headroom_bytes | floor)
            and .resource_preflight.production_memory_growth_headroom_bytes >= 0
            and .resource_preflight.production_memory_growth_headroom_bytes
              == ([.production_memory_current_bytes[]
                | select(.active_state == "active")
                | .growth_target_bytes - .current_bytes] | add // 0)
            and .resource_preflight.required_bytes
              == (.resource_preflight.host_memory_reserve_bytes
                + .resource_preflight.phase_memory_max_bytes
                + .resource_preflight.production_memory_growth_headroom_bytes)
            and .resource_preflight.host_memory_available_bytes
              >= .resource_preflight.required_bytes
            and .passed == true' <<<"$decoded_output" >/dev/null || {
          printf 'gate-preflight returned invalid JSON evidence\n' >&2
          exit 1
        }
        printf '%s\n' "$decoded_output"
      elif [[ -n "$output" ]]; then
        printf '%s' "$output" | base64 --decode || true
        printf '\n'
      fi
      if [[ "$exit_code" == '0' ]]; then
        if [[ $ACTION == gate-preflight ]]; then
          printf '%s completed successfully: %s\n' "$ACTION" "$invoke_id" >&2
        else
          printf '%s completed successfully: %s\n' "$ACTION" "$invoke_id"
        fi
        exit 0
      fi
      printf '%s\n' "$result_json" >&2
      exit 1
      ;;
    Failed|Stopped|PartialFailed|Timeout)
      printf '%s\n' "$result_json" >&2
      exit 1
      ;;
  esac
  sleep 5
done

printf 'timed out locally; stopping Cloud Assistant invocation %s\n' "$invoke_id" >&2
aliyun ecs StopInvocation \
  --RegionId "$REGION_ID" \
  --InvokeId "$invoke_id" \
  --InstanceId.1 "$INSTANCE_ID" \
  "${aliyun_profile_args[@]}" >/dev/null || true
for _ in $(seq 1 "$cancel_polls"); do
  result_json=$(aliyun ecs DescribeInvocationResults \
    --RegionId "$REGION_ID" \
    --InvokeId "$invoke_id" \
    --InstanceId "$INSTANCE_ID" \
    "${aliyun_profile_args[@]}" || true)
  status=$(printf '%s' "$result_json" \
    | jq -r '[.. | objects | .InvocationStatus? // empty][0] // empty')
  case "$status" in
    Success|Finished|Failed|Stopped|PartialFailed|Timeout)
      printf 'invocation reached terminal state after cancellation: %s\n' "$status" >&2
      exit 1
      ;;
  esac
  sleep 5
done
printf 'invocation did not confirm cancellation: %s\n' "$invoke_id" >&2
exit 1
