#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf '%s\n' \
    'Usage: ACTION=gate|cutover INSTANCE_ID=i-... ARTIFACT_SHA256=<64 hex> invoke-rust-lob-operation.sh' \
    '' \
    'The command always targets ap-northeast-1 and uses Alibaba Cloud Assistant.'
}

for command in aliyun base64 jq; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$command" >&2
    exit 2
  fi
done

: "${ACTION:?set ACTION to gate or cutover}"
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
printf -v remote_script '#!/usr/bin/env bash\nset -euo pipefail\nexec %q %q\n' \
  "$host_path" "$ARTIFACT_SHA256"
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
printf 'Cloud Assistant invocation: %s (%s)\n' "$invoke_id" "$ACTION"

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
      if [[ -n "$output" ]]; then
        printf '%s' "$output" | base64 --decode || true
        printf '\n'
      fi
      if [[ "$exit_code" == '0' ]]; then
        printf '%s completed successfully: %s\n' "$ACTION" "$invoke_id"
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
