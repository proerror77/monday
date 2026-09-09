#!/usr/bin/env bash
set -Eeuo pipefail

controller_stage="preflight"
current_generation="unknown"

log_event() {
  local event="$1"
  shift
  printf 'schema_version=monday.research_event.v1 component=campaign-cycle-controller event=%s' "$event" >&2
  printf ' %s' "$@" >&2
  printf '\n' >&2
}

die() {
  log_event cycle_failed "generation=$current_generation" "stage=$controller_stage" "reason=$*"
  echo "campaign-cycle-controller: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: campaign-cycle-controller.sh [start] \
  (--campaign-inputs FILE --input-root DIR | --fresh-inputs \
   --fresh-raw-root DIR --fresh-reference-root DIR \
   --fresh-symbol SYMBOL --fresh-image-ref IMAGE@sha256:DIGEST \
   --fresh-mission-id ID --fresh-output-root DIR \
   --fresh-output-prefix PREFIX --fresh-materializer FILE \
   --fresh-bucket-ms N --fresh-label-horizon-buckets N --fresh-top-depth N \
   (--fresh-start-received-at-ns N --fresh-end-received-at-ns N \
    | --fresh-duration-ns N [--fresh-cutoff-received-at-ns N] --fresh-max-candidates N) \
   --fresh-max-inputs N --fresh-max-input-bytes N \
   --fresh-max-scan-entries N --fresh-materializer-timeout-seconds N \
   --fresh-max-materializer-output-bytes N) \
  --source-revision SHA \
  --image IMAGE@sha256:DIGEST --campaign-root HTTPS_URL \
  [--control CONTROL_JSON] [--signer EXECUTABLE] \
  --work-dir DIR --seed N --seed N \
  [--context NAME] [--namespace NAME] [--max-follow-ups 3] \
  [--job-timeout 7h]

       campaign-cycle-controller.sh approve \
  --work-dir DIR --signer EXECUTABLE [--control CONTROL_JSON] \
  [--alpha-harness EXECUTABLE] [--aliyun EXECUTABLE] [--kubectl EXECUTABLE]

       campaign-cycle-controller.sh ack-readback \
  --work-dir DIR --campaign-pod-name NAME \
  [--alpha-harness EXECUTABLE] [--aliyun EXECUTABLE] [--kubectl EXECUTABLE]

       campaign-cycle-controller.sh status --work-dir DIR

The signer is a separate trust boundary. It is invoked as:
  SIGNER --freeze FREEZE_JSON --output SIGNED_REQUEST_JSON
It must sign every action in the frozen signing plan without changing the
query-free object identity, HTTP method, content type, or required headers.
EOF
}

print_ack_controller_job() {
  local campaign_id="$1"
  local request_sha256="$2"
  local controller_work_dir="$3"
  local script_dir job_template cycle_id
  script_dir="$(cd "$(dirname "$0")" && pwd -P)"
  job_template="$script_dir/../k8s/campaign-cycle-controller-job.example.yaml"
  [[ -s "$job_template" ]] || die "ACK controller Job template is missing: $job_template"
  cycle_id="${controller_work_dir##*/}"
  [[ "$cycle_id" =~ ^[A-Za-z0-9._-]+$ ]] \
    || die "work directory basename is unsafe for an ACK Job handoff: $cycle_id"
  sed \
    -e "s/REPLACE_CONTROLLER_JOB_NAME/campaign-cycle-${request_sha256:0:16}/g" \
    -e "s/REPLACE_CAMPAIGN_ID/$campaign_id/g" \
    -e "s/REPLACE_REQUEST_SHA256/$request_sha256/g" \
    -e "s/REPLACE_CYCLE_ID/$cycle_id/g" \
    "$job_template"
}

validate_controller_state() {
  local controller_state="$1"
  jq -e '
    (.campaign_inputs | type == "string")
    and (.campaign_inputs_sha256 | type == "string")
    and (.input_root | type == "string")
    and (.source_revision | type == "string")
    and (.image | type == "string")
    and (.campaign_root | type == "string")
    and (.context | type == "string")
    and (.namespace | type == "string")
    and (.max_follow_ups | type == "number")
    and (.job_timeout | type == "string")
    and (.seeds | type == "array" and length >= 2)
    and all(.seeds[]; type == "number")
    and ((.input_mode // "receipt") == "receipt"
      or ((.input_mode == "fresh") and (.fresh | type == "object")))
  ' "$controller_state" >/dev/null || die "controller state is invalid: $controller_state"
}

sha256_file() {
  if command -v shasum >/dev/null; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

build_fresh_controller_contract() {
  seeds_json=$(printf '%s\n' "${seeds[@]}" | jq -R 'tonumber' | jq -s '.')
  fresh_state_json=$(jq -n \
    --arg raw_root "$fresh_raw_root" \
    --arg reference_root "$fresh_reference_root" \
    --arg start_received_at_ns "$fresh_start_received_at_ns" \
    --arg end_received_at_ns "$fresh_end_received_at_ns" \
    --arg symbol "$fresh_symbol" \
    --arg image_ref "$fresh_image_ref" \
    --arg mission_id "$fresh_mission_id" \
    --arg output_root "$fresh_output_root" \
    --arg output_prefix "$fresh_output_prefix" \
    --arg bucket_ms "$fresh_bucket_ms" \
    --arg label_horizon_buckets "$fresh_label_horizon_buckets" \
    --arg top_depth "$fresh_top_depth" \
    --arg duration_ns "$fresh_duration_ns" \
    --arg cutoff_received_at_ns "$fresh_cutoff_received_at_ns" \
    --arg max_candidates "$fresh_max_candidates" \
    --arg materializer "$fresh_materializer" \
    --arg binary_dir "$fresh_binary_dir" \
    --arg max_scan_entries "$fresh_max_scan_entries" \
    --arg max_inputs "$fresh_max_inputs" \
    --arg max_input_bytes "$fresh_max_input_bytes" \
    --arg materializer_timeout_seconds "$fresh_materializer_timeout_seconds" \
    --arg max_materializer_output_bytes "$fresh_max_materializer_output_bytes" \
    --arg inventory_out "$fresh_inventory_out" \
    --arg request_out "$fresh_request_out" \
    --arg materializer_work_dir "$fresh_materializer_work_dir" \
    --arg preparation_report "$fresh_report_out" \
    '{
      raw_root:$raw_root,reference_root:$reference_root,
      start_received_at_ns:$start_received_at_ns,end_received_at_ns:$end_received_at_ns,
      symbol:$symbol,image_ref:$image_ref,mission_id:$mission_id,
      output_root:$output_root,output_prefix:$output_prefix,
      bucket_ms:$bucket_ms,label_horizon_buckets:$label_horizon_buckets,top_depth:$top_depth,
      duration_ns:$duration_ns,cutoff_received_at_ns:$cutoff_received_at_ns,
      max_candidates:$max_candidates,
      materializer:$materializer,binary_dir:$binary_dir,
      max_scan_entries:$max_scan_entries,max_inputs:$max_inputs,max_input_bytes:$max_input_bytes,
      materializer_timeout_seconds:$materializer_timeout_seconds,
      max_materializer_output_bytes:$max_materializer_output_bytes,
      inventory_out:$inventory_out,request_out:$request_out,
      materializer_work_dir:$materializer_work_dir,
      preparation_report:$preparation_report
    }')
}

validate_fresh_controller_owner() {
  [[ "$mode" == "start" && "$fresh_mode" == true ]] || return 0
  state="$work_dir/controller-inputs.json"
  build_fresh_controller_contract
  if [[ -e "$state" ]]; then
    [[ ! -L "$state" ]] || die "existing controller state is a symbolic link: $state"
    [[ -s "$state" ]] || die "existing controller state is empty: $state"
    validate_controller_state "$state"
    jq -e \
      --arg campaign_inputs "$campaign_inputs" \
      --arg input_root "$input_root" \
      --arg source_revision "$source_revision" \
      --arg image "$image" \
      --arg campaign_root "$campaign_root" \
      --arg context "$context" \
      --arg namespace "$namespace" \
      --arg job_timeout "$job_timeout" \
      --argjson max_follow_ups "$max_follow_ups" \
      --argjson seeds "$seeds_json" \
      --argjson fresh "$fresh_state_json" \
      '.input_mode == "fresh"
       and .campaign_inputs == $campaign_inputs
       and .input_root == $input_root
       and .source_revision == $source_revision
       and .image == $image
       and .campaign_root == $campaign_root
       and .context == $context
       and .namespace == $namespace
       and .max_follow_ups == $max_follow_ups
       and .job_timeout == $job_timeout
       and .seeds == $seeds
       and .fresh == $fresh' \
      "$state" >/dev/null \
      || die "existing work directory belongs to different fresh controller inputs"
    [[ ! -L "$campaign_inputs" && -s "$campaign_inputs" ]] \
      || die "existing fresh campaign inputs are missing or a symbolic link"
    existing_campaign_inputs_sha256="$(jq -er '.campaign_inputs_sha256' "$state")" \
      || die "existing fresh controller state has no campaign inputs SHA256"
    [[ "$(sha256_file "$campaign_inputs")" == "$existing_campaign_inputs_sha256" ]] \
      || die "existing fresh campaign inputs changed after controller state was recorded"
  fi
}

# Persist the first complete learn report before publishing anything. A resumed
# campaign-learn may return different execution diagnostics for the same plan.
validate_learning_checkpoint() {
  local dir="$1" expected_request="$2" expected_result="$3"
  local checkpoint="$dir/learning-checkpoint.json"
  local report_sha outcome plan_sha=""
  [[ -s "$checkpoint" && -s "$dir/learn-report.json" ]] || return 1
  report_sha="$(sha256_file "$dir/learn-report.json")" || return 1
  outcome="$(jq -er '.outcome' "$dir/learn-report.json")" || return 1
  case "$outcome" in
    follow_up)
      [[ -s "$dir/next-research-plan.json" ]] || return 1
      plan_sha="$(sha256_file "$dir/next-research-plan.json")" || return 1
      ;;
    no_improvement) [[ ! -e "$dir/next-research-plan.json" ]] || return 1 ;;
    *) return 1 ;;
  esac
  jq -e --arg request "$expected_request" --arg result "$expected_result" \
    --arg report "$report_sha" --arg plan "$plan_sha" --arg outcome "$outcome" '
    .schema_version == "monday.campaign_learning_checkpoint.v1"
    and .request_sha256 == $request and .campaign_result_sha256 == $result
    and .learn_report_sha256 == $report and .research_plan_file_sha256 == $plan
    and .outcome == $outcome
  ' "$checkpoint" >/dev/null
}

