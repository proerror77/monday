#!/usr/bin/env bash
set -euo pipefail

root="$(mktemp -d)"
root="$(cd "$root" && pwd -P)"
mac_work_dir="$(mktemp -d /tmp/monday-cex-e2e.XXXXXX)"
trap 'rm -rf -- "$root" "$mac_work_dir"' EXIT
bin="$root/bin"
start_dir="$root/start"
export FAKE_STATE="$root/state"
export FAKE_REAL_RM
FAKE_REAL_RM="$(command -v rm)"
export FAKE_REAL_MV
FAKE_REAL_MV="$(command -v mv)"
mkdir "$bin" "$start_dir" "$start_dir/input" "$FAKE_STATE"
touch "$start_dir/campaign-inputs.json"

cat >"$bin/alpha-harness" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

value_after() {
  local wanted="$1"
  shift
  while (($#)); do
    if [[ "$1" == "$wanted" ]]; then
      printf '%s' "$2"
      return
    fi
    shift
  done
  exit 1
}

sha_file() {
  if command -v shasum >/dev/null; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

increment() {
  local file="$1"
  local count=0
  [[ ! -e "$file" ]] || read -r count <"$file"
  printf '%s\n' "$((count + 1))" >"$file"
}

case "$1 $2" in
  "mission campaign-freeze")
    output="$(value_after --output "$@")"
    generation=0
    [[ " $* " != *" --research-plan "* ]] || generation=1
    campaign_id="campaign-g$generation"
    object_root="https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/g$generation"
    jq -n \
      --arg campaign_id "$campaign_id" \
      --arg object_root "$object_root" \
      --argjson generation "$generation" '{
        schema_version:"cex-campaign-freeze-v1",
        campaign_inputs_sha256:("b" * 64),
        canonical_request:{
          schema_version:"cex-campaign-request-v5",
          campaign_id:$campaign_id,
          build_source_revision:("a" * 40),
          image_identity:("a" * 64),
          campaign_inputs_sha256:("b" * 64),
          producer_source_revision:("c" * 40),
          producer_image_identity:("d" * 64),
          research_plan:{
            schema_version:"cex-campaign-research-plan-v2",
            generation:$generation,
            search_policy_revision:{
              schema_version:"cex-campaign-search-policy-revision-v1",
              revision_id:("cex-search-policy-" + (if $generation == 0 then ("0" * 64) else ("1" * 64) end)),
              parent_revision_id:(if $generation == 0 then null else ("cex-search-policy-" + ("0" * 64)) end),
              position_policy:(if $generation == 0 then "cost_aware" else "prediction_identity" end)
            },
            learning_directive:(if $generation == 0 then null else {
              schema_version:"cex-campaign-learning-directive-v1",
              failure_class:"no_trades_after_costs"
            } end)
          },
          holdout_id:"holdout-test",
          declared_total_trials:4,
          rounds:[
            {
              round_id:"r1",seed:7,
              identity:{schema_version:"cex-campaign-round-identity-v1",data_window_hours:31,data_fingerprint_sha256:("1" * 64),image_identity:("a" * 64),build_source_revision:("a" * 40)},
              mission_readback_url:($object_root + "/r1/mission.json"),
              result_readback_url:($object_root + "/r1/results.zip")
            },
            {
              round_id:"r2",seed:11,
              identity:{schema_version:"cex-campaign-round-identity-v1",data_window_hours:31,data_fingerprint_sha256:("1" * 64),image_identity:("a" * 64),build_source_revision:("a" * 40)},
              mission_readback_url:($object_root + "/r2/mission.json"),
              result_readback_url:($object_root + "/r2/results.zip")
            }
          ],
          campaign_result_readback_url:($object_root + "/campaign-result.json")
        },
        signing_plan:{actions:[]}
      }' >"$output"
    jq -n --arg campaign_id "$campaign_id" '{campaign_id:$campaign_id}'
    ;;
  "mission campaign-finalize")
    freeze="$(value_after --freeze "$@")"
    request_out="$(value_after --request-out "$@")"
    submission_out="$(value_after --submission-out "$@")"
    jq '.canonical_request' "$freeze" >"$request_out"
    request_sha256="$(sha_file "$request_out")"
    generation="$(jq -r '.campaign_id | sub("campaign-g"; "")' "$request_out")"
    job_name="job-g$generation"
    jq -n --arg request_sha256 "$request_sha256" --arg job_name "$job_name" \
      '{request_sha256:$request_sha256,job_name:$job_name}' >"$submission_out"
    cp "$request_out" "$FAKE_STATE/request.json"
    jq -n \
      --arg campaign_id "campaign-g$generation" \
      --arg request_sha256 "$request_sha256" \
      --arg job_name "$job_name" \
      '{campaign_id:$campaign_id,request_sha256:$request_sha256,job_name:$job_name}'
    ;;
  "mission dispatch")
    submission="$(value_after --submission "$@")"
    jq -r '.request_sha256' "$submission" >"$FAKE_STATE/request-sha256"
    jq -r '.job_name' "$submission" >"$FAKE_STATE/job-name"
    increment "$FAKE_STATE/dispatch-count"
    printf '{"submitted":true}\n'
    ;;
  "mission campaign-learn")
    output="$(value_after --output "$@")"
    increment "$FAKE_STATE/learn-count"
    if [[ "${FAKE_LEARN_OUTCOME:-}" == "no_improvement" ]]; then
      rm -f -- "$output"
      jq -n '{failure_class:"overtrade_capacity",outcome:"no_improvement",evidence_signature:{schema_version:"cex-campaign-research-evidence-signature-v1",feature_fields_sha256:("7" * 64),factor_signatures_sha256:("8" * 64)}}'
      exit 0
    fi
    reused_existing=false
    if [[ -e "$output" ]]; then
      reused_existing=true
    else
      printf '{"schema_version":"cex-campaign-research-plan-v2"}\n' >"$output"
      increment "$FAKE_STATE/plan-count"
    fi
    if [[ "${FAKE_FAIL_AFTER_PLAN:-0}" == 1 && ! -e "$FAKE_STATE/plan-failed-once" ]]; then
      : >"$FAKE_STATE/plan-failed-once"
      exit 75
    fi
    jq -n --argjson reused_existing "$reused_existing" '{failure_class:"no_trades_after_costs",outcome:"follow_up",reused_existing:$reused_existing,evidence_signature:{schema_version:"cex-campaign-research-evidence-signature-v1",feature_fields_sha256:("7" * 64),factor_signatures_sha256:("8" * 64)},learning_directive_sha256:("9" * 64),search_policy_revision_id:("cex-search-policy-" + ("1" * 64)),research_plan_sha256:("e" * 64)}'
    ;;
  *)
    echo "unexpected alpha-harness invocation: $*" >&2
    exit 1
    ;;
