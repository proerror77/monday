#!/usr/bin/env bash
set -euo pipefail

die() {
  echo "campaign-cycle-controller: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: campaign-cycle-controller.sh \
  --campaign-inputs FILE --input-root DIR --source-revision SHA \
  --image IMAGE@sha256:DIGEST --campaign-root HTTPS_URL \
  --signer EXECUTABLE --work-dir DIR --seed N --seed N \
  [--context NAME] [--namespace NAME] [--max-follow-ups 3] \
  [--max-tokens 300] [--job-timeout 4h]

The signer is a separate trust boundary. It is invoked as:
  SIGNER --freeze FREEZE_JSON --output SIGNED_REQUEST_JSON
It must sign every action in the frozen signing plan without changing the
query-free object identity, HTTP method, content type, or required headers.
EOF
}

alpha_harness="alpha-harness"
aliyun_cli="aliyun"
kubectl_cli="kubectl"
context="monday-research-apne1"
namespace="monday-research"
max_follow_ups=3
max_tokens=300
job_timeout="4h"
campaign_inputs=""
input_root=""
source_revision=""
image=""
campaign_root=""
signer=""
work_dir=""
seeds=()

while (($#)); do
  case "$1" in
    --alpha-harness) alpha_harness="$2"; shift 2 ;;
    --aliyun) aliyun_cli="$2"; shift 2 ;;
    --kubectl) kubectl_cli="$2"; shift 2 ;;
    --campaign-inputs) campaign_inputs="$2"; shift 2 ;;
    --input-root) input_root="$2"; shift 2 ;;
    --source-revision) source_revision="$2"; shift 2 ;;
    --image) image="$2"; shift 2 ;;
    --campaign-root) campaign_root="$2"; shift 2 ;;
    --signer) signer="$2"; shift 2 ;;
    --work-dir) work_dir="$2"; shift 2 ;;
    --seed) seeds+=("$2"); shift 2 ;;
    --context) context="$2"; shift 2 ;;
    --namespace) namespace="$2"; shift 2 ;;
    --max-follow-ups) max_follow_ups="$2"; shift 2 ;;
    --max-tokens) max_tokens="$2"; shift 2 ;;
    --job-timeout) job_timeout="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

for required in campaign_inputs input_root source_revision image campaign_root signer work_dir; do
  [[ -n "${!required}" ]] || die "--${required//_/-} is required"