validate_generation_completion() {
  local dir="$1" expected_generation="$2"
  local checkpoint="$dir/generation-complete"
  local report_sha expected_request expected_result learning_sha="" learned_outcome="" learn_report_sha=""
  local campaign_root max_follow_ups
  [[ -s "$checkpoint" && -s "$dir/generation-report.json" \
    && -s "$dir/finalize-report.json" && -s "$dir/campaign-result.json" ]] || return 1
  campaign_root="$(jq -er '.campaign_root' "$dir/../controller-inputs.json")" || return 1
  max_follow_ups="$(jq -er '.max_follow_ups' "$dir/../controller-inputs.json")" || return 1
  report_sha="$(sha256_file "$dir/generation-report.json")" || return 1
  expected_request="$(jq -er '.request_sha256' "$dir/finalize-report.json")" || return 1
  expected_result="$(sha256_file "$dir/campaign-result.json")" || return 1
  if [[ -e "$dir/learning-checkpoint.json" ]]; then
    validate_learning_checkpoint "$dir" "$expected_request" "$expected_result" || return 1
    learning_sha="$(sha256_file "$dir/learning-checkpoint.json")" || return 1
    learned_outcome="$(jq -er '.outcome' "$dir/learning-checkpoint.json")" || return 1
    learn_report_sha="$(sha256_file "$dir/learn-report.json")" || return 1
    cmp -s "$dir/learn-report.json" "$dir/learn-report-readback.json" || return 1
    if [[ "$(jq -er '.outcome' "$dir/learning-checkpoint.json")" == follow_up ]]; then
      cmp -s "$dir/next-research-plan.json" "$dir/next-research-plan-readback.json" || return 1
    fi
  fi
  jq -e --slurpfile report "$dir/generation-report.json" \
    --argjson generation "$expected_generation" --arg report_sha "$report_sha" \
    --arg request "$expected_request" --arg result "$expected_result" \
    --arg learning "$learning_sha" --arg learned_outcome "$learned_outcome" \
    --arg learn_report_sha "$learn_report_sha" --arg campaign_root "$campaign_root" \
    --argjson max_follow_ups "$max_follow_ups" '
    .schema_version == "monday.campaign_generation_completion.v1"
    and .generation == $generation and $report[0].generation == $generation
    and .generation_report_sha256 == $report_sha
    and .learning_checkpoint_sha256 == $learning
    and $report[0].request_sha256 == $request
    and $report[0].campaign_result_sha256 == $result
    and (.campaign_pod_name | type == "string" and length > 0)
    and (if .outcome == "follow_up" then
      $learned_outcome == "follow_up" and .cycle_result == null
    elif .outcome == "complete" then
      if $learned_outcome == "no_improvement" then
        .cycle_result == ($report[0] + {
          termination_reason:"no_improvement",learning_outcome:"no_improvement",
          learn_report_url:($campaign_root + "/campaign-id=" + $report[0].campaign_id + "/learning/generation=" + ($generation | tostring) + "/learn-report.json"),
          learn_report_sha256:$learn_report_sha
        })
      elif $learning == "" and $report[0].termination_reason != "campaign_no_candidate" then
        .cycle_result == $report[0]
      elif $learning == "" and $generation == $max_follow_ups then
        .cycle_result == ($report[0] + {bounded_loop_exhausted:true})
      else false end
    else false end)
  ' "$checkpoint" >/dev/null
}

cycle_status() {
  local status_dir="$1"
  local status_state="$status_dir/controller-inputs.json"
  local generation_dir candidate
  local generation=-1
  local checkpoint_status="incomplete"
  local next_stage="freeze"
  local campaign_id=""
  local request_sha256=""
  local job_name=""
  local termination_reason=""

  [[ -d "$status_dir" ]] || die "work directory does not exist: $status_dir"
  [[ -s "$status_state" ]] || die "controller state is missing: $status_state"
  validate_controller_state "$status_state"

  for generation_dir in "$status_dir"/generation-*; do
    [[ -d "$generation_dir" ]] || continue
    candidate="${generation_dir##*/generation-}"
    [[ "$candidate" =~ ^[0-9]+$ ]] || continue
    if ((candidate > generation)); then
      generation="$candidate"
    fi
  done
  if ((generation < 0)); then
    generation=0
    generation_dir="$status_dir/generation-0"
  else
    generation_dir="$status_dir/generation-$generation"
  fi

  if [[ -e "$generation_dir/terminal-failure" ]]; then
    checkpoint_status="terminal_failure"
    next_stage=""
  elif [[ -e "$generation_dir/generation-complete" ]]; then
    validate_generation_completion "$generation_dir" "$generation" \
      || die "saved Campaign completion checkpoint is invalid or unsupported"
    if [[ "$(jq -er '.outcome' "$generation_dir/generation-complete")" == complete ]]; then
      checkpoint_status="complete"
      next_stage=""
      termination_reason="$(jq -er '.cycle_result.termination_reason' "$generation_dir/generation-complete")"
    else
      next_stage="next_generation"
    fi
  elif [[ -e "$generation_dir/result-readback-complete" ]]; then
    if [[ -s "$generation_dir/campaign-result.json" ]]; then
      termination_reason="$(jq -r '.termination_reason // empty' "$generation_dir/campaign-result.json")"
    fi
    if [[ ! -s "$generation_dir/settlement-report.json" ]] \
      || ! jq -e '.status == "settled"' "$generation_dir/settlement-report.json" >/dev/null 2>&1; then
      next_stage="ledger_settlement"
    elif [[ "$termination_reason" == "campaign_no_candidate" ]]; then
      next_stage="campaign_learning"
    else
      next_stage="finalize_cycle"
    fi
  elif [[ -e "$generation_dir/provenance-readback-complete" ]]; then
    next_stage="oss_result_readback"
  elif [[ -e "$generation_dir/dispatched" ]]; then
    next_stage="kubernetes_runtime_readback"
  elif [[ -e "$generation_dir/finalized" ]]; then
    next_stage="dispatch"
  elif [[ -e "$generation_dir/frozen" ]]; then
    next_stage="sign_finalize"
  fi

  if [[ -s "$generation_dir/finalize-report.json" ]]; then
    campaign_id="$(jq -r '.campaign_id // empty' "$generation_dir/finalize-report.json")"
    request_sha256="$(jq -r '.request_sha256 // empty' "$generation_dir/finalize-report.json")"
    job_name="$(jq -r '.job_name // empty' "$generation_dir/finalize-report.json")"
  fi
  if [[ -z "$termination_reason" && -s "$generation_dir/campaign-result.json" ]]; then
    termination_reason="$(jq -r '.termination_reason // empty' "$generation_dir/campaign-result.json")"
  fi

  jq -n \
    --slurpfile inputs "$status_state" \
    --arg checkpoint_status "$checkpoint_status" \
    --argjson generation "$generation" \
    --arg next_stage "$next_stage" \
    --arg campaign_id "$campaign_id" \
    --arg request_sha256 "$request_sha256" \
    --arg job_name "$job_name" \
    --arg termination_reason "$termination_reason" '{
      schema_version:"monday.campaign_cycle_status.v1",
      checkpoint_status:$checkpoint_status,
      generation:$generation,
      next_stage:(if $next_stage == "" then null else $next_stage end),
      campaign_inputs_sha256:$inputs[0].campaign_inputs_sha256,
      source_revision:$inputs[0].source_revision,
      image:$inputs[0].image,
      campaign_id:(if $campaign_id == "" then null else $campaign_id end),
      request_sha256:(if $request_sha256 == "" then null else $request_sha256 end),
      job_name:(if $job_name == "" then null else $job_name end),
      termination_reason:(if $termination_reason == "" then null else $termination_reason end)
    }'
}

