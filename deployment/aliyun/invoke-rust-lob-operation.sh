#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf '%s\n' \
    'Usage: ACTION=gate-preflight|gate|controller-apply|cutover|restore INSTANCE_ID=i-... ARTIFACT_SHA256=<64 hex> invoke-rust-lob-operation.sh' \
    '' \
    'gate-preflight, gate, cutover, and restore optionally accept CONTROLLER_RELEASE_SHA256=<64 hex>.' \
    'controller-apply requires CONTROLLER_RELEASE_SHA256=<64 hex>.' \
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
CONTROLLER_RELEASE_SHA256=${CONTROLLER_RELEASE_SHA256:-}
if [[ -n $CONTROLLER_RELEASE_SHA256 ]]; then
  [[ $CONTROLLER_RELEASE_SHA256 =~ ^[A-Fa-f0-9]{64}$ ]] \
    || { printf 'CONTROLLER_RELEASE_SHA256 must contain exactly 64 hexadecimal characters\n' >&2; exit 2; }
  CONTROLLER_RELEASE_SHA256=$(printf '%s' "$CONTROLLER_RELEASE_SHA256" \
    | tr '[:upper:]' '[:lower:]')
fi

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
  controller-apply)
    host_script=host-rust-lob-controller-apply.sh
    timeout_seconds=600
    command_name=monday-rust-lob-controller-apply
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

case "$ACTION" in
  controller-apply)
    [[ -n $CONTROLLER_RELEASE_SHA256 ]] \
      || { printf '%s requires CONTROLLER_RELEASE_SHA256\n' "$ACTION" >&2; exit 2; }
    ;;
esac

controller_dispatch=false
case "$ACTION" in
  gate-preflight|gate|cutover|restore)
    if [[ -n $CONTROLLER_RELEASE_SHA256 ]]; then
      controller_dispatch=true
    else
      printf 'deprecated: ACTION=%s without CONTROLLER_RELEASE_SHA256 uses artifact-release controller bytes; pin an applied controller release\n' \
        "$ACTION" >&2
    fi
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

if [[ $ACTION == controller-apply ]]; then
  host_path="/opt/monday/releases/binance-lob-controller/$CONTROLLER_RELEASE_SHA256/deployment/$host_script"
  printf -v remote_script '#!/usr/bin/env bash\nset -euo pipefail\nexec %q %q %q\n' \
    "$host_path" "$CONTROLLER_RELEASE_SHA256" "$ARTIFACT_SHA256"