esac
EOF

cat >"$bin/signer" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
freeze=""
output=""
while (($#)); do
  case "$1" in
    --freeze) freeze="$2"; shift 2 ;;
    --output) output="$2"; shift 2 ;;
    *) exit 1 ;;
  esac
done
count=0
[[ ! -e "$FAKE_STATE/signer-count" ]] || read -r count <"$FAKE_STATE/signer-count"
printf '%s\n' "$((count + 1))" >"$FAKE_STATE/signer-count"
jq '.canonical_request' "$freeze" >"$output"
EOF

cat >"$bin/kubectl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
request_sha256="$(<"$FAKE_STATE/request-sha256")"
job_name="$(<"$FAKE_STATE/job-name")"
case " $* " in
  *" wait "*)
    printf '%s\n' "$*" >>"$FAKE_STATE/job-waits"
    exit 0
    ;;
  *" get job/"*)
    jq -n --arg job_name "$job_name" --arg request_sha256 "$request_sha256" '{
      metadata:{name:$job_name,annotations:{"research.monday/request-sha256":$request_sha256}},
      status:{conditions:[{type:"Complete",status:"True"}]}
    }'
    ;;
  *" get pod/"*)
    jq -n --arg request_sha256 "$request_sha256" --arg job_name "$job_name" '{
      metadata:{annotations:{"research.monday/request-sha256":$request_sha256},ownerReferences:[{kind:"Job",name:$job_name}]},
      status:{
        phase:"Succeeded",
        containerStatuses:[{
          name:"alpha-campaign",
          imageID:("registry.example/research@sha256:" + ("a" * 64)),
          state:{terminated:{exitCode:0}}
        }]
      }
    }'
    ;;
  *" delete secret "*)
    printf '%s\n' "$job_name-inputs" >>"$FAKE_STATE/deleted-secrets"
    ;;
  *)
    echo "unexpected kubectl invocation: $*" >&2
    exit 1
    ;;
esac
EOF