alpha_harness="alpha-harness"
aliyun_cli="aliyun"
kubectl_cli="kubectl"
context="monday-research-apne1"
namespace="monday-research"
max_follow_ups=3
job_timeout="7h"
campaign_inputs=""
input_root=""
source_revision=""
image=""
campaign_root=""
signer=""
control=""
campaign_pod_name=""
work_dir=""
seeds=()
mode="start"
fresh_mode=false
fresh_raw_root=""
fresh_reference_root=""
fresh_start_received_at_ns=""
fresh_end_received_at_ns=""
fresh_symbol=""
fresh_image_ref=""
fresh_mission_id=""
fresh_output_root=""
fresh_output_prefix=""
fresh_bucket_ms=""
fresh_label_horizon_buckets=""
fresh_top_depth=""
fresh_duration_ns=""
fresh_cutoff_received_at_ns=""
fresh_max_candidates=""
fresh_materializer=""
fresh_binary_dir=""
fresh_max_scan_entries=""
fresh_max_inputs=""
fresh_max_input_bytes=""
fresh_materializer_timeout_seconds=""
fresh_max_materializer_output_bytes=""
fresh_inventory_out=""
fresh_request_out=""
fresh_materializer_work_dir=""
fresh_report_out=""

case "${1:-}" in
  start|approve|ack-readback|status)
    mode="$1"
    shift
    ;;
esac

if [[ "$mode" == "status" ]]; then
  while (($#)); do
    case "$1" in
      --work-dir) work_dir="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) die "status accepts only --work-dir" ;;
    esac
  done
  [[ -n "$work_dir" ]] || die "--work-dir is required"
  command -v jq >/dev/null || die "jq is required"
  cycle_status "$work_dir"
  exit 0
fi

while (($#)); do
  case "$1" in
    --alpha-harness) alpha_harness="$2"; shift 2 ;;
    --aliyun) aliyun_cli="$2"; shift 2 ;;
    --kubectl) kubectl_cli="$2"; shift 2 ;;
    --campaign-inputs) [[ "$mode" == "start" ]] || die "$mode loads --campaign-inputs from controller state"; campaign_inputs="$2"; shift 2 ;;
    --fresh-inputs) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_mode=true; shift ;;
    --fresh-raw-root) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_raw_root="$2"; shift 2 ;;
    --fresh-reference-root) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_reference_root="$2"; shift 2 ;;
    --fresh-start-received-at-ns|--fresh-window-start-ns) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_start_received_at_ns="$2"; shift 2 ;;
    --fresh-end-received-at-ns|--fresh-window-end-ns) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_end_received_at_ns="$2"; shift 2 ;;
    --fresh-symbol) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_symbol="$2"; shift 2 ;;
    --fresh-image-ref|--fresh-producer-image) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_image_ref="$2"; shift 2 ;;
    --fresh-mission-id) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_mission_id="$2"; shift 2 ;;
    --fresh-output-root) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_output_root="$2"; shift 2 ;;
    --fresh-output-prefix) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_output_prefix="$2"; shift 2 ;;
    --fresh-bucket-ms) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_bucket_ms="$2"; shift 2 ;;
    --fresh-label-horizon-buckets) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_label_horizon_buckets="$2"; shift 2 ;;
    --fresh-top-depth) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_top_depth="$2"; shift 2 ;;
    --fresh-duration-ns) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_duration_ns="$2"; shift 2 ;;
    --fresh-cutoff-received-at-ns|--fresh-cutoff-ns) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_cutoff_received_at_ns="$2"; shift 2 ;;
    --fresh-max-candidates) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_max_candidates="$2"; shift 2 ;;
    --fresh-materializer) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_materializer="$2"; shift 2 ;;
    --fresh-binary-dir) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_binary_dir="$2"; shift 2 ;;
    --fresh-max-scan-entries) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_max_scan_entries="$2"; shift 2 ;;
    --fresh-max-inputs) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_max_inputs="$2"; shift 2 ;;
    --fresh-max-input-bytes) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_max_input_bytes="$2"; shift 2 ;;
    --fresh-materializer-timeout-seconds) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_materializer_timeout_seconds="$2"; shift 2 ;;
    --fresh-max-materializer-output-bytes) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_max_materializer_output_bytes="$2"; shift 2 ;;
    --fresh-inventory-out) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_inventory_out="$2"; shift 2 ;;
    --fresh-request-out) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_request_out="$2"; shift 2 ;;
    --fresh-materializer-work-dir) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_materializer_work_dir="$2"; shift 2 ;;
    --fresh-report-out) [[ "$mode" == "start" ]] || die "$mode loads fresh inputs from controller state"; fresh_report_out="$2"; shift 2 ;;
    --input-root) [[ "$mode" == "start" ]] || die "$mode loads --input-root from controller state"; input_root="$2"; shift 2 ;;
    --source-revision) [[ "$mode" == "start" ]] || die "$mode loads --source-revision from controller state"; source_revision="$2"; shift 2 ;;
    --image) [[ "$mode" == "start" ]] || die "$mode loads --image from controller state"; image="$2"; shift 2 ;;
    --campaign-root) [[ "$mode" == "start" ]] || die "$mode loads --campaign-root from controller state"; campaign_root="$2"; shift 2 ;;
    --signer) signer="$2"; shift 2 ;;
    --control) control="$2"; shift 2 ;;
    --campaign-pod-name) [[ "$mode" == "ack-readback" ]] || die "--campaign-pod-name is ACK-only"; campaign_pod_name="$2"; shift 2 ;;
    --work-dir) work_dir="$2"; shift 2 ;;
    --seed) [[ "$mode" == "start" ]] || die "$mode loads --seed from controller state"; seeds+=("$2"); shift 2 ;;
    --context) [[ "$mode" == "start" ]] || die "$mode loads --context from controller state"; context="$2"; shift 2 ;;
    --namespace) [[ "$mode" == "start" ]] || die "$mode loads --namespace from controller state"; namespace="$2"; shift 2 ;;
    --max-follow-ups) [[ "$mode" == "start" ]] || die "$mode loads --max-follow-ups from controller state"; max_follow_ups="$2"; shift 2 ;;
    --job-timeout) [[ "$mode" == "start" ]] || die "$mode loads --job-timeout from controller state"; job_timeout="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

if [[ "$mode" == "approve" || "$mode" == "ack-readback" ]]; then
  [[ -n "$work_dir" ]] || die "--work-dir is required"
  command -v jq >/dev/null || die "jq is required"
  state="$work_dir/controller-inputs.json"
  [[ -s "$state" ]] || die "controller state is missing: $state"
  validate_controller_state "$state"
  campaign_inputs="$(jq -er '.campaign_inputs' "$state")"
  input_root="$(jq -er '.input_root' "$state")"
  source_revision="$(jq -er '.source_revision' "$state")"
  image="$(jq -er '.image' "$state")"
  campaign_root="$(jq -er '.campaign_root' "$state")"
  context="$(jq -er '.context' "$state")"
  namespace="$(jq -er '.namespace' "$state")"
  max_follow_ups="$(jq -er '.max_follow_ups' "$state")"
  job_timeout="$(jq -er '.job_timeout' "$state")"
  if [[ "$(jq -r '.input_mode // "receipt"' "$state")" == "fresh" ]]; then
    fresh_mode=true
  fi
  if [[ -z "$control" ]]; then
    control="$(jq -r '.control // empty' "$state")"
  fi
  while IFS= read -r seed; do
    seeds+=("$seed")
  done < <(jq -er '.seeds[]' "$state")
fi

