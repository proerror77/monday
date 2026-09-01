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
  --signer EXECUTABLE --work-dir NEW_DIR --seed N --seed N \
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
[[ ! -e "$work_dir" ]] || die "--work-dir must not already exist"
[[ -x "$signer" ]] || die "signer is not executable: $signer"
command -v "$alpha_harness" >/dev/null || die "alpha-harness executable not found"
command -v "$aliyun_cli" >/dev/null || die "aliyun executable not found"
command -v "$kubectl_cli" >/dev/null || die "kubectl executable not found"
command -v jq >/dev/null || die "jq is required"

mkdir -p "$work_dir"
sensitive_files=()
cleanup_sensitive_files() {
  local path
  for path in "${sensitive_files[@]}"; do
    [[ ! -e "$path" ]] || rm -f -- "$path"
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

research_plan=""
generation=0
while ((generation <= max_follow_ups)); do
  generation_dir="$work_dir/generation-$generation"
  mkdir "$generation_dir"
  freeze="$generation_dir/freeze.json"
  freeze_report="$generation_dir/freeze-report.json"
  signed_request="$generation_dir/signed-request.json"
  request="$generation_dir/request.json"
  submission="$generation_dir/submission.json"
  finalize_report="$generation_dir/finalize-report.json"
  dispatch_report="$generation_dir/dispatch-report.json"
  result="$generation_dir/campaign-result.json"
  sensitive_files+=("$signed_request" "$request" "$submission")

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
  "$alpha_harness" "${freeze_args[@]}" >"$freeze_report"

  "$signer" --freeze "$freeze" --output "$signed_request" >/dev/null
  [[ -s "$signed_request" ]] || die "signer did not create a signed request"

  "$alpha_harness" mission campaign-finalize \
    --freeze "$freeze" \
    --signed-request "$signed_request" \
    --attempt-id "research-cycle-g$generation" \
    --image "$image" \
    --request-out "$request" \
    --submission-out "$submission" >"$finalize_report"

  campaign_id="$(jq -er '.campaign_id' "$finalize_report")"
  request_sha256="$(jq -er '.request_sha256' "$finalize_report")"
  job_name="$(jq -er '.job_name' "$finalize_report")"

  "$alpha_harness" mission dispatch submit \
    --submission "$submission" \
    --context "$context" \
    --namespace "$namespace" >"$dispatch_report"
  rm -f -- "$signed_request" "$submission"

  if ! "$kubectl_cli" --context "$context" --namespace "$namespace" wait \
    --for=condition=complete "job/$job_name" --timeout="$job_timeout"; then
    "$kubectl_cli" --context "$context" --namespace "$namespace" get \
      "job/$job_name" -o json >"$generation_dir/job-status.json" || true
    die "Campaign Job did not complete: $job_name"
  fi
  "$kubectl_cli" --context "$context" --namespace "$namespace" get \
    "job/$job_name" -o json >"$generation_dir/job-status.json"

  result_url="$(jq -er '.campaign_result_readback_url' "$request")"
  canonical_result_url="${result_url%%\?*}"
  [[ "$canonical_result_url" == https://*/* ]] || die "Campaign result URL is not canonical HTTPS"
  host_and_key="${canonical_result_url#https://}"
  host="${host_and_key%%/*}"
  key="${host_and_key#*/}"
  bucket="${host%%.*}"
  [[ "$host" == "$bucket.oss-ap-northeast-1-internal.aliyuncs.com" ]] \
    || die "Campaign result URL is outside Tokyo internal OSS"
  "$aliyun_cli" ossutil cp "oss://$bucket/$key" "$result" \
    --endpoint oss-ap-northeast-1.aliyuncs.com

  result_sha256="$(sha256_file "$result")"
  jq -e --arg campaign_id "$campaign_id" --arg request_sha256 "$request_sha256" \
    '.campaign_id == $campaign_id and .request_sha256 == $request_sha256' \
    "$result" >/dev/null || die "Campaign result identity does not match the submitted request"
  termination_reason="$(jq -er '.termination_reason' "$result")"

  jq -n \
    --argjson generation "$generation" \
    --arg campaign_id "$campaign_id" \
    --arg request_sha256 "$request_sha256" \
    --arg job_name "$job_name" \
    --arg result_sha256 "$result_sha256" \
    --arg termination_reason "$termination_reason" \
    '{generation:$generation,campaign_id:$campaign_id,request_sha256:$request_sha256,job_name:$job_name,campaign_result_sha256:$result_sha256,termination_reason:$termination_reason}' \
    >"$generation_dir/generation-report.json"

  if [[ "$termination_reason" != "campaign_no_candidate" ]]; then
    cp "$generation_dir/generation-report.json" "$work_dir/cycle-result.json"
    jq . "$work_dir/cycle-result.json"
    exit 0
  fi
  if ((generation == max_follow_ups)); then
    jq '. + {bounded_loop_exhausted:true}' "$generation_dir/generation-report.json" \
      >"$work_dir/cycle-result.json"
    jq . "$work_dir/cycle-result.json"
    exit 0
  fi

  research_plan="$generation_dir/next-research-plan.json"
  "$alpha_harness" mission campaign-learn \
    --request "$request" \
    --result "$result" \
    --result-sha256 "$result_sha256" \
    --max-tokens "$max_tokens" \
    --output "$research_plan" >"$generation_dir/learn-report.json"
  rm -f -- "$request"
  generation=$((generation + 1))
done