cat >"$bin/aliyun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2" == "ossutil cp" ]]
[[ " $* " == *" --endpoint oss-ap-northeast-1-internal.aliyuncs.com "* ]]
[[ " $* " != *" --endpoint oss-ap-northeast-1.aliyuncs.com "* ]]
printf '%s\n' "$*" >>"$FAKE_STATE/ossutil-calls"
source_object="$3"
destination="$4"

sha_text() {
  if command -v shasum >/dev/null; then
    printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
  else
    printf '%s' "$1" | sha256sum | awk '{print $1}'
  fi
}

mkdir -p "$FAKE_STATE/oss-objects"
if [[ "$source_object" != oss://* ]]; then
  [[ " $* " == *" --forbid-overwrite "* ]]
  object_key="$(sha_text "$destination")"
  [[ ! -e "$FAKE_STATE/oss-objects/$object_key" ]] || exit 1
  cp "$source_object" "$FAKE_STATE/oss-objects/$object_key"
  if [[ "${FAKE_LOSE_PUT_RESPONSE:-0}" == 1 && ! -e "$FAKE_STATE/put-response-lost" ]]; then
    : >"$FAKE_STATE/put-response-lost"
    exit 75
  fi
  exit 0
fi
if [[ "$source_object" == *"/learning/"* ]]; then
  if [[ "${FAKE_FAIL_LEARN_READBACK:-0}" == 1 && "$source_object" == */learn-report.json && ! -e "$FAKE_STATE/learn-readback-failed" ]]; then
    : >"$FAKE_STATE/learn-readback-failed"
    exit 75
  fi
  object_key="$(sha_text "$source_object")"
  cp "$FAKE_STATE/oss-objects/$object_key" "$destination"
  exit 0
fi

generation=0
[[ "$source_object" != *"/g1/"* ]] || generation=1

if [[ "$source_object" == *"/campaign-result.json"* ]]; then
  if [[ "$generation" == 0 && ! -e "$FAKE_STATE/result-failed-once" ]]; then
    : >"$FAKE_STATE/result-failed-once"
    exit 75
  fi
  request_sha256="$(<"$FAKE_STATE/request-sha256")"
  termination="campaign_no_candidate"
  [[ "$generation" != 1 ]] || termination="campaign_finalized"
  mission_r1_sha="$(sha_text "mission-g$generation-r1")"
  mission_r2_sha="$(sha_text "mission-g$generation-r2")"
  bundle_r1_sha="$(sha_text "bundle-g$generation-r1")"
  bundle_r2_sha="$(sha_text "bundle-g$generation-r2")"
  directive_sha="$(sha_text "$(jq -c '.research_plan.learning_directive' "$FAKE_STATE/request.json")")"
  if [[ "$generation" == 1 && ! -e "$FAKE_STATE/bad-directive-digest-once" ]]; then
    : >"$FAKE_STATE/bad-directive-digest-once"
    directive_sha="$(printf '9%.0s' {1..64})"
  else
    printf '%s\n' "$directive_sha" >"$FAKE_STATE/directive-sha256"
  fi
  jq -n \
    --slurpfile request "$FAKE_STATE/request.json" \
    --arg request_sha256 "$request_sha256" \
    --arg termination "$termination" \
    --arg mission_r1_sha "$mission_r1_sha" \
    --arg mission_r2_sha "$mission_r2_sha" \
    --arg bundle_r1_sha "$bundle_r1_sha" \
    --arg bundle_r2_sha "$bundle_r2_sha" \
    --arg directive_sha "$directive_sha" \
    --argjson generation "$generation" '{
      schema_version:"cex-campaign-result-v8",
      campaign_id:$request[0].campaign_id,
      request_sha256:$request_sha256,
      build_source_revision:$request[0].build_source_revision,
      image_identity:$request[0].image_identity,
      campaign_inputs_sha256:$request[0].campaign_inputs_sha256,
      producer_source_revision:$request[0].producer_source_revision,
      producer_image_identity:$request[0].producer_image_identity,
      research_plan_sha256:("e" * 64),
      learning_directive:$request[0].research_plan.learning_directive,
      learning_directive_sha256:(if $request[0].research_plan.learning_directive == null then null else $directive_sha end),
      search_policy_revision:$request[0].research_plan.search_policy_revision,
      holdout_id:$request[0].holdout_id,
      declared_total_trials:$request[0].declared_total_trials,
      consumed_trials:2,
      stop_rule:"bounded_multi_round_single_finalize_v2",
      termination_reason:$termination,
      rounds:[
        {round_id:"r1",seed:7,identity:$request[0].rounds[0].identity,mission_sha256:$mission_r1_sha,request_sha256:$request_sha256,result_bundle_sha256:$bundle_r1_sha,result_readback_bundle_sha256:$bundle_r1_sha,consumed_trials:1},
        {round_id:"r2",seed:11,identity:$request[0].rounds[1].identity,mission_sha256:$mission_r2_sha,request_sha256:$request_sha256,result_bundle_sha256:$bundle_r2_sha,result_readback_bundle_sha256:$bundle_r2_sha,consumed_trials:1}
      ],
      selected_round_id:(if $generation == 1 then "r1" else null end),
      selected_candidate_id:(if $generation == 1 then "candidate-1" else null end),
      selected_candidate_content_hash:(if $generation == 1 then ("f" * 64) else null end),
      finalization:(if $generation == 1 then {verified:true} else null end)
    }' >"$destination"
elif [[ "$source_object" == *"/mission.json"* ]]; then
  round_id="r1"
  [[ "$source_object" != *"/r2/"* ]] || round_id="r2"
  printf '%s' "mission-g$generation-$round_id" >"$destination"
elif [[ "$source_object" == *"/results.zip"* ]]; then
  round_id="r1"
  [[ "$source_object" != *"/r2/"* ]] || round_id="r2"
  printf '%s' "bundle-g$generation-$round_id" >"$destination"
else
  echo "unexpected OSS readback: $source_object" >&2
  exit 1
fi
EOF

cat >"$bin/rm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
"$FAKE_REAL_RM" "$@"
if [[ "${FAKE_CRASH_AFTER_REQUEST_REMOVE:-0}" == 1 && ! -e "$FAKE_STATE/cleanup-crashed" ]]; then
  for argument in "$@"; do
    if [[ "$argument" == */generation-0/request.json && -s "${argument%/*}/learn-report-readback.json" ]]; then
      : >"$FAKE_STATE/cleanup-crashed"
      kill -KILL "$PPID"
      exit 137
    fi
  done
fi
EOF

cat >"$bin/mv" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
destination="${!#}"
if [[ "${FAKE_FAIL_CYCLE_SUMMARY:-0}" == 1 && "$destination" == */cycle-result.json && ! -e "$FAKE_STATE/summary-failed-once" ]]; then
  : >"$FAKE_STATE/summary-failed-once"
  exit 75
fi
"$FAKE_REAL_MV" "$@"
if [[ "${FAKE_CRASH_AFTER_COMPLETION_COMMIT:-0}" == 1 && "$destination" == */generation-complete && ! -e "$FAKE_STATE/completion-crashed" ]]; then
  : >"$FAKE_STATE/completion-crashed"
  kill -KILL "$PPID"
  exit 137
fi
EOF

cat >"$bin/uname" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == "-s" ]]
printf '%s\n' "${FAKE_UNAME:-Linux}"
EOF