if [[ "$mode" == "start" ]]; then
  if [[ "$fresh_mode" == true ]]; then
    [[ -z "$campaign_inputs" ]] || die "--fresh-inputs conflicts with --campaign-inputs"
    [[ -z "$input_root" ]] || die "--fresh-inputs conflicts with --input-root"
    [[ -n "$fresh_raw_root" ]] || die "--fresh-raw-root is required with --fresh-inputs"
    [[ -n "$fresh_reference_root" ]] || die "--fresh-reference-root is required with --fresh-inputs"
    if [[ -n "$fresh_start_received_at_ns" || -n "$fresh_end_received_at_ns" ]]; then
      [[ -n "$fresh_start_received_at_ns" && -n "$fresh_end_received_at_ns" ]] \
        || die "both fresh explicit window bounds are required"
      [[ -z "$fresh_duration_ns$fresh_cutoff_received_at_ns$fresh_max_candidates" ]] \
        || die "fresh explicit and latest window modes are mutually exclusive"
    else
      [[ -n "$fresh_duration_ns" ]] || die "--fresh-duration-ns is required with --fresh-inputs latest mode"
      [[ -n "$fresh_max_candidates" ]] || die "--fresh-max-candidates is required with --fresh-inputs latest mode"
    fi
    [[ -n "$fresh_symbol" ]] || die "--fresh-symbol is required with --fresh-inputs"
    [[ -n "$fresh_image_ref" ]] || die "--fresh-image-ref is required with --fresh-inputs"
    [[ -n "$fresh_mission_id" ]] || die "--fresh-mission-id is required with --fresh-inputs"
    [[ -n "$fresh_output_root" ]] || die "--fresh-output-root is required with --fresh-inputs"
    [[ -n "$fresh_output_prefix" ]] || die "--fresh-output-prefix is required with --fresh-inputs"
    [[ -n "$fresh_bucket_ms" ]] || die "--fresh-bucket-ms is required with --fresh-inputs"
    [[ -n "$fresh_label_horizon_buckets" ]] || die "--fresh-label-horizon-buckets is required with --fresh-inputs"
    [[ -n "$fresh_top_depth" ]] || die "--fresh-top-depth is required with --fresh-inputs"
    [[ -n "$fresh_materializer" ]] || die "--fresh-materializer is required with --fresh-inputs"
    [[ -n "$fresh_max_scan_entries" ]] || die "--fresh-max-scan-entries is required with --fresh-inputs"
    [[ -n "$fresh_max_inputs" ]] || die "--fresh-max-inputs is required with --fresh-inputs"
    [[ -n "$fresh_max_input_bytes" ]] || die "--fresh-max-input-bytes is required with --fresh-inputs"
    [[ -n "$fresh_materializer_timeout_seconds" ]] || die "--fresh-materializer-timeout-seconds is required with --fresh-inputs"
    [[ -n "$fresh_max_materializer_output_bytes" ]] || die "--fresh-max-materializer-output-bytes is required with --fresh-inputs"
    [[ -n "$fresh_inventory_out" ]] || fresh_inventory_out="$work_dir/fresh-inputs/frozen.env"
    [[ -n "$fresh_request_out" ]] || fresh_request_out="$fresh_output_root/.fresh-inputs/$fresh_output_prefix/request.json"
    [[ -n "$fresh_materializer_work_dir" ]] || fresh_materializer_work_dir="$work_dir/fresh-inputs/materializer"
    [[ -n "$fresh_report_out" ]] || fresh_report_out="$work_dir/fresh-inputs/preparation.json"
    fresh_output_root="$(cd "$fresh_output_root" && pwd -P)" \
      || die "fresh output root does not exist: $fresh_output_root"
    input_root="$fresh_output_root/$fresh_output_prefix"
    campaign_inputs="$input_root/receipts/campaign-inputs.json"
  else
    [[ -z "$fresh_raw_root$fresh_reference_root$fresh_start_received_at_ns$fresh_end_received_at_ns$fresh_duration_ns$fresh_cutoff_received_at_ns$fresh_max_candidates$fresh_symbol$fresh_image_ref$fresh_mission_id$fresh_output_root$fresh_output_prefix$fresh_bucket_ms$fresh_label_horizon_buckets$fresh_top_depth$fresh_materializer$fresh_binary_dir$fresh_max_scan_entries$fresh_max_inputs$fresh_max_input_bytes$fresh_materializer_timeout_seconds$fresh_max_materializer_output_bytes$fresh_inventory_out$fresh_request_out$fresh_materializer_work_dir$fresh_report_out" ]] \
      || die "fresh input arguments require --fresh-inputs"
    [[ -n "$campaign_inputs" ]] || die "--campaign-inputs is required when --fresh-inputs is absent"
  fi
fi

for required in campaign_inputs input_root source_revision image campaign_root work_dir; do
  [[ -n "${!required}" ]] || die "--${required//_/-} is required"
done
if [[ "$mode" != "ack-readback" ]]; then
  if [[ -z "$signer" && ! ("$mode" == "start" && "$fresh_mode" == true) ]]; then
    die "--signer is required"
  fi
else
  [[ "$campaign_pod_name" =~ ^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$ ]] \
    || die "--campaign-pod-name must be an exact Kubernetes Pod name"