elif [[ $controller_dispatch == true ]]; then
  artifact_release="/opt/monday/releases/binance-lob-archiver/$ARTIFACT_SHA256"
  controller_root=/opt/monday/releases/binance-lob-controller
  controller_release="$controller_root/$CONTROLLER_RELEASE_SHA256"
  controller_deployment="$controller_release/deployment"
  printf -v remote_variables \
    'artifact_sha=%q\nartifact_release=%q\ncontroller_sha=%q\ncontroller_root=%q\ncontroller_release=%q\ncontroller_deployment=%q\nhost_script=%q\n' \
    "$ARTIFACT_SHA256" "$artifact_release" "$CONTROLLER_RELEASE_SHA256" \
    "$controller_root" "$controller_release" "$controller_deployment" "$host_script"
  remote_script=$'#!/usr/bin/env bash\nset -euo pipefail\nexport LC_ALL=C\n'
  remote_script+="$remote_variables"
  remote_script+=$'\nregular_file() {\n  [[ -f $1 && ! -L $1 ]]\n}\n\ndirect_directory() {\n  local path=$1 resolved\n  [[ -d $path && ! -L $path ]] || return 1\n  resolved=$(readlink -f -- "$path") || return 1\n  [[ $resolved == "$path" ]]\n}\n\ndie() {\n  printf \'controller dispatcher failed: %s\\n\' "$*" >&2\n  exit 1\n}\n\ndirect_directory "$artifact_release" || die "artifact release path is missing or indirect"\ndirect_directory "$artifact_release/deployment" || die "artifact deployment path is missing or indirect"\nartifact_manifest="$artifact_release/release.json"\nregular_file "$artifact_manifest" || die "artifact release manifest is missing or indirect"\njq -e --arg artifact "$artifact_sha" \'\n  .artifact_sha256 == $artifact\n  and (.runtime_contract_sha256 | type == "string")\n  and (.runtime_contract_sha256 | test("^[a-f0-9]{64}$"))\' \\\n  "$artifact_manifest" >/dev/null || die "artifact release identity is invalid"\nruntime_contract=$(jq -er \'.runtime_contract_sha256\' "$artifact_manifest") \\\n  || die "artifact release runtime identity is missing"\n\ndirect_directory "$controller_root" || die "controller release root is missing or indirect"\ndirect_directory "$controller_release" || die "controller release path is missing or indirect"\ndirect_directory "$controller_deployment" || die "controller deployment path is missing or indirect"\ncontroller_manifest="$controller_release/release.json"\nregular_file "$controller_manifest" || die "controller release manifest is missing or indirect"\nregular_file "$controller_release/release.json.sha256" || die "controller manifest checksum is missing or indirect"\nregular_file "$controller_release/deployment.sha256" || die "controller deployment checksum is missing or indirect"\n[[ $(sha256sum "$controller_manifest" | awk \'{print $1}\') == "$controller_sha" ]] \\\n  || die "controller release manifest digest mismatch"\n(cd "$controller_release" \\\n  && sha256sum --check --strict release.json.sha256 >/dev/null \\\n  && sha256sum --check --strict deployment.sha256 >/dev/null) \\\n  || die "controller release checksum verification failed"\njq -e --arg artifact "$artifact_sha" --arg runtime "$runtime_contract" \'\n  .schema == "monday.rust_lob_controller_release.v1"\n  and .artifact_sha256 == $artifact\n  and .runtime_contract_sha256 == $runtime\n  and (.deployment_bundle_sha256 | type == "string" and test("^[a-f0-9]{64}$"))\n  and (.deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))\' \\\n  "$controller_manifest" >/dev/null || die "controller release identity is invalid"\n\nregular_file "$controller_deployment/rust-lob-control-plane-lib.sh" \\\n  || die "controller deployment helper is missing or indirect"\n# shellcheck disable=SC1090,SC1091\n. "$controller_deployment/rust-lob-control-plane-lib.sh"\nactive_deployment=$(monday_rust_lob_active_controller_deployment \\\n  "$controller_root" "$artifact_sha" "$runtime_contract") \\\n  || die "active controller deployment is invalid"\n[[ $active_deployment == "$controller_deployment" ]] \\\n  || die "active controller deployment differs from requested controller release"\ntarget_host_script="$controller_deployment/$host_script"\nregular_file "$target_host_script" && [[ -x $target_host_script ]] \\\n  || die "controller host script is missing, indirect, or not executable"\n'
  if [[ $ACTION == gate-preflight ]]; then
    remote_script+=$(printf 'exec %q --resource-preflight %q\n' \
      "$controller_deployment/$host_script" "$ARTIFACT_SHA256")
  else
    remote_script+=$(printf 'exec %q %q\n' \
      "$controller_deployment/$host_script" "$ARTIFACT_SHA256")
  fi
else
  host_path="/opt/monday/releases/binance-lob-archiver/$ARTIFACT_SHA256/deployment/$host_script"
  if [[ $ACTION == gate-preflight ]]; then
    printf -v remote_script '#!/usr/bin/env bash\nset -euo pipefail\nexec %q --resource-preflight %q\n' \
      "$host_path" "$ARTIFACT_SHA256"
  else
    printf -v remote_script '#!/usr/bin/env bash\nset -euo pipefail\nexec %q %q\n' \
      "$host_path" "$ARTIFACT_SHA256"
  fi
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
            and (.io_full_psi_windows | type) == "array"
            and (.io_full_psi_windows | length) == 3
            and all(.io_full_psi_windows[];
              .phase == "resource-preflight"
              and .phase_run == 1
              and .stage == "calibration"
              and (.started_at | type) == "string"
              and (.started_at
                | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
              and (.finished_at | type) == "string"
              and (.finished_at
                | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
              and .finished_at >= .started_at
              and (.previous_total_us | type) == "number"
              and .previous_total_us == (.previous_total_us | floor)
              and .previous_total_us >= 0
              and (.current_total_us | type) == "number"
              and .current_total_us == (.current_total_us | floor)
              and .current_total_us >= .previous_total_us
              and .delta_us == (.current_total_us - .previous_total_us)
              and (.window_us | type) == "number"
              and .window_us == (.window_us | floor) and .window_us > 0
              and (.ratio | type) == "number" and .ratio >= 0
              and (((.ratio - (.delta_us / .window_us)) as $difference
                | (if $difference < 0 then -$difference else $difference end))
                  <= 0.000000001)
              and .hit == ((.delta_us / .window_us) >= (150000 / 15000000))
              and (.consecutive_hits | type) == "number"
              and .consecutive_hits == (.consecutive_hits | floor)
              and .consecutive_hits >= 0 and .consecutive_hits < 3)
            and (.io_full_psi_windows as $psi
              | reduce range(0; 3) as $index
                ({hits:0,current:null,valid:true};
                  ($psi[$index]) as $window
                  | (if $window.hit then (.hits + 1) else 0 end) as $expected_hits
                  | .valid = (.valid
                      and $window.consecutive_hits == $expected_hits
                      and (if $index > 0 then
                        $window.previous_total_us == .current
                      else true end))
                  | .hits = $expected_hits
                  | .current = $window.current_total_us)
              | .valid)
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