chmod +x "$bin/alpha-harness" "$bin/signer" "$bin/kubectl" "$bin/aliyun" "$bin/uname" "$bin/rm" "$bin/mv"
export PATH="$bin:$PATH"

controller="$(cd "$(dirname "$0")" && pwd)/scripts/campaign-cycle-controller.sh"
source_revision="$(printf 'a%.0s' {1..40})"
image_digest="$(printf 'a%.0s' {1..64})"
controller_args=(
  start
  --alpha-harness "$bin/alpha-harness"
  --aliyun "$bin/aliyun"
  --kubectl "$bin/kubectl"
  --campaign-inputs campaign-inputs.json
  --input-root input
  --source-revision "$source_revision"
  --image "registry.example/research@sha256:$image_digest"
  --campaign-root https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns
  --signer "$bin/signer"
  --work-dir "$mac_work_dir"
  --seed 7 --seed 11
  --max-follow-ups 1
)
ack_g0_args=(
  ack-readback
  --alpha-harness "$bin/alpha-harness"
  --aliyun "$bin/aliyun"
  --kubectl "$bin/kubectl"
  --campaign-pod-name pod-g0
  --work-dir "$root/campaign-root/cycle"
)
ack_g1_args=("${ack_g0_args[@]}")
ack_g1_args[8]=pod-g1
approve_args=(
  approve
  --alpha-harness "$bin/alpha-harness"
  --aliyun "$bin/aliyun"
  --kubectl "$bin/kubectl"
  --signer "$bin/signer"
  --work-dir "$root/campaign-root/cycle"
)

if ! (cd "$start_dir" && FAKE_UNAME=Darwin "$controller" "${controller_args[@]}") \
  >"$root/start.stdout" 2>"$root/start.stderr"; then
  cat "$root/start.stderr" >&2
  exit 1
