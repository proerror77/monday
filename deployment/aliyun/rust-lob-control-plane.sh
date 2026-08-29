#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf '%s\n' \
    "Usage: ${0##*/} release --instance i-... --uri oss://... --payload <64 hex> --source <git sha>" \
    "       ${0##*/} gate --instance i-... --from-controller <sha|direct> --candidate-controller <64 hex> [--preflight-only]" \
    "       ${0##*/} cutover --instance i-... --from <sha|direct> --to <64 hex> --gate-receipt /path --gate-sha256 <64 hex>" \
    "       ${0##*/} restore --instance i-... --controller <64 hex>" \
    "       ${0##*/} readback --instance i-... --controller <64 hex> --transition-receipt /path --receipt-sha256 <64 hex>" \
    '' \
    'The operator accepts only these five operations and always targets Tokyo.' >&2
}

die() { printf '%s\n' "$*" >&2; exit 1; }

for command in base64 jq tr; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

operation=${1:-}
case "$operation" in
  release|gate|cutover|restore|readback) shift ;;
  -h|--help|'') usage; exit 0 ;;
  *) usage; exit 2 ;;
esac

instance=
artifact_uri=
artifact_sha=
source_revision=
from_controller=
from=
to=
gate_receipt=
gate_sha=
controller=
transition_receipt=
receipt_sha=
preflight_only=false
region=${REGION_ID:-ap-northeast-1}
profile=${ALIYUN_LOCAL_PROFILE:-default}

while (($#)); do
  case $1 in
    --instance) instance=${2:-}; shift 2 ;;
    --uri|--artifact-uri) artifact_uri=${2:-}; shift 2 ;;
    --payload|--artifact-sha256) artifact_sha=${2:-}; shift 2 ;;
    --source|--source-revision) source_revision=${2:-}; shift 2 ;;
    --from-controller) from_controller=${2:-}; shift 2 ;;
    --from) from=${2:-}; shift 2 ;;
    --to) to=${2:-}; shift 2 ;;
    --candidate-controller) to=${2:-}; shift 2 ;;
    --gate-receipt) gate_receipt=${2:-}; shift 2 ;;
    --gate-sha256) gate_sha=${2:-}; shift 2 ;;
    --controller) controller=${2:-}; shift 2 ;;
    --transition-receipt) transition_receipt=${2:-}; shift 2 ;;
    --receipt-sha256) receipt_sha=${2:-}; shift 2 ;;
    --preflight-only)
      [[ $operation == gate ]] || die '--preflight-only is only valid for gate'
      preflight_only=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done

[[ $region == ap-northeast-1 ]] || die 'only the Tokyo region is permitted'
[[ $instance =~ ^i-[a-z0-9]+$ ]] || { usage; exit 2; }
hex64() { [[ ${1:-} =~ ^[A-Fa-f0-9]{64}$ ]]; }
sha40() { [[ ${1:-} =~ ^[A-Fa-f0-9]{40,64}$ ]]; }
remote_path() { [[ ${1:-} =~ ^/[A-Za-z0-9._/@+=:-]+$ ]]; }
normalize_sha() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

profile_args=()
[[ -n $profile ]] && profile_args=(--profile "$profile")

case "$operation" in
  release)
    hex64 "$artifact_sha" || die 'release requires a 64-hex payload digest'
    [[ $artifact_uri =~ ^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$ ]] \
      || die 'release requires a private OSS artifact URI'
    sha40 "$source_revision" || die 'release requires a full source revision'
    artifact_sha=$(normalize_sha "$artifact_sha")
    source_revision=$(normalize_sha "$source_revision")
    REGION_ID="$region" ALIYUN_LOCAL_PROFILE="$profile" \
      exec "$(cd -- "$(dirname -- "$0")" && pwd)/publish-rust-lob-pair-release.sh" \
      --instance "$instance" --artifact-uri "$artifact_uri" \
      --artifact-sha256 "$artifact_sha" --source-revision "$source_revision"
    ;;
  gate)
    if [[ $from_controller != direct ]] && ! hex64 "$from_controller"; then
      die 'gate requires --from-controller <64 hex> or direct'
    fi
    hex64 "$to" || die 'gate requires --candidate-controller <64 hex>'
    controller=${to,,}; from_controller=${from_controller,,}
    host_script=host-rust-lob-shadow-gate.sh
    if [[ $preflight_only == true ]]; then
      command_name=monday-rust-lob-shadow-preflight
      timeout_seconds=300
    else
      command_name=monday-rust-lob-shadow-gate
      timeout_seconds=3600
    fi
    remote_args=(--from-controller "$from_controller" --candidate-controller "$controller")
    [[ $preflight_only == true ]] && remote_args+=(--preflight-only)
    ;;
  cutover)
    if [[ $from != direct ]] && ! hex64 "$from"; then
      die 'cutover requires --from <64 hex> or direct'
    fi
    hex64 "$to" || die 'cutover requires --to <64 hex>'
    remote_path "$gate_receipt" || die 'cutover requires an absolute gate receipt path'
    hex64 "$gate_sha" || die 'cutover requires a 64-hex gate receipt digest'
    from=${from,,}; to=${to,,}; controller=$to; gate_sha=${gate_sha,,}
    host_script=host-rust-lob-cutover.sh
    command_name=monday-rust-lob-cutover
    timeout_seconds=3600
    remote_args=(--from "$from" --to "$to" --gate-receipt "$gate_receipt" --gate-sha256 "$gate_sha")
    ;;
  restore)
    hex64 "$controller" || die 'restore requires --controller <64 hex>'
    controller=${controller,,}
    host_script=host-rust-lob-restore.sh
    command_name=monday-rust-lob-restore
    timeout_seconds=3600
    remote_args=(--controller "$controller")
    ;;
  readback)
    hex64 "$controller" || die 'readback requires --controller <64 hex>'
    remote_path "$transition_receipt" || die 'readback requires an absolute transition receipt path'
    hex64 "$receipt_sha" || die 'readback requires a 64-hex transition receipt digest'
    controller=${controller,,}; receipt_sha=${receipt_sha,,}
    host_script=host-rust-lob-readback.sh
    command_name=monday-rust-lob-readback
    timeout_seconds=1800
    remote_args=(--controller "$controller" --transition-receipt "$transition_receipt" --receipt-sha256 "$receipt_sha")
    ;;