fi
((${#seeds[@]} >= 2)) || die "at least two --seed values are required"
[[ "$max_follow_ups" =~ ^[0-3]$ ]] || die "--max-follow-ups must be between 0 and 3"
[[ ! -e "$work_dir" || -d "$work_dir" ]] || die "--work-dir must be a directory"
if [[ "$mode" != "ack-readback" ]]; then
  if [[ "$fresh_mode" != true ]]; then
    campaign_inputs_dir="$(cd "$(dirname "$campaign_inputs")" && pwd -P)" \
      || die "campaign inputs directory does not exist: $(dirname "$campaign_inputs")"
    campaign_inputs="$campaign_inputs_dir/$(basename "$campaign_inputs")"
    [[ -f "$campaign_inputs" ]] || die "campaign inputs file does not exist: $campaign_inputs"
    input_root="$(cd "$input_root" && pwd -P)" || die "input root does not exist: $input_root"
  fi
  if [[ -n "$signer" ]]; then
    [[ -x "$signer" ]] || die "signer is not executable: $signer"
  fi
fi
command -v "$alpha_harness" >/dev/null || die "alpha-harness executable not found"
command -v jq >/dev/null || die "jq is required"
command -v cmp >/dev/null || die "cmp is required"
if [[ ! ("$mode" == "start" && "$fresh_mode" == true && -z "$signer") ]]; then
  command -v "$aliyun_cli" >/dev/null || die "aliyun executable not found"
  command -v "$kubectl_cli" >/dev/null || die "kubectl executable not found"
fi

# Fresh preparation can publish to an external output prefix. Verify an
# existing controller state before creating the work directory, logging a
# preparation stage, or invoking that materializer.
validate_fresh_controller_owner

umask 077
mkdir -p "$work_dir"
chmod 700 "$work_dir"
state_tmp=""
cleanup_sensitive_files() {
  local generation_dir
  [[ -z "$state_tmp" || ! -e "$state_tmp" ]] || rm -f -- "$state_tmp"
  for generation_dir in "$work_dir"/generation-*; do
    [[ -d "$generation_dir" ]] || continue
    rm -f -- "$generation_dir/signed-request.json"
    if [[ -e "$generation_dir/terminal-failure" || ! -e "$generation_dir/finalized" ]] \
      || validate_generation_completion "$generation_dir" "${generation_dir##*/generation-}"; then
      rm -f -- "$generation_dir/request.json" "$generation_dir/submission.json"
    fi
  done
}
trap cleanup_sensitive_files EXIT
trap 'exit 130' HUP INT TERM
on_controller_error() {
  trap - ERR
  die "command failed at line $2 with exit $1"
}
trap 'on_controller_error "$?" "$LINENO"' ERR

commit_generation_completion() {
  local outcome="$1" cycle_result="${2:-null}" learning_sha=""
  if [[ -e "$generation_dir/learning-checkpoint.json" ]]; then
    learning_sha="$(sha256_file "$generation_dir/learning-checkpoint.json")"
  fi
  jq -n --argjson generation "$generation" --arg outcome "$outcome" \
    --arg pod "$campaign_pod_name" --argjson cycle_result "$cycle_result" \
    --arg report "$(sha256_file "$generation_dir/generation-report.json")" \
    --arg learning "$learning_sha" '{
      schema_version:"monday.campaign_generation_completion.v1",
      generation:$generation,outcome:$outcome,campaign_pod_name:$pod,
      generation_report_sha256:$report,learning_checkpoint_sha256:$learning,
      cycle_result:$cycle_result
    }' >"$generation_dir/generation-complete.partial"
  mv -f -- "$generation_dir/generation-complete.partial" "$generation_dir/generation-complete"
  validate_generation_completion "$generation_dir" "$generation" \
    || die "Campaign completion checkpoint is invalid"
}

publish_cycle_checkpoint() {
  local dir="$1"
  # The generation completion is authoritative; this summary can be rebuilt
  # after interruption between committing the generation and writing the cache.
  jq '.cycle_result' "$dir/generation-complete" >"$work_dir/cycle-result.json.partial"
  mv -f -- "$work_dir/cycle-result.json.partial" "$work_dir/cycle-result.json"
}

oss_readback() {
  local object_url="$1"
  local destination="$2"
  local canonical_url="${object_url%%\?*}"
  local host_and_key host key bucket partial
  [[ "$(uname -s)" != "Darwin" ]] \
    || die "OSS result readback is forbidden on Darwin; run the ACK controller Job"
  [[ "$canonical_url" == https://*/* ]] || die "OSS readback URL is not canonical HTTPS"
  host_and_key="${canonical_url#https://}"
  host="${host_and_key%%/*}"
  key="${host_and_key#*/}"
  bucket="${host%%.*}"
  [[ -n "$key" && "$host" == "$bucket.oss-ap-northeast-1-internal.aliyuncs.com" ]] \
    || die "OSS readback URL is outside Tokyo internal OSS"
  partial="$destination.partial"
  rm -f -- "$partial"
  "$aliyun_cli" ossutil cp "oss://$bucket/$key" "$partial" \
    --endpoint oss-ap-northeast-1-internal.aliyuncs.com >&2
  mv -f -- "$partial" "$destination"
}

oss_publish_readback() {
  local source="$1"
  local object_url="$2"
  local readback="$3"
  local canonical_url="${object_url%%\?*}"
  local host_and_key host key bucket partial
  [[ "$(uname -s)" != "Darwin" ]] \
    || die "OSS learn publication is forbidden on Darwin; run the ACK controller Job"
  [[ -s "$source" ]] || die "OSS learn publication source is missing: $source"
  [[ "$canonical_url" == https://*/* ]] || die "OSS learn URL is not canonical HTTPS"
  host_and_key="${canonical_url#https://}"
  host="${host_and_key%%/*}"
  key="${host_and_key#*/}"
  bucket="${host%%.*}"
  [[ -n "$key" && "$host" == "$bucket.oss-ap-northeast-1-internal.aliyuncs.com" ]] \
    || die "OSS learn URL is outside Tokyo internal OSS"
  if ! "$aliyun_cli" ossutil cp "$source" "oss://$bucket/$key" \
    --endpoint oss-ap-northeast-1-internal.aliyuncs.com --forbid-overwrite >&2; then
    log_event immutable_publish_reused "object=$canonical_url"
  fi
  partial="$readback.partial"
  rm -f -- "$partial"
  "$aliyun_cli" ossutil cp "oss://$bucket/$key" "$partial" \
    --endpoint oss-ap-northeast-1-internal.aliyuncs.com >&2
  cmp -s "$source" "$partial" || die "published learn artifact readback SHA256 mismatch"
  mv -f -- "$partial" "$readback"
}

verify_kubernetes_provenance() {
  local job_status="$1"
  local pod_status="$2"
  local expected_job="$3"
  local expected_request="$4"
  local expected_image="sha256:$5"
  jq -e --arg job "$expected_job" --arg request "$expected_request" '
    .metadata.name == $job
    and .metadata.annotations["research.monday/request-sha256"] == $request
    and (.status.conditions // [] | any(.type == "Complete" and .status == "True"))
  ' "$job_status" >/dev/null || return 1
  jq -e --arg job "$expected_job" --arg request "$expected_request" --arg image "$expected_image" '
    (.items | length) == 1
    and .items[0].metadata.annotations["research.monday/request-sha256"] == $request
    and (.items[0].metadata.ownerReferences | any(.kind == "Job" and .name == $job))
    and .items[0].status.phase == "Succeeded"
    and ([.items[0].status.containerStatuses[]? | select(.name == "alpha-campaign")] | length) == 1
    and ([.items[0].status.containerStatuses[]? | select(.name == "alpha-campaign")][0]
      | (.imageID | endswith("@" + $image))
      and .state.terminated.exitCode == 0)
  ' "$pod_status" >/dev/null
}

if [[ "$mode" == "start" && "$fresh_mode" == true ]]; then
  controller_stage="fresh_inputs"
  fresh_preparation_dir="$work_dir/fresh-inputs"
  mkdir -p "$fresh_preparation_dir"
  fresh_preparation_stdout="$fresh_preparation_dir/preparation.stdout"
  fresh_prepare_args=(
    mission prepare-fresh-inputs
    --raw-root "$fresh_raw_root"
    --reference-root "$fresh_reference_root"
    --symbol "$fresh_symbol"
    --image-ref "$fresh_image_ref"
    --mission-id "$fresh_mission_id"
    --output-prefix "$fresh_output_prefix"
    --bucket-ms "$fresh_bucket_ms"
    --label-horizon-buckets "$fresh_label_horizon_buckets"
    --top-depth "$fresh_top_depth"
    --max-scan-entries "$fresh_max_scan_entries"
    --max-inputs "$fresh_max_inputs"
    --max-input-bytes "$fresh_max_input_bytes"
    --inventory-out "$fresh_inventory_out"
    --request-out "$fresh_request_out"
    --campaign-inputs-out "$campaign_inputs"
    --output-root "$fresh_output_root"
    --materializer "$fresh_materializer"
    --materializer-work-dir "$fresh_materializer_work_dir"
    --materializer-timeout-seconds "$fresh_materializer_timeout_seconds"
    --max-materializer-output-bytes "$fresh_max_materializer_output_bytes"
    --report-out "$fresh_report_out"
  )
  if [[ -n "$fresh_start_received_at_ns" ]]; then
    fresh_prepare_args+=(--start-received-at-ns "$fresh_start_received_at_ns")
    fresh_prepare_args+=(--end-received-at-ns "$fresh_end_received_at_ns")
  else
    fresh_prepare_args+=(--duration-ns "$fresh_duration_ns")
    fresh_prepare_args+=(--max-candidates "$fresh_max_candidates")
    if [[ -n "$fresh_cutoff_received_at_ns" ]]; then
      fresh_prepare_args+=(--cutoff-received-at-ns "$fresh_cutoff_received_at_ns")
    fi
  fi
  if [[ -n "$fresh_binary_dir" ]]; then
    fresh_prepare_args+=(--binary-dir "$fresh_binary_dir")
  fi
  log_event stage_started "stage=fresh_inputs" \
    "selection_mode=$(if [[ -n "$fresh_start_received_at_ns" ]]; then printf explicit; else printf latest; fi)" \
    "window_start_received_at_ns=$fresh_start_received_at_ns" \
    "window_end_received_at_ns=$fresh_end_received_at_ns" \
    "output_prefix=$fresh_output_prefix"
  "$alpha_harness" "${fresh_prepare_args[@]}" >"$fresh_preparation_stdout"
  [[ -s "$fresh_report_out" && -s "$campaign_inputs" ]] \
    || die "fresh input preparation did not produce complete evidence"
  fresh_report_campaign_inputs="$(jq -er '.campaign_inputs_path' "$fresh_report_out")"
  fresh_report_input_root="$(jq -er '.input_root' "$fresh_report_out")"
  fresh_report_request="$(jq -er '.request_path' "$fresh_report_out")"
  fresh_report_request_sha256="$(jq -er '.request_sha256' "$fresh_report_out")"
  [[ "$fresh_report_campaign_inputs" == "$campaign_inputs" ]] \
    || die "fresh preparation campaign inputs path drifted"
  [[ "$fresh_report_input_root" == "$input_root" ]] \
    || die "fresh preparation input root drifted"
  [[ "$fresh_report_request" == "$fresh_request_out" ]] \
    || die "fresh preparation request path drifted"
  [[ "$(sha256_file "$fresh_request_out")" == "$fresh_report_request_sha256" ]] \
    || die "fresh preparation request SHA256 readback differs"
  jq -e \
    --arg source_revision "$source_revision" \
    --arg symbol "$fresh_symbol" \
    --arg mission_id "$fresh_mission_id" \
    --arg output_prefix "$fresh_output_prefix" \
    '.schema_version == "monday.cex_fresh_inputs_preparation.v1"
     and .status == "ready"
     and .source_revision == $source_revision
     and .symbol == $symbol
     and .mission_id == $mission_id
     and .run_root == .input_root
     and .output_prefix == $output_prefix' \
    "$fresh_report_out" >/dev/null \
    || die "fresh input preparation identity is incomplete or drifted"
  log_event stage_completed \
    "stage=fresh_inputs" \
    "campaign_inputs_sha256=$(sha256_file "$campaign_inputs")" \
    "inventory_sha256=$(jq -er '.inventory_sha256' "$fresh_report_out")" \
    "input_fingerprint_sha256=$(jq -er '.input_fingerprint_sha256' "$fresh_report_out")"
fi

state="$work_dir/controller-inputs.json"
if [[ "$mode" == "ack-readback" ]]; then
  campaign_inputs_sha256="$(jq -er '.campaign_inputs_sha256' "$state")"
else
  campaign_inputs_sha256="$(sha256_file "$campaign_inputs")"
  seeds_json="$(printf '%s\n' "${seeds[@]}" | jq -R 'tonumber' | jq -s '.')"
  state_tmp="$state.partial.$$"
  fresh_state_json=null
  input_mode=receipt
  if [[ "$fresh_mode" == true ]]; then
    input_mode=fresh
    if [[ "$mode" == "start" ]]; then
      build_fresh_controller_contract
    else
      fresh_state_json="$(jq -c '.fresh' "$state")"
    fi
  fi
  jq -n \
    --arg campaign_inputs "$campaign_inputs" \
    --arg campaign_inputs_sha256 "$campaign_inputs_sha256" \
    --arg input_root "$input_root" \
    --arg source_revision "$source_revision" \
    --arg image "$image" \
    --arg campaign_root "$campaign_root" \
    --arg context "$context" \
    --arg namespace "$namespace" \
    --arg control "$control" \
    --arg input_mode "$input_mode" \
    --argjson fresh "$fresh_state_json" \
    --argjson max_follow_ups "$max_follow_ups" \
    --arg job_timeout "$job_timeout" \
    --argjson seeds "$seeds_json" \
    '{campaign_inputs:$campaign_inputs,campaign_inputs_sha256:$campaign_inputs_sha256,input_root:$input_root,source_revision:$source_revision,image:$image,campaign_root:$campaign_root,context:$context,namespace:$namespace,max_follow_ups:$max_follow_ups,job_timeout:$job_timeout,seeds:$seeds}
     + (if $input_mode == "fresh" then {input_mode:"fresh",fresh:$fresh,control:(if $control == "" then null else $control end)}
        elif $control == "" then {} else {control:$control} end)' \
    >"$state_tmp"
  if [[ -e "$state" ]]; then
    # Preserve historical checkpoints; only the retired token budget is irrelevant.
    jq -e -s 'length == 2 and ((.[0] | del(.control)) == (.[1] | del(.max_tokens,.control)))' \
      "$state_tmp" "$state" >/dev/null \
      || die "existing work directory belongs to different controller inputs"
    rm -f -- "$state_tmp"
  else
    mv -- "$state_tmp" "$state"
  fi
fi
state_tmp=""
if [[ "$mode" == "start" && "$fresh_mode" == true && -z "$signer" ]]; then
  needs_authority="$work_dir/needs-authority.json"
  jq -n \
    --arg campaign_inputs "$campaign_inputs" \
    --arg campaign_inputs_sha256 "$campaign_inputs_sha256" \
    --arg request "$fresh_request_out" \
    --arg request_sha256 "$(jq -er '.request_sha256' "$fresh_report_out")" \
    --arg preparation_report "$fresh_report_out" \
    --arg input_root "$input_root" \
    '{
      schema_version:"monday.campaign_cycle_needs_authority.v1",
      status:"needs_authority",
      reason:"signer_missing",
      campaign_inputs:$campaign_inputs,
      campaign_inputs_sha256:$campaign_inputs_sha256,
      request:$request,
      request_sha256:$request_sha256,
      input_root:$input_root,
      preparation_report:$preparation_report,
      preparation_preserved:true,
      resume_ready:true
    }' >"$needs_authority"
  log_event needs_authority \
    "stage=fresh_inputs" \
    "reason=signer_missing" \
    "campaign_inputs_sha256=$campaign_inputs_sha256" \
    "artifact=$needs_authority"
  jq . "$needs_authority"
  exit 0
fi
log_event cycle_started \
  "mode=$mode" \
  "campaign_inputs_sha256=$campaign_inputs_sha256" \
  "source_revision=$source_revision" \
  "image=$image" \
  "seed_count=${#seeds[@]}" \
  "max_follow_ups=$max_follow_ups" \
  "context=$context" \
  "namespace=$namespace"

kubectl_readback_args=(--namespace "$namespace")
if [[ "$mode" != "ack-readback" ]]; then
  kubectl_readback_args=(--context "$context" --namespace "$namespace")
fi

if [[ -s "$work_dir/cycle-result.json" ]]; then
  completed_generation="$(jq -er '.generation | select(type == "number" and . >= 0 and floor == .)' "$work_dir/cycle-result.json")"
  completed_dir="$work_dir/generation-$completed_generation"
  validate_generation_completion "$completed_dir" "$completed_generation" \
    || die "saved Campaign completion checkpoint is invalid or unsupported"
  [[ "$(jq -er '.outcome' "$completed_dir/generation-complete")" == complete ]] \
    || die "cycle result does not belong to a terminal generation"
  publish_cycle_checkpoint "$completed_dir"
  log_event cycle_checkpoint_reused "result=cycle-result.json"
  jq . "$work_dir/cycle-result.json"
  exit 0
fi

research_plan=""
generation=0
while ((generation <= max_follow_ups)); do
  current_generation="$generation"
  controller_stage="generation"
  generation_dir="$work_dir/generation-$generation"
  mkdir -p "$generation_dir"
  freeze="$generation_dir/freeze.json"
  freeze_report="$generation_dir/freeze-report.json"
  signed_request="$generation_dir/signed-request.json"
  request="$generation_dir/request.json"
  submission="$generation_dir/submission.json"
  finalize_report="$generation_dir/finalize-report.json"
  dispatch_report="$generation_dir/dispatch-report.json"
  result="$generation_dir/campaign-result.json"
  [[ ! -e "$generation_dir/terminal-failure" ]] \
    || die "Campaign generation $generation already reached a terminal failure"
  if [[ -e "$generation_dir/generation-complete" ]]; then
    validate_generation_completion "$generation_dir" "$generation" \
      || die "saved Campaign completion checkpoint is invalid or unsupported"
    log_event generation_checkpoint_reused "generation=$generation"
    if [[ "$(jq -er '.outcome' "$generation_dir/generation-complete")" == complete ]]; then
      publish_cycle_checkpoint "$generation_dir"
      jq . "$work_dir/cycle-result.json"
      exit 0
    fi
    research_plan="$generation_dir/next-research-plan.json"
    [[ -s "$research_plan" ]] || die "completed generation is missing its follow-up plan"
    if [[ "$mode" == ack-readback \
      && "$(jq -er '.campaign_pod_name' "$generation_dir/generation-complete")" == "$campaign_pod_name" ]]; then
      controller_stage="approval_handoff"
      log_event stage_checkpoint_reused "generation=$generation" "stage=approval_handoff"
      cycle_status "$work_dir"
      exit 0
    fi
    generation=$((generation + 1))
    continue
  fi
  if [[ "$mode" == "ack-readback" && ! -e "$generation_dir/dispatched" ]]; then
    die "ACK readback requires an approved, dispatched Campaign generation"
  fi
  log_event generation_started \
    "generation=$generation" \
    "research_plan=${research_plan:-initial}" \
    "seed_count=${#seeds[@]}"

  freeze_args=(
    mission campaign-freeze
    --campaign-inputs "$campaign_inputs"
    --input-root "$input_root"
    --source-revision "$source_revision"
    --image "$image"
    --campaign-root "$campaign_root"
    --output "$freeze"
  )
  for seed in "${seeds[@]}"; do
    freeze_args+=(--seed "$seed")
  done
  [[ -z "$research_plan" ]] || freeze_args+=(--research-plan "$research_plan")
  if [[ ! -e "$generation_dir/frozen" ]]; then
    controller_stage="freeze"
    log_event stage_started "generation=$generation" "stage=freeze"
    rm -f -- "$freeze" "$freeze_report"
    "$alpha_harness" "${freeze_args[@]}" >"$freeze_report"
    [[ -s "$freeze" && -s "$freeze_report" ]] || die "Campaign freeze did not produce complete evidence"
    : >"$generation_dir/frozen"
    log_event stage_completed \
      "generation=$generation" \
      "stage=freeze" \
      "campaign_id=$(jq -er '.campaign_id' "$freeze_report")" \
      "declared_total_trials=$(jq -er '.declared_total_trials' "$freeze_report")"
  else
    log_event stage_checkpoint_reused "generation=$generation" "stage=freeze"
  fi
  [[ -s "$freeze" && -s "$freeze_report" ]] || die "Campaign freeze checkpoint is incomplete"

  if [[ ! -e "$generation_dir/finalized" ]]; then
    controller_stage="sign_finalize"
    log_event stage_started "generation=$generation" "stage=sign_finalize"
    rm -f -- "$signed_request" "$request" "$submission" "$finalize_report"
    "$signer" --freeze "$freeze" --output "$signed_request" >/dev/null
    [[ -s "$signed_request" ]] || die "signer did not create a signed request"
    chmod 600 "$signed_request"
    "$alpha_harness" mission campaign-finalize \
      --freeze "$freeze" \
      --signed-request "$signed_request" \
      --attempt-id "research-cycle-g$generation" \
      --image "$image" \
      --request-out "$request" \
      --submission-out "$submission" >"$finalize_report"
    [[ -s "$request" && -s "$submission" && -s "$finalize_report" ]] \
      || die "Campaign finalize did not produce complete evidence"
    chmod 600 "$request" "$submission"
    rm -f -- "$signed_request"
    : >"$generation_dir/finalized"
  else
    log_event stage_checkpoint_reused "generation=$generation" "stage=sign_finalize"
  fi
  [[ -s "$request" && -s "$finalize_report" ]] || die "Campaign finalize checkpoint is incomplete"

  campaign_id="$(jq -er '.campaign_id' "$finalize_report")"
  request_sha256="$(jq -er '.request_sha256' "$finalize_report")"
  job_name="$(jq -er '.job_name' "$finalize_report")"
  [[ "$(sha256_file "$request")" == "$request_sha256" ]] \
    || die "Campaign request no longer matches its finalized SHA256"
  log_event stage_completed \
    "generation=$generation" \
    "stage=sign_finalize" \
    "campaign_id=$campaign_id" \
    "request_sha256=$request_sha256" \
    "job_name=$job_name"

  if [[ ! -e "$generation_dir/dispatched" ]]; then
    controller_stage="dispatch"
    log_event stage_started "generation=$generation" "stage=dispatch" "job_name=$job_name"
    [[ -s "$submission" ]] || die "finalized Campaign is missing its resumable submission"
    controller_control="$(printenv MONDAY_CAMPAIGN_CONTROL 2>/dev/null || true)"
    dispatch_control="$control"
    [[ -n "$dispatch_control" ]] || dispatch_control="$controller_control"
    if [[ "$fresh_mode" == true && ( -z "$dispatch_control" || ! -f "$dispatch_control" ) ]]; then
      needs_authority="$generation_dir/needs-authority.json"
      jq -n \
        --arg campaign_id "$campaign_id" \
        --arg request_sha256 "$request_sha256" \
        --arg request "$request" \
        --arg submission "$submission" \
        --arg campaign_inputs_sha256 "$campaign_inputs_sha256" \
        --arg dispatch_control "$dispatch_control" \
        '{
          schema_version:"monday.campaign_cycle_needs_authority.v1",
          status:"needs_authority",
          reason:"dispatch_control_missing",
          campaign_id:$campaign_id,
          request_sha256:$request_sha256,
          campaign_inputs_sha256:$campaign_inputs_sha256,
          control:$dispatch_control,
          request:$request,
          submission:$submission,
          sign_finalize_dispatch_preserved:true
        }' >"$needs_authority"
      log_event needs_authority \
        "generation=$generation" \
        "stage=dispatch" \
        "reason=dispatch_control_missing" \
        "campaign_id=$campaign_id" \
        "request_sha256=$request_sha256" \
        "artifact=$needs_authority"
      jq . "$needs_authority"
      exit 0
    fi
    dispatch_control_args=()
    if [[ -n "$control" ]]; then
      dispatch_control_args=(--control "$control")
    fi
    "$alpha_harness" mission dispatch submit \
      "${dispatch_control_args[@]}" \
      --submission "$submission" \
      --context "$context" \
      --namespace "$namespace" >"$dispatch_report.partial"
    mv -f -- "$dispatch_report.partial" "$dispatch_report"
    : >"$generation_dir/dispatched"
    log_event stage_completed "generation=$generation" "stage=dispatch" "job_name=$job_name"
  else
    log_event stage_checkpoint_reused "generation=$generation" "stage=dispatch" "job_name=$job_name"
  fi
  [[ -s "$dispatch_report" ]] || die "Campaign dispatch checkpoint is incomplete"

  if [[ "$mode" != "ack-readback" ]]; then
    controller_stage="ack_handoff"
    log_event stage_completed \
      "generation=$generation" \
      "stage=ack_handoff" \
      "job_name=$job_name" \
      "template=campaign-cycle-controller-job.example.yaml"
    print_ack_controller_job "$campaign_id" "$request_sha256" "$work_dir"
    exit 0
  fi

  job_status="$generation_dir/job-status.json"
  pod_status="$generation_dir/pod-status.json"
  if [[ ! -e "$generation_dir/provenance-readback-complete" ]]; then
    controller_stage="kubernetes_runtime_readback"
    log_event stage_started \
      "generation=$generation" \
      "stage=kubernetes_runtime_readback" \
      "job_name=$job_name" \
      "timeout=$job_timeout"
    wait_completed=true
    if ! "$kubectl_cli" "${kubectl_readback_args[@]}" wait \
      --for=condition=complete "job/$job_name" --timeout="$job_timeout" >&2; then
      wait_completed=false
    fi
    "$kubectl_cli" "${kubectl_readback_args[@]}" get \
      "job/$job_name" -o json >"$job_status" \
      || die "Campaign Job status readback failed: $job_name"
    job_complete="$(jq -r '(.status.conditions // [] | any(.type == "Complete" and .status == "True"))' "$job_status")"
    job_failed="$(jq -r '(.status.conditions // [] | any(.type == "Failed" and .status == "True"))' "$job_status")"
    if [[ "$job_failed" == true ]]; then
      "$kubectl_cli" "${kubectl_readback_args[@]}" get \
        "pod/$campaign_pod_name" -o json | jq '{items:[.]}' >"$pod_status" || true
      : >"$generation_dir/terminal-failure"
      rm -f -- "$request" "$submission"
      die "Campaign Job failed: $job_name"
    fi
    [[ "$job_complete" == true ]] \
      || { [[ "$wait_completed" == true ]] && die "Campaign Job completion condition is missing: $job_name"; die "Campaign Job wait ended before terminal status: $job_name"; }
    pod_readback_ok=true
    if ! "$kubectl_cli" "${kubectl_readback_args[@]}" get \
      "pod/$campaign_pod_name" -o json | jq '{items:[.]}' >"$pod_status"; then
      pod_readback_ok=false
    fi
    [[ "$pod_readback_ok" == true ]] || die "Campaign Pod status readback failed: $job_name"
    image_identity="$(jq -er '.image_identity' "$request")"
    if ! verify_kubernetes_provenance \
      "$job_status" "$pod_status" "$job_name" "$request_sha256" "$image_identity"; then
      : >"$generation_dir/terminal-failure"
      rm -f -- "$request" "$submission"
      die "Campaign Pod or image provenance does not match the submitted request"
    fi
    : >"$generation_dir/provenance-readback-complete"
    log_event stage_completed \
      "generation=$generation" \
      "stage=kubernetes_runtime_readback" \
      "job_name=$job_name" \
      "request_sha256=$request_sha256" \
      "image_identity=$image_identity"
  else
    log_event stage_checkpoint_reused \
      "generation=$generation" \
      "stage=kubernetes_runtime_readback" \
      "job_name=$job_name"
  fi
  image_identity="$(jq -er '.image_identity' "$request")"
  verify_kubernetes_provenance \
    "$job_status" "$pod_status" "$job_name" "$request_sha256" "$image_identity" \
    || die "saved Campaign Pod provenance is invalid"

  round_readback_dir="$generation_dir/round-readback"
  mkdir -p "$round_readback_dir"
  if [[ ! -e "$generation_dir/result-readback-complete" ]]; then
    controller_stage="oss_result_readback"
    log_event stage_started \
      "generation=$generation" \
      "stage=oss_result_readback" \
      "campaign_id=$campaign_id"
    result_url="$(jq -er '.campaign_result_readback_url' "$request")"
    oss_readback "$result_url" "$result"
    expected_learning_directive_sha256="none"
    if jq -e '.research_plan.learning_directive != null' "$request" >/dev/null; then
      expected_learning_directive_sha256="$(
        sha256_file <(jq -cj '.research_plan.learning_directive' "$request")
      )"
    fi
    jq -e --slurpfile request_doc "$request" \
      --arg request_sha256 "$request_sha256" \
      --arg expected_learning_directive_sha256 "$expected_learning_directive_sha256" '
      .schema_version == "cex-campaign-result-v8"
      and .campaign_id == $request_doc[0].campaign_id
      and .request_sha256 == $request_sha256
      and .build_source_revision == $request_doc[0].build_source_revision
      and .image_identity == $request_doc[0].image_identity
      and .campaign_inputs_sha256 == $request_doc[0].campaign_inputs_sha256
      and .producer_source_revision == $request_doc[0].producer_source_revision
      and .producer_image_identity == $request_doc[0].producer_image_identity
      and .search_policy_revision == $request_doc[0].research_plan.search_policy_revision
      and .learning_directive == ($request_doc[0].research_plan.learning_directive // null)
      and (
        if ($request_doc[0].research_plan.learning_directive // null) == null then
          .learning_directive_sha256 == null
        else
          .learning_directive_sha256 == $expected_learning_directive_sha256
        end
      )
      and .holdout_id == $request_doc[0].holdout_id
      and .declared_total_trials == $request_doc[0].declared_total_trials
      and .stop_rule == "bounded_multi_round_single_finalize_v2"
      and (.rounds | length) == ($request_doc[0].rounds | length)
      and ([.rounds[].consumed_trials] | add // 0) == .consumed_trials
      and (all(.rounds[]; .request_sha256 == $request_sha256))
      and ([.rounds[] as $round
        | any($request_doc[0].rounds[];
            .round_id == $round.round_id and .identity == $round.identity)] | all)
      and (
        if .termination_reason == "campaign_no_candidate" then
          .selected_round_id == null and .selected_candidate_id == null
          and .selected_candidate_content_hash == null and .finalization == null
        elif .termination_reason == "campaign_selected_pre_holdout" then
          .selected_round_id != null and .selected_candidate_id != null
          and .selected_candidate_content_hash != null and .finalization == null
        elif .termination_reason == "campaign_finalized" then
          .selected_round_id != null and .selected_candidate_id != null
          and .selected_candidate_content_hash != null and .finalization != null
        else false end
      )
    ' "$result" >/dev/null || die "Campaign result is incomplete or not bound to the request"

    request_round_count="$(jq -er '.rounds | length' "$request")"
    for ((round_index = 0; round_index < request_round_count; round_index++)); do
      request_round_id="$(jq -er --argjson index "$round_index" '.rounds[$index].round_id' "$request")"
      result_round_id="$(jq -er --argjson index "$round_index" '.rounds[$index].round_id' "$result")"
      [[ "$request_round_id" == "$result_round_id" ]] || die "Campaign round ordering or identity drifted"
      mission_url="$(jq -er --argjson index "$round_index" '.rounds[$index].mission_readback_url' "$request")"
      bundle_url="$(jq -er --argjson index "$round_index" '.rounds[$index].result_readback_url' "$request")"
      mission_readback="$round_readback_dir/round-$round_index-mission.json"
      bundle_readback="$round_readback_dir/round-$round_index-results.zip"
      oss_readback "$mission_url" "$mission_readback"
      oss_readback "$bundle_url" "$bundle_readback"
      mission_sha256="$(jq -er --argjson index "$round_index" '.rounds[$index].mission_sha256' "$result")"
      bundle_sha256="$(jq -er --argjson index "$round_index" '.rounds[$index].result_bundle_sha256' "$result")"
      bundle_readback_sha256="$(jq -er --argjson index "$round_index" '.rounds[$index].result_readback_bundle_sha256' "$result")"
      [[ "$(sha256_file "$mission_readback")" == "$mission_sha256" ]] \
        || die "Campaign round Mission readback SHA256 mismatch: $request_round_id"
      [[ "$(sha256_file "$bundle_readback")" == "$bundle_sha256" \
        && "$bundle_sha256" == "$bundle_readback_sha256" ]] \
        || die "Campaign round result readback SHA256 mismatch: $request_round_id"
      log_event round_readback_completed \
        "generation=$generation" \
        "round_index=$round_index" \
        "round_id=$request_round_id" \
        "mission_sha256=$mission_sha256" \
        "result_bundle_sha256=$bundle_sha256"
    done
    : >"$generation_dir/result-readback-complete"
  else
    log_event stage_checkpoint_reused \
      "generation=$generation" \
      "stage=oss_result_readback" \
      "campaign_id=$campaign_id"
  fi

  result_sha256="$(sha256_file "$result")"
  jq -e --arg campaign_id "$campaign_id" --arg request_sha256 "$request_sha256" \
    '.campaign_id == $campaign_id and .request_sha256 == $request_sha256' \
    "$result" >/dev/null || die "Campaign result identity does not match the submitted request"
  request_round_count="$(jq -er '.rounds | length' "$request")"
  for ((round_index = 0; round_index < request_round_count; round_index++)); do
    mission_readback="$round_readback_dir/round-$round_index-mission.json"
    bundle_readback="$round_readback_dir/round-$round_index-results.zip"
    mission_sha256="$(jq -er --argjson index "$round_index" '.rounds[$index].mission_sha256' "$result")"
    bundle_sha256="$(jq -er --argjson index "$round_index" '.rounds[$index].result_bundle_sha256' "$result")"
    bundle_readback_sha256="$(jq -er --argjson index "$round_index" '.rounds[$index].result_readback_bundle_sha256' "$result")"
    [[ "$(sha256_file "$mission_readback")" == "$mission_sha256" \
      && "$(sha256_file "$bundle_readback")" == "$bundle_sha256" \
      && "$bundle_sha256" == "$bundle_readback_sha256" ]] \
      || die "saved Campaign round readback is invalid: $round_index"
  done
  # Keep the finalized request until durable ledger settlement. The Rust seam
  # independently verifies Job/Pod identity and reconstructs the round evidence;
  # neither a completed Job nor these shell checkpoint files authorize a child.
  controller_stage="ledger_settlement"
  [[ -s "$submission" ]] || die "Campaign settlement requires the original finalized submission"
  "$alpha_harness" mission dispatch settle \
    --submission "$submission" \
    --context "$context" \
    --namespace "$namespace" >"$generation_dir/settlement-report.json.partial"
  jq -e --arg request "$request_sha256" --arg result "$result_sha256" \
    '.status == "settled" and .request_sha256 == $request and .campaign_result_sha256 == $result' \
    "$generation_dir/settlement-report.json.partial" >/dev/null \
    || die "Campaign settlement report differs from the independently read-back result"
  mv -f -- "$generation_dir/settlement-report.json.partial" "$generation_dir/settlement-report.json"
  log_event stage_completed "generation=$generation" "stage=ledger_settlement" "campaign_id=$campaign_id"

  termination_reason="$(jq -er '.termination_reason' "$result")"
  observed_image_id="$(jq -er '.items[0].status.containerStatuses[] | select(.name == "alpha-campaign") | .imageID' "$pod_status")"
  search_policy_revision_id="$(jq -er '.search_policy_revision.revision_id' "$result")"
  learning_directive_sha256="$(jq -r '.learning_directive_sha256 // "none"' "$result")"
  log_event stage_completed \
    "generation=$generation" \
    "stage=oss_result_readback" \
    "campaign_id=$campaign_id" \
    "campaign_result_sha256=$result_sha256" \
    "termination_reason=$termination_reason" \
    "learning_directive_sha256=$learning_directive_sha256" \
    "search_policy_revision_id=$search_policy_revision_id" \
    "round_count=$request_round_count" \
    "consumed_trials=$(jq -er '.consumed_trials' "$result")"

  jq -n \
    --argjson generation "$generation" \
    --arg campaign_id "$campaign_id" \
    --arg request_sha256 "$request_sha256" \
    --arg job_name "$job_name" \
    --arg result_sha256 "$result_sha256" \
    --arg termination_reason "$termination_reason" \
    --arg observed_image_id "$observed_image_id" \
    --arg learning_directive_sha256 "$learning_directive_sha256" \
    --arg search_policy_revision_id "$search_policy_revision_id" \
    --argjson round_readback_count "$request_round_count" \
    '{generation:$generation,campaign_id:$campaign_id,request_sha256:$request_sha256,job_name:$job_name,campaign_result_sha256:$result_sha256,termination_reason:$termination_reason,observed_image_id:$observed_image_id,learning_directive_sha256:$learning_directive_sha256,search_policy_revision_id:$search_policy_revision_id,round_readback_count:$round_readback_count}' \
    >"$generation_dir/generation-report.json"

  if [[ "$termination_reason" != "campaign_no_candidate" ]]; then
    commit_generation_completion complete "$(cat "$generation_dir/generation-report.json")"
    publish_cycle_checkpoint "$generation_dir"
    rm -f -- "$request" "$submission"
    controller_stage="complete"
    log_event cycle_completed \
      "generation=$generation" \
      "campaign_id=$campaign_id" \
      "termination_reason=$termination_reason" \
      "campaign_result_sha256=$result_sha256"
    jq . "$work_dir/cycle-result.json"
    exit 0
  fi
  if ((generation == max_follow_ups)); then
    commit_generation_completion complete "$(jq '. + {bounded_loop_exhausted:true}' "$generation_dir/generation-report.json")"
    publish_cycle_checkpoint "$generation_dir"
    rm -f -- "$request" "$submission"
    controller_stage="complete"
    log_event cycle_completed \
      "generation=$generation" \
      "campaign_id=$campaign_id" \
      "termination_reason=bounded_loop_exhausted" \
      "campaign_result_sha256=$result_sha256"
    jq . "$work_dir/cycle-result.json"
    exit 0
  fi

  research_plan="$generation_dir/next-research-plan.json"
  controller_stage="campaign_learning"
  log_event stage_started \
    "generation=$generation" \
    "stage=campaign_learning" \
    "parent_campaign_id=$campaign_id" \
    "parent_result_sha256=$result_sha256"
  learning_checkpoint="$generation_dir/learning-checkpoint.json"
  if [[ ! -e "$learning_checkpoint" ]]; then
    "$alpha_harness" mission campaign-learn \
      --request "$request" \
      --result "$result" \
      --result-sha256 "$result_sha256" \
      --output "$research_plan" >"$generation_dir/learn-report.json.partial"
    mv -f -- "$generation_dir/learn-report.json.partial" "$generation_dir/learn-report.json"
    learning_outcome="$(jq -er '.outcome' "$generation_dir/learn-report.json")"
    research_plan_file_sha256=""
    if [[ "$learning_outcome" == follow_up ]]; then
      [[ -s "$research_plan" ]] || die "Campaign learning did not produce a child research plan"
      research_plan_file_sha256="$(sha256_file "$research_plan")"
    fi
    jq -n --arg request "$request_sha256" --arg result "$result_sha256" \
      --arg report "$(sha256_file "$generation_dir/learn-report.json")" \
      --arg plan "$research_plan_file_sha256" --arg outcome "$learning_outcome" '{
        schema_version:"monday.campaign_learning_checkpoint.v1",
        request_sha256:$request,campaign_result_sha256:$result,
        learn_report_sha256:$report,research_plan_file_sha256:$plan,outcome:$outcome
      }' >"$learning_checkpoint.partial"
    mv -f -- "$learning_checkpoint.partial" "$learning_checkpoint"
  else
    log_event stage_checkpoint_reused "generation=$generation" "stage=campaign_learning"
  fi
  validate_learning_checkpoint "$generation_dir" "$request_sha256" "$result_sha256" \
    || die "saved Campaign learning checkpoint is invalid"
  [[ -s "$generation_dir/learn-report.json" ]] \
    || die "Campaign learning did not produce a learn report"
  learning_outcome="$(jq -er '.outcome' "$generation_dir/learn-report.json")"
  learn_report_url="$campaign_root/campaign-id=$campaign_id/learning/generation=$generation/learn-report.json"
  oss_publish_readback \
    "$generation_dir/learn-report.json" "$learn_report_url" \
    "$generation_dir/learn-report-readback.json"
  if [[ "$learning_outcome" == "follow_up" ]]; then
    [[ -s "$research_plan" ]] || die "Campaign learning did not produce a child research plan"
    research_plan_url="$campaign_root/campaign-id=$campaign_id/learning/generation=$generation/next-research-plan.json"
    oss_publish_readback \
      "$research_plan" "$research_plan_url" \
      "$generation_dir/next-research-plan-readback.json"
  elif [[ "$learning_outcome" != "no_improvement" ]]; then
    die "Campaign learning returned an unsupported outcome: $learning_outcome"
  fi
  log_event stage_completed \
    "generation=$generation" \
    "stage=campaign_learning" \
    "parent_campaign_id=$campaign_id" \
    "failure_class=$(jq -er '.failure_class' "$generation_dir/learn-report.json")" \
    "outcome=$learning_outcome" \
    "learn_report_sha256=$(sha256_file "$generation_dir/learn-report.json")"
  if [[ "$learning_outcome" == "no_improvement" ]]; then
    commit_generation_completion complete "$(jq --arg learn_report_url "$learn_report_url" \
      --arg learn_report_sha256 "$(sha256_file "$generation_dir/learn-report.json")" \
      '. + {termination_reason:"no_improvement",learning_outcome:"no_improvement",learn_report_url:$learn_report_url,learn_report_sha256:$learn_report_sha256}' \
      "$generation_dir/generation-report.json")"
    publish_cycle_checkpoint "$generation_dir"
    rm -f -- "$request" "$submission"
    controller_stage="complete"
    log_event cycle_completed \
      "generation=$generation" \
      "campaign_id=$campaign_id" \
      "termination_reason=no_improvement" \
      "campaign_result_sha256=$result_sha256"
    jq . "$work_dir/cycle-result.json"
    exit 0
  fi
  commit_generation_completion follow_up
  rm -f -- "$request" "$submission"
  if [[ "$mode" == "ack-readback" ]]; then
    controller_stage="approval_handoff"
    log_event stage_completed \
      "generation=$generation" \
      "stage=approval_handoff" \
      "next_generation=$((generation + 1))" \
      "research_plan_sha256=$(jq -er '.research_plan_sha256' "$generation_dir/learn-report.json")"
    cycle_status "$work_dir"
    exit 0
  fi
  generation=$((generation + 1))
done