fi
test "$(jq -r '.campaign_inputs' "$mac_work_dir/controller-inputs.json")" \
  = "$start_dir/campaign-inputs.json"
test "$(jq -r '.input_root' "$mac_work_dir/controller-inputs.json")" = "$start_dir/input"
test -s "$mac_work_dir/generation-0/request.json"
test "$(<"$FAKE_STATE/signer-count")" == 1
test "$(<"$FAKE_STATE/dispatch-count")" == 1
grep -Fq 'kind: Job' "$root/start.stdout"
request_sha256="$(jq -r '.request_sha256' "$mac_work_dir/generation-0/finalize-report.json")"
grep -Fq "name: campaign-cycle-${request_sha256:0:16}" "$root/start.stdout"
grep -Fq 'research.monday/campaign-id: campaign-g0' "$root/start.stdout"
grep -Fq 'campaign-cycle-controller@sha256:REPLACE_WITH_IMMUTABLE_DIGEST' "$root/start.stdout"
grep -Fq "/campaign-root/cycles/${mac_work_dir##*/}" "$root/start.stdout"
grep -Fq 'event=stage_completed generation=0 stage=ack_handoff' "$root/start.stderr"
test ! -e "$FAKE_STATE/ossutil-calls"
test -z "$(find "$mac_work_dir" -name '*results.zip' -print -quit)"
jq -e '
  .schema_version == "monday.campaign_cycle_status.v1"
  and .checkpoint_status == "incomplete"
  and .generation == 0
  and .next_stage == "kubernetes_runtime_readback"
  and .campaign_id == "campaign-g0"
  and .job_name == "job-g0"
' < <("$controller" status --work-dir "$mac_work_dir") >/dev/null

darwin_ack_args=(
  ack-readback
  --alpha-harness "$bin/alpha-harness"
  --aliyun "$bin/aliyun"
  --kubectl "$bin/kubectl"
  --campaign-pod-name pod-g0
  --work-dir "$mac_work_dir"
)
if FAKE_UNAME=Darwin "$controller" "${darwin_ack_args[@]}" \
  >"$root/darwin.stdout" 2>"$root/darwin.stderr"; then
  echo "Darwin ACK readback unexpectedly succeeded" >&2
  exit 1
fi
grep -Fq 'stage=oss_result_readback' "$root/darwin.stderr"
grep -Fq 'OSS result readback is forbidden on Darwin' "$root/darwin.stderr"
test ! -e "$FAKE_STATE/ossutil-calls"
test -z "$(find "$mac_work_dir" -name '*results.zip' -print -quit)"

mkdir -p "$root/campaign-root"
cp -R "$mac_work_dir" "$root/campaign-root/cycle"
mv "$start_dir/campaign-inputs.json" "$root/campaign-inputs.offline"
mv "$start_dir/input" "$root/input.offline"

if "$controller" "${ack_g0_args[@]}" >"$root/first.stdout" 2>"$root/first.stderr"; then
  echo "first ACK readback unexpectedly succeeded" >&2
  exit 1
fi
grep -Fq 'schema_version=monday.research_event.v1 component=campaign-cycle-controller event=cycle_failed generation=0 stage=oss_result_readback' "$root/first.stderr"

if ! "$controller" "${ack_g0_args[@]}" >"$root/learn.stdout" 2>"$root/learn.stderr"; then
  cat "$root/learn.stderr" >&2
  exit 1
fi
grep -Fq 'event=stage_completed generation=0 stage=approval_handoff next_generation=1' "$root/learn.stderr"
test -s "$root/campaign-root/cycle/generation-0/next-research-plan.json"
test -s "$root/campaign-root/cycle/generation-0/learn-report-readback.json"
test -s "$root/campaign-root/cycle/generation-0/next-research-plan-readback.json"
grep -Fq -- '--endpoint oss-ap-northeast-1-internal.aliyuncs.com --forbid-overwrite' "$FAKE_STATE/ossutil-calls"
mv "$root/campaign-inputs.offline" "$start_dir/campaign-inputs.json"
mv "$root/input.offline" "$start_dir/input"

oss_calls_before_approve="$(wc -l <"$FAKE_STATE/ossutil-calls" | tr -d ' ')"
if ! FAKE_UNAME=Darwin "$controller" "${approve_args[@]}" \
  >"$root/approve.stdout" 2>"$root/approve.stderr"; then
  cat "$root/approve.stderr" >&2
  exit 1