esac

controller_path="/opt/monday/releases/binance-lob-controller/$controller/deployment/$host_script"
printf -v remote_script 'set -Eeuo pipefail\nexec env -i HOME=/root LC_ALL=C PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin %q' "$controller_path"
for arg in "${remote_args[@]}"; do
  printf -v escaped ' %q' "$arg"
  remote_script+=$escaped
done
command_content=$(printf '%s\n' "$remote_script" | base64 | tr -d '\n')

if [[ ${MONDAY_CONTROL_PLANE_DRY_RUN:-0} == 1 ]]; then
  jq -cn --arg operation "$operation" --arg instance "$instance" \
    --arg controller "$controller" --arg command "$remote_script" \
    --argjson preflight_only "$preflight_only" \
    '{operation:$operation,instance:$instance,controller:$controller,
      command:$command,preflight_only:$preflight_only,
      production_changed:($operation == "cutover" or $operation == "restore")}'
  exit 0
fi

for command in aliyun seq sleep; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

run_json=$(aliyun ecs RunCommand --RegionId "$region" --InstanceId.1 "$instance" \
  --Type RunShellScript --ContentEncoding Base64 --CommandContent "$command_content" \
  --KeepCommand false --Name "$command_name" --Timeout "$timeout_seconds" \
  "${profile_args[@]}")
invoke_id=$(printf '%s' "$run_json" | jq -er '.InvokeId')
printf 'Cloud Assistant invocation: %s (%s)\n' "$invoke_id" "$operation"

polls=$((timeout_seconds / 5))
if [[ -n ${MONDAY_OPERATION_TEST_POLLS:-} ]]; then
  [[ ${MONDAY_ALLOW_SHORT_OPERATION_TEST:-0} == 1 ]] || die 'short polling requires MONDAY_ALLOW_SHORT_OPERATION_TEST=1'
  polls=${MONDAY_OPERATION_TEST_POLLS}
fi
[[ $polls =~ ^[1-9][0-9]*$ ]] || die 'poll count must be a positive integer'

for ((poll = 1; poll <= polls; poll++)); do
  result_json=$(aliyun ecs DescribeInvocationResults --RegionId "$region" \
    --InvokeId "$invoke_id" --InstanceId "$instance" "${profile_args[@]}" 2>/dev/null || true)
  status=$(printf '%s' "$result_json" | jq -r '[.. | objects | .InvocationStatus? // empty][0] // empty' 2>/dev/null || true)
  exit_code=$(printf '%s' "$result_json" | jq -r '[.. | objects | .ExitCode? // empty][0] // empty' 2>/dev/null || true)
  case "$status" in
    Success|Finished)
      output=$(printf '%s' "$result_json" | jq -r '[.. | objects | .Output? // empty][0] // empty')
      if [[ -n $output ]]; then
        printf '%s' "$output" | base64 --decode || true
        printf '\n'
      fi
      [[ $exit_code == 0 ]] || die "$operation failed: $invoke_id"
      printf '%s completed successfully: %s\n' "$operation" "$invoke_id"
      exit 0
      ;;
    Failed|Stopped|PartialFailed|Timeout)
      printf '%s\n' "$result_json" >&2
      die "$operation reached terminal state: $status"
      ;;
  esac
  sleep 5
done

printf 'timed out locally; stopping invocation %s\n' "$invoke_id" >&2
aliyun ecs StopInvocation --RegionId "$region" --InvokeId "$invoke_id" \
  --InstanceId.1 "$instance" "${profile_args[@]}" >/dev/null 2>&1 || true
die "operation did not reach a terminal state: $invoke_id"