done
((${#seeds[@]} >= 2)) || die "at least two --seed values are required"
[[ "$max_follow_ups" =~ ^[0-3]$ ]] || die "--max-follow-ups must be between 0 and 3"
[[ "$max_tokens" =~ ^[1-9][0-9]*$ ]] || die "--max-tokens must be positive"
[[ ! -e "$work_dir" || -d "$work_dir" ]] || die "--work-dir must be a directory"
[[ -x "$signer" ]] || die "signer is not executable: $signer"
command -v "$alpha_harness" >/dev/null || die "alpha-harness executable not found"
command -v "$aliyun_cli" >/dev/null || die "aliyun executable not found"
command -v "$kubectl_cli" >/dev/null || die "kubectl executable not found"
command -v jq >/dev/null || die "jq is required"
command -v cmp >/dev/null || die "cmp is required"

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
    if [[ -e "$generation_dir/generation-complete" || ! -e "$generation_dir/finalized" ]]; then
      rm -f -- "$generation_dir/request.json" "$generation_dir/submission.json"
    elif [[ -e "$generation_dir/dispatched" ]]; then
      rm -f -- "$generation_dir/submission.json"
    fi
  done
}
trap cleanup_sensitive_files EXIT
trap 'exit 130' HUP INT TERM

sha256_file() {
  if command -v shasum >/dev/null; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

oss_readback() {
  local object_url="$1"
  local destination="$2"
  local canonical_url="${object_url%%\?*}"
  local host_and_key host key bucket partial
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
    --endpoint oss-ap-northeast-1.aliyuncs.com
  mv -f -- "$partial" "$destination"
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
  jq -e --arg request "$expected_request" --arg image "$expected_image" '
    (.items | length) == 1
    and .items[0].metadata.annotations["research.monday/request-sha256"] == $request
    and .items[0].status.phase == "Succeeded"
    and ([.items[0].status.containerStatuses[]? | select(.name == "alpha-campaign")] | length) == 1
    and ([.items[0].status.containerStatuses[]? | select(.name == "alpha-campaign")][0]
      | (.imageID | endswith("@" + $image))
      and .state.terminated.exitCode == 0)
  ' "$pod_status" >/dev/null
}

campaign_inputs_sha256="$(sha256_file "$campaign_inputs")"
seeds_json="$(printf '%s\n' "${seeds[@]}" | jq -R 'tonumber' | jq -s '.')"
state="$work_dir/controller-inputs.json"
state_tmp="$state.partial.$$"
jq -n \
  --arg campaign_inputs "$campaign_inputs" \
  --arg campaign_inputs_sha256 "$campaign_inputs_sha256" \
  --arg input_root "$input_root" \
  --arg source_revision "$source_revision" \
  --arg image "$image" \
  --arg campaign_root "$campaign_root" \
  --arg context "$context" \
  --arg namespace "$namespace" \
  --argjson max_follow_ups "$max_follow_ups" \
  --argjson max_tokens "$max_tokens" \
  --arg job_timeout "$job_timeout" \
  --argjson seeds "$seeds_json" \
  '{campaign_inputs:$campaign_inputs,campaign_inputs_sha256:$campaign_inputs_sha256,input_root:$input_root,source_revision:$source_revision,image:$image,campaign_root:$campaign_root,context:$context,namespace:$namespace,max_follow_ups:$max_follow_ups,max_tokens:$max_tokens,job_timeout:$job_timeout,seeds:$seeds}' \
  >"$state_tmp"
if [[ -e "$state" ]]; then
  cmp -s "$state_tmp" "$state" || die "existing work directory belongs to different controller inputs"
  rm -f -- "$state_tmp"
else
  mv -- "$state_tmp" "$state"
fi
state_tmp=""

if [[ -s "$work_dir/cycle-result.json" ]]; then
  jq . "$work_dir/cycle-result.json"
  exit 0
fi

research_plan=""
generation=0
while ((generation <= max_follow_ups)); do
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
    research_plan="$generation_dir/next-research-plan.json"
    [[ -s "$research_plan" ]] || die "completed generation is missing its follow-up plan"
    generation=$((generation + 1))
    continue
  fi

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
    rm -f -- "$freeze" "$freeze_report"
    "$alpha_harness" "${freeze_args[@]}" >"$freeze_report"
    [[ -s "$freeze" && -s "$freeze_report" ]] || die "Campaign freeze did not produce complete evidence"
    : >"$generation_dir/frozen"
  fi
  [[ -s "$freeze" && -s "$freeze_report" ]] || die "Campaign freeze checkpoint is incomplete"

  if [[ ! -e "$generation_dir/finalized" ]]; then
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
  fi
  [[ -s "$request" && -s "$finalize_report" ]] || die "Campaign finalize checkpoint is incomplete"

  campaign_id="$(jq -er '.campaign_id' "$finalize_report")"
  request_sha256="$(jq -er '.request_sha256' "$finalize_report")"
  job_name="$(jq -er '.job_name' "$finalize_report")"
  [[ "$(sha256_file "$request")" == "$request_sha256" ]] \
    || die "Campaign request no longer matches its finalized SHA256"

  if [[ ! -e "$generation_dir/dispatched" ]]; then
    [[ -s "$submission" ]] || die "finalized Campaign is missing its resumable submission"
    "$alpha_harness" mission dispatch submit \
      --submission "$submission" \
      --context "$context" \
      --namespace "$namespace" >"$dispatch_report.partial"
    mv -f -- "$dispatch_report.partial" "$dispatch_report"
    : >"$generation_dir/dispatched"
    rm -f -- "$submission"
  fi
  [[ -s "$dispatch_report" ]] || die "Campaign dispatch checkpoint is incomplete"

  job_status="$generation_dir/job-status.json"
  pod_status="$generation_dir/pod-status.json"
  if [[ ! -e "$generation_dir/provenance-readback-complete" ]]; then
    wait_completed=true
    if ! "$kubectl_cli" --context "$context" --namespace "$namespace" wait \
      --for=condition=complete "job/$job_name" --timeout="$job_timeout"; then
      wait_completed=false
    fi
    "$kubectl_cli" --context "$context" --namespace "$namespace" get \
      "job/$job_name" -o json >"$job_status" \
      || die "Campaign Job status readback failed: $job_name"
    job_complete="$(jq -r '(.status.conditions // [] | any(.type == "Complete" and .status == "True"))' "$job_status")"
    job_failed="$(jq -r '(.status.conditions // [] | any(.type == "Failed" and .status == "True"))' "$job_status")"
    if [[ "$job_failed" == true ]]; then
      "$kubectl_cli" --context "$context" --namespace "$namespace" get pods \
        -l "job-name=$job_name" -o json >"$pod_status" || true
      "$kubectl_cli" --context "$context" --namespace "$namespace" delete \
        secret "$job_name-inputs" --ignore-not-found=true >/dev/null
      rm -f -- "$request"
      : >"$generation_dir/terminal-failure"
      die "Campaign Job failed: $job_name"
    fi
    [[ "$job_complete" == true ]] \
      || { [[ "$wait_completed" == true ]] && die "Campaign Job completion condition is missing: $job_name"; die "Campaign Job wait ended before terminal status: $job_name"; }
    pod_readback_ok=true
    if ! "$kubectl_cli" --context "$context" --namespace "$namespace" get pods \
      -l "job-name=$job_name" -o json >"$pod_status"; then
      pod_readback_ok=false
    fi
    "$kubectl_cli" --context "$context" --namespace "$namespace" delete \
      secret "$job_name-inputs" --ignore-not-found=true >/dev/null
    [[ "$pod_readback_ok" == true ]] || die "Campaign Pod status readback failed: $job_name"
    image_identity="$(jq -er '.image_identity' "$request")"
    if ! verify_kubernetes_provenance \
      "$job_status" "$pod_status" "$job_name" "$request_sha256" "$image_identity"; then
      rm -f -- "$request"
      : >"$generation_dir/terminal-failure"
      die "Campaign Pod or image provenance does not match the submitted request"
    fi
    : >"$generation_dir/provenance-readback-complete"
  fi
  image_identity="$(jq -er '.image_identity' "$request")"
  verify_kubernetes_provenance \
    "$job_status" "$pod_status" "$job_name" "$request_sha256" "$image_identity" \
    || die "saved Campaign Pod provenance is invalid"

  round_readback_dir="$generation_dir/round-readback"
  mkdir -p "$round_readback_dir"
  if [[ ! -e "$generation_dir/result-readback-complete" ]]; then
    result_url="$(jq -er '.campaign_result_readback_url' "$request")"
    oss_readback "$result_url" "$result"
    jq -e --slurpfile request_doc "$request" --arg request_sha256 "$request_sha256" '
      .schema_version == "cex-campaign-result-v6"
      and .campaign_id == $request_doc[0].campaign_id
      and .request_sha256 == $request_sha256
      and .build_source_revision == $request_doc[0].build_source_revision
      and .image_identity == $request_doc[0].image_identity
      and .campaign_inputs_sha256 == $request_doc[0].campaign_inputs_sha256
      and .producer_source_revision == $request_doc[0].producer_source_revision
      and .producer_image_identity == $request_doc[0].producer_image_identity
      and .holdout_id == $request_doc[0].holdout_id
      and .declared_total_trials == $request_doc[0].declared_total_trials
      and .stop_rule == "bounded_multi_round_single_finalize_v2"
      and (.rounds | length) == ($request_doc[0].rounds | length)
      and ([.rounds[].consumed_trials] | add // 0) == .consumed_trials
      and (all(.rounds[]; .request_sha256 == $request_sha256))
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
    done
    : >"$generation_dir/result-readback-complete"
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
  termination_reason="$(jq -er '.termination_reason' "$result")"
  observed_image_id="$(jq -er '.items[0].status.containerStatuses[] | select(.name == "alpha-campaign") | .imageID' "$pod_status")"

  jq -n \
    --argjson generation "$generation" \
    --arg campaign_id "$campaign_id" \
    --arg request_sha256 "$request_sha256" \
    --arg job_name "$job_name" \
    --arg result_sha256 "$result_sha256" \
    --arg termination_reason "$termination_reason" \
    --arg observed_image_id "$observed_image_id" \
    --argjson round_readback_count "$request_round_count" \
    '{generation:$generation,campaign_id:$campaign_id,request_sha256:$request_sha256,job_name:$job_name,campaign_result_sha256:$result_sha256,termination_reason:$termination_reason,observed_image_id:$observed_image_id,round_readback_count:$round_readback_count}' \
    >"$generation_dir/generation-report.json"

  if [[ "$termination_reason" != "campaign_no_candidate" ]]; then
    cp "$generation_dir/generation-report.json" "$work_dir/cycle-result.json"
    : >"$generation_dir/generation-complete"
    rm -f -- "$request"
    jq . "$work_dir/cycle-result.json"
    exit 0
  fi
  if ((generation == max_follow_ups)); then
    jq '. + {bounded_loop_exhausted:true}' "$generation_dir/generation-report.json" \
      >"$work_dir/cycle-result.json"
    : >"$generation_dir/generation-complete"
    rm -f -- "$request"
    jq . "$work_dir/cycle-result.json"
    exit 0
  fi

  research_plan="$generation_dir/next-research-plan.json"
  "$alpha_harness" mission campaign-learn \
    --request "$request" \
    --result "$result" \
    --result-sha256 "$result_sha256" \
    --max-tokens "$max_tokens" \
    --output "$research_plan" >"$generation_dir/learn-report.json.partial"
  mv -f -- "$generation_dir/learn-report.json.partial" "$generation_dir/learn-report.json"
  [[ -s "$research_plan" && -s "$generation_dir/learn-report.json" ]] \
    || die "Campaign learning did not produce a complete child research plan"
  : >"$generation_dir/generation-complete"
  rm -f -- "$request"
  generation=$((generation + 1))
done