fi
grep -Fq 'kind: Job' "$root/approve.stdout"
grep -Fq 'event=stage_completed generation=1 stage=ack_handoff' "$root/approve.stderr"
test "$(wc -l <"$FAKE_STATE/ossutil-calls" | tr -d ' ')" == "$oss_calls_before_approve"

# A retried parent readback must not start reading the now-dispatched child.
"$controller" "${ack_g0_args[@]}" >"$root/parent-retry.out" 2>"$root/parent-retry.err"
test "$(wc -l <"$FAKE_STATE/ossutil-calls" | tr -d ' ')" == "$oss_calls_before_approve"
test "$(<"$FAKE_STATE/dispatch-count")" == 2
grep -Fq 'stage=approval_handoff' "$root/parent-retry.err"

if "$controller" "${ack_g1_args[@]}" >"$root/second.stdout" 2>"$root/second.stderr"; then
  echo "controller accepted a mismatched learning-directive digest" >&2
  exit 1
fi
grep -Fq 'schema_version=monday.research_event.v1 component=campaign-cycle-controller event=cycle_failed generation=1 stage=oss_result_readback' "$root/second.stderr"
jq -e '
  .checkpoint_status == "incomplete"
  and .generation == 1
  and .next_stage == "oss_result_readback"
  and .campaign_id == "campaign-g1"
  and .job_name == "job-g1"
' < <("$controller" status --work-dir "$root/campaign-root/cycle") >/dev/null

if ! "$controller" "${ack_g1_args[@]}" >"$root/third.stdout" 2>"$root/third.stderr"; then
  cat "$root/third.stderr" >&2
  exit 1
fi

for event in \
  'event=cycle_started' \
  'event=generation_checkpoint_reused generation=0' \
  'event=stage_checkpoint_reused generation=1 stage=kubernetes_runtime_readback' \
  'event=round_readback_completed generation=1 round_index=0 round_id=r1' \
  'event=stage_completed generation=1 stage=oss_result_readback' \
  'event=cycle_completed generation=1 campaign_id=campaign-g1 termination_reason=campaign_finalized'; do
  grep -Fq "$event" "$root/third.stderr"
done

jq -e --arg directive_sha256 "$(<"$FAKE_STATE/directive-sha256")" \
  '.generation == 1 and .termination_reason == "campaign_finalized" and .round_readback_count == 2 and .learning_directive_sha256 == $directive_sha256 and .search_policy_revision_id == ("cex-search-policy-" + ("1" * 64))' \
  "$root/campaign-root/cycle/cycle-result.json" >/dev/null
jq -e '
  .checkpoint_status == "complete"
  and .generation == 1
  and .next_stage == null
  and .termination_reason == "campaign_finalized"
' < <("$controller" status --work-dir "$root/campaign-root/cycle") >/dev/null
test "$(<"$FAKE_STATE/signer-count")" == 2
test "$(<"$FAKE_STATE/dispatch-count")" == 2
test "$(wc -l <"$FAKE_STATE/deleted-secrets" | tr -d ' ')" == 2
test "$(grep -c -- '--timeout=7h' "$FAKE_STATE/job-waits")" == 2
for generation in 0 1; do
  test -e "$root/campaign-root/cycle/generation-$generation/provenance-readback-complete"
  test -e "$root/campaign-root/cycle/generation-$generation/result-readback-complete"
  for round_index in 0 1; do
    test -s "$root/campaign-root/cycle/generation-$generation/round-readback/round-$round_index-mission.json"
    test -s "$root/campaign-root/cycle/generation-$generation/round-readback/round-$round_index-results.zip"
  done
  for sensitive in signed-request.json request.json submission.json; do
    test ! -e "$root/campaign-root/cycle/generation-$generation/$sensitive"
  done
done
test -z "$(find "$mac_work_dir" -name '*results.zip' -print -quit)"

no_improvement_mac="$root/no-improvement-mac"
no_improvement_ack="$root/campaign-root/no-improvement"
no_improvement_args=("${controller_args[@]}")
no_improvement_args[16]=https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/no-improvement-campaigns
no_improvement_args[20]="$no_improvement_mac"
no_improvement_args[26]=3
if ! (cd "$start_dir" && FAKE_UNAME=Darwin "$controller" "${no_improvement_args[@]}") \
  >"$root/no-improvement-start.stdout" 2>"$root/no-improvement-start.stderr"; then
  cat "$root/no-improvement-start.stderr" >&2
  exit 1
fi
cp -R "$no_improvement_mac" "$no_improvement_ack"
dispatches_before_no_improvement="$(<"$FAKE_STATE/dispatch-count")"
if ! FAKE_LEARN_OUTCOME=no_improvement "$controller" ack-readback \
  --alpha-harness "$bin/alpha-harness" \
  --aliyun "$bin/aliyun" \
  --kubectl "$bin/kubectl" \
  --campaign-pod-name pod-g0 \
  --work-dir "$no_improvement_ack" \
  >"$root/no-improvement.stdout" 2>"$root/no-improvement.stderr"; then
  cat "$root/no-improvement.stderr" >&2
  exit 1
fi
jq -e '.termination_reason == "no_improvement" and .learning_outcome == "no_improvement"' \
  "$no_improvement_ack/cycle-result.json" >/dev/null
test "$(<"$FAKE_STATE/dispatch-count")" == "$dispatches_before_no_improvement"
test ! -d "$no_improvement_ack/generation-1"
test ! -e "$no_improvement_ack/generation-0/next-research-plan.json"
test -s "$no_improvement_ack/generation-0/learn-report-readback.json"

recovery_case() (
  local label="$1" fault="$2" outcome="${3:-follow_up}"
  local case_root="$root/recovery-$label"
  local case_work="$case_root/cycle"
  local request_dir="$case_work/generation-0"
  local initial_args=("${controller_args[@]}")
  export FAKE_STATE="$case_root/state"
  mkdir -p "$FAKE_STATE"
  : >"$FAKE_STATE/result-failed-once"
  initial_args[20]="$case_work"
  [[ "$outcome" != bounded ]] || initial_args[26]=0
  (cd "$start_dir" && "$controller" "${initial_args[@]}") >"$case_root/start.out" 2>"$case_root/start.err"
  local readback_args=("${ack_g0_args[@]}")
  readback_args[10]="$case_work"
  local injected_fault="$fault"
  if [[ "$fault" == TAMPER_* ]]; then
    injected_fault=FAKE_FAIL_LEARN_READBACK
  fi
  if [[ "$fault" == TAMPER_COMPLETION || "$fault" == TAMPER_EMPTY_COMPLETION ]]; then
    injected_fault=FAKE_CRASH_AFTER_COMPLETION_COMMIT
  fi

  if [[ "$fault" == FAKE_LOSE_PUT_RESPONSE ]]; then
    env "$injected_fault=1" FAKE_LEARN_OUTCOME="$outcome" "$controller" "${readback_args[@]}" >"$case_root/first.out" 2>"$case_root/first.err"
  else
    if env "$injected_fault=1" FAKE_LEARN_OUTCOME="$outcome" "$controller" "${readback_args[@]}" >"$case_root/first.out" 2>"$case_root/first.err"; then
      echo "recovery fault did not interrupt controller: $label" >&2
      return 1
    fi
  fi
  if [[ "$fault" == TAMPER_* ]]; then
    local expected_error
    if [[ "$fault" == TAMPER_COMPLETION ]]; then
      jq '.cycle_result.termination_reason = "campaign_finalized"' "$request_dir/generation-complete" >"$case_root/tampered-completion"
      cp "$case_root/tampered-completion" "$request_dir/generation-complete"
      expected_error='saved Campaign completion checkpoint is invalid or unsupported'
    elif [[ "$fault" == TAMPER_EMPTY_COMPLETION ]]; then
      : >"$request_dir/generation-complete"
      expected_error='saved Campaign completion checkpoint is invalid or unsupported'
    elif [[ "$fault" == TAMPER_LEARN_REPORT ]]; then
      printf '\n' >>"$request_dir/learn-report.json"
      expected_error='saved Campaign learning checkpoint is invalid'
    else
      local objects=("$FAKE_STATE/oss-objects/"*)
      test "${#objects[@]}" == 1
      printf '\n' >>"${objects[0]}"
      expected_error='published learn artifact readback SHA256 mismatch'
    fi
    if "$controller" "${readback_args[@]}" >"$case_root/rejected.out" 2>"$case_root/rejected.err"; then
      echo "controller accepted corrupted learning evidence: $label" >&2
      return 1
    fi
    grep -Fq "$expected_error" "$case_root/rejected.err"
    test "$(<"$FAKE_STATE/learn-count")" == 1
    test "$(<"$FAKE_STATE/dispatch-count")" == 1
    test -s "$request_dir/request.json"
    if [[ "$fault" != TAMPER_COMPLETION && "$fault" != TAMPER_EMPTY_COMPLETION ]]; then
      test ! -e "$request_dir/generation-complete"
    else
      if "$controller" status --work-dir "$case_work" >"$case_root/status.out" 2>"$case_root/status.err"; then
        echo "status accepted an invalid completion: $label" >&2
        return 1
      fi
      grep -Fq "$expected_error" "$case_root/status.err"
    fi
    printf 'campaign recovery %s: PASS\n' "$label"
    return
  fi
  if [[ "$fault" == FAKE_FAIL_CYCLE_SUMMARY || "$fault" == FAKE_CRASH_AFTER_COMPLETION_COMMIT ]]; then
    test -s "$request_dir/generation-complete"
    if [[ "$outcome" != follow_up ]]; then
      test ! -e "$case_work/cycle-result.json"
      local expected_termination=no_improvement
      [[ "$outcome" != bounded ]] || expected_termination=campaign_no_candidate
      jq -e --arg termination "$expected_termination" '.checkpoint_status == "complete" and .termination_reason == $termination' \
        < <("$controller" status --work-dir "$case_work") >/dev/null
    fi
  fi
  if ! FAKE_LEARN_OUTCOME="$outcome" "$controller" "${readback_args[@]}" >"$case_root/resumed.out" 2>"$case_root/resumed.err"; then
    echo "controller failed to recover: $label" >&2
    cat "$case_root/resumed.err" >&2
    return 1
  fi
  test "$(<"$FAKE_STATE/dispatch-count")" == 1
  test "$(<"$FAKE_STATE/signer-count")" == 1
  test ! -d "$case_work/generation-1"
  test ! -e "$request_dir/request.json"
  if [[ "$outcome" != bounded ]]; then
    cmp "$request_dir/learn-report.json" "$request_dir/learn-report-readback.json"
  fi
  if [[ "$outcome" == follow_up ]]; then
    test "$(<"$FAKE_STATE/plan-count")" == 1
    cmp "$request_dir/next-research-plan.json" "$request_dir/next-research-plan-readback.json"
    jq -e '.next_stage == "next_generation"' < <("$controller" status --work-dir "$case_work") >/dev/null
  elif [[ "$outcome" == no_improvement ]]; then
    jq -e '.termination_reason == "no_improvement"' "$case_work/cycle-result.json" >/dev/null
  else
    jq -e '.termination_reason == "campaign_no_candidate" and .bounded_loop_exhausted == true' "$case_work/cycle-result.json" >/dev/null
    test ! -e "$FAKE_STATE/learn-count"
    test ! -e "$request_dir/learning-checkpoint.json"
  fi
  if [[ "$fault" == FAKE_FAIL_AFTER_PLAN ]]; then
    test "$(<"$FAKE_STATE/learn-count")" == 2
  elif [[ "$outcome" != bounded ]]; then
    test "$(<"$FAKE_STATE/learn-count")" == 1
  fi
  printf 'campaign recovery %s: PASS\n' "$label"
)

selected_recovery=false
for scenario in \
  'report-readback FAKE_FAIL_LEARN_READBACK' \
  'plan-written FAKE_FAIL_AFTER_PLAN' \
  'lost-put-response FAKE_LOSE_PUT_RESPONSE' \
  'follow-up-cleanup FAKE_CRASH_AFTER_REQUEST_REMOVE' \
  'terminal-cleanup FAKE_CRASH_AFTER_REQUEST_REMOVE no_improvement' \
  'follow-up-commit FAKE_CRASH_AFTER_COMPLETION_COMMIT' \
  'terminal-commit FAKE_CRASH_AFTER_COMPLETION_COMMIT no_improvement' \
  'cycle-summary FAKE_FAIL_CYCLE_SUMMARY no_improvement' \
  'bounded-commit FAKE_CRASH_AFTER_COMPLETION_COMMIT bounded' \
  'changed-local-report TAMPER_LEARN_REPORT' \
  'changed-remote-report TAMPER_REMOTE_REPORT' \
  'changed-completion TAMPER_COMPLETION no_improvement' \
  'empty-completion TAMPER_EMPTY_COMPLETION no_improvement'; do
  read -r -a scenario_args <<<"$scenario"
  [[ -z "${CAMPAIGN_RECOVERY_SCENARIO:-}" || "$CAMPAIGN_RECOVERY_SCENARIO" == "${scenario_args[0]}" ]] || continue
  selected_recovery=true
  recovery_case "${scenario_args[@]}"
done
[[ "$selected_recovery" == true ]]
echo "campaign cycle controller test: PASS"
