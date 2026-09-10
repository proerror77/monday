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
  "mission prepare-fresh-inputs")
    report="$(value_after --report-out "$@")"
    campaign_inputs="$(value_after --campaign-inputs-out "$@")"
    output_root="$(value_after --output-root "$@")"
    output_prefix="$(value_after --output-prefix "$@")"
    market="$(value_after --market "$@")"
    case "$market" in
      spot|usdm) ;;
      *) echo "unexpected fresh market: $market" >&2; exit 1 ;;
    esac
    printf '%s\n' "$market" >>"$FAKE_STATE/fresh-markets"
    if [[ " $* " == *" --binary-dir "* ]]; then
      printf 'binary:%s\n' "$(value_after --binary-dir "$@")" >>"$FAKE_STATE/fresh-binary-dirs"
    fi
    symbol="$(value_after --symbol "$@")"
    mission_id="$(value_after --mission-id "$@")"
    request_out="$(value_after --request-out "$@")"
    mkdir -p "$(dirname "$campaign_inputs")" "$(dirname "$report")"
    mkdir -p "$(dirname "$request_out")"
    if [[ ! -e "$report" ]]; then
      increment "$FAKE_STATE/fresh-preparation-count"
    fi
    printf '{}\n' >"$campaign_inputs"
    inventory_out="$(value_after --inventory-out "$@")"
    printf 'RUN_ID=inventory-fresh\n' >"$inventory_out"
    printf '{}\n' >"$request_out"
    campaign_inputs_sha256="$(sha_file "$campaign_inputs")"
    run_root="$output_root/$output_prefix"
    jq -n \
      --arg source_revision "$FAKE_SOURCE_REVISION" \
      --arg image_ref "$(value_after --image-ref "$@")" \
      --arg symbol "$symbol" \
      --arg mission_id "$mission_id" \
      --arg output_prefix "$output_prefix" \
      --arg run_root "$run_root" \
      --arg campaign_inputs "$campaign_inputs" \
      --arg request_path "$request_out" \
      --arg request_sha256 "$(sha_file "$request_out")" \
      --arg inventory_sha256 "$(sha_file "$inventory_out")" \
      --arg campaign_inputs_sha256 "$campaign_inputs_sha256" \
      '{
        schema_version:"monday.cex_fresh_inputs_preparation.v1",
        status:"ready",source_revision:$source_revision,image_ref:$image_ref,
        symbol:$symbol,mission_id:$mission_id,
        start_received_at_ns:1,end_received_at_ns:2,
        max_inputs:4,max_input_bytes:1000000,
        input_root:$run_root,run_root:$run_root,output_prefix:$output_prefix,
        inventory_path:"inventory.env",request_path:$request_path,
        request_sha256:$request_sha256,campaign_inputs_path:$campaign_inputs,
        inventory_sha256:$inventory_sha256,input_fingerprint_sha256:("b" * 64),
        campaign_inputs_sha256:$campaign_inputs_sha256,
        materialization_receipt_sha256:("c" * 64),
        feature_sha256:("d" * 64),materialization_sha256:("e" * 64),
        replay_artifact_sha256:("f" * 64),replay_manifest_sha256:("0" * 64)
      }' >"$report"
    printf '{"status":"ready","reused_existing":true}\n'
    ;;
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
  "mission campaign-study-propose")
    output="$(value_after --output "$@")"
    plan_output="$(value_after --research-plan-output "$@")"
    increment "$FAKE_STATE/study-propose-count"
    if [[ "${FAKE_FAIL_STUDY_PROPOSE_ONCE:-0}" == 1 \
      && ! -e "$FAKE_STATE/study-propose-failed-once" ]] \
      || [[ -e "$FAKE_STATE/fail-study-propose-once" ]]; then
      : >"$FAKE_STATE/study-propose-failed-once"
      rm -f -- "$FAKE_STATE/fail-study-propose-once"
      printf '{"status":"'
      exit 75
    fi
    if [[ "${FAKE_STUDY_PROPOSAL_STATUS:-ready}" == needs_authority ]]; then
      jq -n --arg reason "${FAKE_STUDY_PROPOSAL_REASON:-target_family_is_not_a_predeclared_study_member}" \
        '{schema_version:"monday.campaign_study_proposal_report.v1",status:"needs_authority",reason:$reason}'
    else
      printf '{"study_proposal":true}\n' >"$output"
      printf '{"schema_version":"cex-campaign-research-plan-v2"}\n' >"$plan_output"
      jq -n --arg proposal "$output" --arg plan "$plan_output" \
        '{schema_version:"monday.campaign_study_proposal_report.v1",status:"ready",proposal_path:$proposal,research_plan_path:$plan}'
    fi
    ;;
  "mission dispatch")
    submission="$(value_after --submission "$@")"
    if [[ "$3" == settle ]]; then
      if [[ "${FAKE_FAIL_SETTLEMENT_ONCE:-0}" == 1 && ! -e "$FAKE_STATE/settlement-failed-once" ]]; then
        : >"$FAKE_STATE/settlement-failed-once"
        exit 75
      fi
      job_name="$(jq -r '.job_name' "$submission")"
      if [[ ! -e "$FAKE_STATE/settled-$job_name" ]]; then
        : >"$FAKE_STATE/settled-$job_name"
        increment "$FAKE_STATE/settlement-count"
      fi
      result_sha256="$(sha_file "${submission%/*}/campaign-result.json")"
      jq -n --arg request "$(jq -r '.request_sha256' "$submission")" --arg result "$result_sha256" \
        '{status:"settled",request_sha256:$request,campaign_result_sha256:$result}'
      exit 0
    fi
    [[ "$3" == submit ]]
    jq -r '.request_sha256' "$submission" >"$FAKE_STATE/request-sha256"
    jq -r '.job_name' "$submission" >"$FAKE_STATE/job-name"
    increment "$FAKE_STATE/dispatch-count"
    printf '{"submitted":true}\n'
    ;;
  "mission campaign-learn")
    [[ -e "$FAKE_STATE/settled-$(<"$FAKE_STATE/job-name")" ]] || { echo "learning before ledger settlement" >&2; exit 1; }
    [[ " $* " != *" --max-tokens "* ]]
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
  if [[ "$generation" == 1 && "${FAKE_STUDY_TARGET_NO_CANDIDATE:-0}" != 1 ]]; then
    termination="campaign_finalized"
  fi
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
      selected_round_id:(if $termination == "campaign_finalized" then "r1" else null end),
      selected_candidate_id:(if $termination == "campaign_finalized" then "candidate-1" else null end),
      selected_candidate_content_hash:(if $termination == "campaign_finalized" then ("f" * 64) else null end),
      finalization:(if $termination == "campaign_finalized" then {verified:true} else null end)
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
printf '{}\n' >"$bin/control"
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
test "$(jq 'has("max_tokens")' "$mac_work_dir/controller-inputs.json")" = false
test -s "$mac_work_dir/generation-0/request.json"
test "$(<"$FAKE_STATE/signer-count")" == 1
test "$(<"$FAKE_STATE/dispatch-count")" == 1
grep -Fq 'kind: Job' "$root/start.stdout"
request_sha256="$(jq -r '.request_sha256' "$mac_work_dir/generation-0/finalize-report.json")"
grep -Fq "name: campaign-cycle-${request_sha256:0:16}" "$root/start.stdout"
grep -Fq 'research.monday/campaign-id: campaign-g0' "$root/start.stdout"
grep -Fq 'campaign-cycle-controller@sha256:REPLACE_WITH_IMMUTABLE_DIGEST' "$root/start.stdout"
grep -Fq "/campaign-root/cycles/${mac_work_dir##*/}" "$root/start.stdout"
if grep -Fq 'REPLACE_RESEARCH_LEARNING_SECRET' "$root/start.stdout"; then
  echo "controller handoff still requires LLM credentials" >&2
  exit 1
fi
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
# Existing checkpoints retain the obsolete token budget as audit history.
legacy_state="$root/campaign-root/cycle/controller-inputs.json"
jq '. + {max_tokens:300}' "$legacy_state" >"$root/legacy-controller-inputs.json"
cp "$root/legacy-controller-inputs.json" "$legacy_state"
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
# Removing the token budget must not weaken the immutable input binding.
printf 'changed input' >"$start_dir/campaign-inputs.json"
if FAKE_UNAME=Darwin "$controller" "${approve_args[@]}" \
  >"$root/drift.stdout" 2>"$root/drift.stderr"; then
  echo "controller accepted changed inputs in a legacy checkpoint" >&2
  exit 1
fi
grep -Fq 'existing work directory belongs to different controller inputs' "$root/drift.stderr"
test "$(<"$FAKE_STATE/dispatch-count")" == 1
: >"$start_dir/campaign-inputs.json"
if ! FAKE_UNAME=Darwin "$controller" "${approve_args[@]}" \
  >"$root/approve.stdout" 2>"$root/approve.stderr"; then
  cat "$root/approve.stderr" >&2
  exit 1
fi
grep -Fq 'kind: Job' "$root/approve.stdout"
cmp -s "$legacy_state" "$root/legacy-controller-inputs.json"
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
test ! -e "$FAKE_STATE/deleted-secrets"
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
  local fake_state="$case_root/state"
  mkdir -p "$fake_state"
  : >"$fake_state/result-failed-once"
  initial_args[20]="$case_work"
  [[ "$outcome" != bounded ]] || initial_args[26]=0
  (cd "$start_dir" && FAKE_STATE="$fake_state" "$controller" "${initial_args[@]}") >"$case_root/start.out" 2>"$case_root/start.err"
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
    FAKE_STATE="$fake_state" env "$injected_fault=1" FAKE_LEARN_OUTCOME="$outcome" "$controller" "${readback_args[@]}" >"$case_root/first.out" 2>"$case_root/first.err"
  else
    if FAKE_STATE="$fake_state" env "$injected_fault=1" FAKE_LEARN_OUTCOME="$outcome" "$controller" "${readback_args[@]}" >"$case_root/first.out" 2>"$case_root/first.err"; then
      echo "recovery fault did not interrupt controller: $label" >&2
      return 1
    fi
  fi
  if [[ "$fault" == FAKE_FAIL_SETTLEMENT_ONCE ]]; then
    test -e "$request_dir/result-readback-complete"
    test ! -s "$request_dir/settlement-report.json"
    jq -e '.checkpoint_status == "incomplete" and .next_stage == "ledger_settlement"' \
      < <(FAKE_STATE="$fake_state" "$controller" status --work-dir "$case_work") >/dev/null
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
      local objects=("$fake_state/oss-objects/"*)
      test "${#objects[@]}" == 1
      printf '\n' >>"${objects[0]}"
      expected_error='published learn artifact readback SHA256 mismatch'
    fi
    if FAKE_STATE="$fake_state" "$controller" "${readback_args[@]}" >"$case_root/rejected.out" 2>"$case_root/rejected.err"; then
      echo "controller accepted corrupted learning evidence: $label" >&2
      return 1
    fi
    grep -Fq "$expected_error" "$case_root/rejected.err"
    test "$(<"$fake_state/learn-count")" == 1
    test "$(<"$fake_state/dispatch-count")" == 1
    test -s "$request_dir/request.json"
    if [[ "$fault" != TAMPER_COMPLETION && "$fault" != TAMPER_EMPTY_COMPLETION ]]; then
      test ! -e "$request_dir/generation-complete"
    else
      if FAKE_STATE="$fake_state" "$controller" status --work-dir "$case_work" >"$case_root/status.out" 2>"$case_root/status.err"; then
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
        < <(FAKE_STATE="$fake_state" "$controller" status --work-dir "$case_work") >/dev/null
    fi
  fi
  if ! FAKE_STATE="$fake_state" FAKE_LEARN_OUTCOME="$outcome" "$controller" "${readback_args[@]}" >"$case_root/resumed.out" 2>"$case_root/resumed.err"; then
    echo "controller failed to recover: $label" >&2
    cat "$case_root/resumed.err" >&2
    return 1
  fi
  test "$(<"$fake_state/dispatch-count")" == 1
  test "$(<"$fake_state/signer-count")" == 1
  test ! -d "$case_work/generation-1"
  test ! -e "$request_dir/request.json"
  test ! -e "$request_dir/submission.json"
  test "$(<"$fake_state/settlement-count")" == 1
  if [[ "$outcome" != bounded ]]; then
    cmp "$request_dir/learn-report.json" "$request_dir/learn-report-readback.json"
  fi
  if [[ "$outcome" == follow_up ]]; then
    test "$(<"$fake_state/plan-count")" == 1
    cmp "$request_dir/next-research-plan.json" "$request_dir/next-research-plan-readback.json"
    jq -e '.next_stage == "next_generation"' < <(FAKE_STATE="$fake_state" "$controller" status --work-dir "$case_work") >/dev/null
  elif [[ "$outcome" == no_improvement ]]; then
    jq -e '.termination_reason == "no_improvement"' "$case_work/cycle-result.json" >/dev/null
  else
    jq -e '.termination_reason == "campaign_no_candidate" and .bounded_loop_exhausted == true' "$case_work/cycle-result.json" >/dev/null
    test ! -e "$fake_state/learn-count"
    test ! -e "$request_dir/learning-checkpoint.json"
  fi
  if [[ "$fault" == FAKE_FAIL_AFTER_PLAN ]]; then
    test "$(<"$fake_state/learn-count")" == 2
  elif [[ "$outcome" != bounded ]]; then
    test "$(<"$fake_state/learn-count")" == 1
  fi
  printf 'campaign recovery %s: PASS\n' "$label"
)

selected_recovery=false
for scenario in \
  'settlement-readback FAKE_FAIL_SETTLEMENT_ONCE' \
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

fresh_case_root="$root/fresh-inputs"
fresh_cycle="$fresh_case_root/cycle"
fresh_output="$fresh_case_root/output"
mkdir -p "$fresh_case_root/raw" "$fresh_case_root/reference" "$fresh_output"
export FAKE_SOURCE_REVISION="$source_revision"
fresh_args=(
  start
  --alpha-harness "$bin/alpha-harness"
  --aliyun "$bin/aliyun"
  --kubectl "$bin/kubectl"
  --fresh-inputs
  --fresh-raw-root "$fresh_case_root/raw"
  --fresh-reference-root "$fresh_case_root/reference"
  --fresh-market usdm
  --fresh-start-received-at-ns 1700000000000000000
  --fresh-end-received-at-ns 1700000060000000000
  --fresh-symbol BTCUSDT
  --fresh-image-ref "registry.example/research@sha256:$image_digest"
  --fresh-mission-id fresh-window
  --fresh-output-root "$fresh_output"
  --fresh-output-prefix campaign-inputs/fresh-window
  --fresh-bucket-ms 1000
  --fresh-label-horizon-buckets 5
  --fresh-top-depth 5
  --fresh-materializer "$bin/signer"
  --fresh-binary-dir "$bin"
  --fresh-max-scan-entries 100
  --fresh-max-inputs 4
  --fresh-max-input-bytes 1000000
  --fresh-materializer-timeout-seconds 10
  --fresh-max-materializer-output-bytes 1024
  --source-revision "$source_revision"
  --image "registry.example/research@sha256:$image_digest"
  --campaign-root https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns
  --work-dir "$fresh_cycle"
  --seed 7 --seed 11
  --max-follow-ups 1
)
fresh_dispatch_before="$(<"$FAKE_STATE/dispatch-count")"
if ! FAKE_UNAME=Darwin "$controller" "${fresh_args[@]}" \
  >"$root/fresh-needs-authority.stdout" 2>"$root/fresh-needs-authority.stderr"; then
  cat "$root/fresh-needs-authority.stderr" >&2
  exit 1
fi
jq -e '
  .status == "needs_authority"
  and .reason == "signer_missing"
  and .resume_ready == true
' "$fresh_cycle/needs-authority.json" >/dev/null
test "$(<"$FAKE_STATE/fresh-preparation-count")" == 1
test "$(<"$FAKE_STATE/dispatch-count")" == "$fresh_dispatch_before"
test ! -e "$fresh_cycle/generation-0"
grep -Fq 'event=needs_authority stage=fresh_inputs reason=signer_missing' \
  "$root/fresh-needs-authority.stderr"

if ! FAKE_UNAME=Darwin "$controller" "${fresh_args[@]}" \
  >"$root/fresh-restart.stdout" 2>"$root/fresh-restart.stderr"; then
  cat "$root/fresh-restart.stderr" >&2
  exit 1
fi
test "$(<"$FAKE_STATE/fresh-preparation-count")" == 1
test "$(<"$FAKE_STATE/dispatch-count")" == "$fresh_dispatch_before"

# A changed fresh contract is rejected before preparation can write the
# output prefix or invoke the materializer. This exercises the preflight
# ownership check against the already persisted controller state.
mismatched_fresh_args=("${fresh_args[@]}")
for ((mismatch_index = 0; mismatch_index < ${#mismatched_fresh_args[@]}; mismatch_index++)); do
  if [[ "${mismatched_fresh_args[mismatch_index]}" == "--fresh-output-prefix" ]]; then
    mismatched_fresh_args[mismatch_index + 1]="campaign-inputs/changed-window"
    break
  fi
done
fresh_prepare_before_mismatch="$(<"$FAKE_STATE/fresh-preparation-count")"
if FAKE_UNAME=Darwin "$controller" "${mismatched_fresh_args[@]}" \
  >"$root/fresh-mismatch.stdout" 2>"$root/fresh-mismatch.stderr"; then
  cat "$root/fresh-mismatch.stderr" >&2
  exit 1
fi
test "$(<"$FAKE_STATE/fresh-preparation-count")" == "$fresh_prepare_before_mismatch"
test ! -e "$fresh_output/campaign-inputs/changed-window"
grep -Fq 'different fresh controller inputs' "$root/fresh-mismatch.stderr"

if ! FAKE_UNAME=Darwin "$controller" approve \
  --alpha-harness "$bin/alpha-harness" \
  --aliyun "$bin/aliyun" \
  --kubectl "$bin/kubectl" \
  --signer "$bin/signer" \
  --control "$bin/control" \
  --work-dir "$fresh_cycle" \
  >"$root/fresh-approve.stdout" 2>"$root/fresh-approve.stderr"; then
  cat "$root/fresh-approve.stderr" >&2
  exit 1
fi
test "$(<"$FAKE_STATE/fresh-preparation-count")" == 1
test "$(<"$FAKE_STATE/dispatch-count")" == "$((fresh_dispatch_before + 1))"
grep -Fq 'event=stage_completed generation=0 stage=ack_handoff' "$root/fresh-approve.stderr"

fresh_latest_case_root="$root/fresh-latest-inputs"
fresh_latest_cycle="$fresh_latest_case_root/cycle"
fresh_latest_output="$fresh_latest_case_root/output"
mkdir -p "$fresh_latest_case_root/raw" "$fresh_latest_case_root/reference" "$fresh_latest_output"
fresh_latest_args=(
  start
  --alpha-harness "$bin/alpha-harness"
  --aliyun "$bin/aliyun"
  --kubectl "$bin/kubectl"
  --fresh-inputs
  --fresh-raw-root "$fresh_latest_case_root/raw"
  --fresh-reference-root "$fresh_latest_case_root/reference"
  --fresh-market usdm
  --fresh-duration-ns 5000
  --fresh-cutoff-received-at-ns 1700000060000000000
  --fresh-max-candidates 8
  --fresh-symbol BTCUSDT
  --fresh-image-ref "registry.example/research@sha256:$image_digest"
  --fresh-mission-id fresh-latest-window
  --fresh-output-root "$fresh_latest_output"
  --fresh-output-prefix campaign-inputs/fresh-latest-window
  --fresh-bucket-ms 1000
  --fresh-label-horizon-buckets 5
  --fresh-top-depth 5
  --fresh-materializer "$bin/signer"
  --fresh-binary-dir "$bin"
  --fresh-max-scan-entries 100
  --fresh-max-inputs 4
  --fresh-max-input-bytes 1000000
  --fresh-materializer-timeout-seconds 10
  --fresh-max-materializer-output-bytes 1024
  --source-revision "$source_revision"
  --image "registry.example/research@sha256:$image_digest"
  --campaign-root https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns
  --work-dir "$fresh_latest_cycle"
  --seed 7 --seed 11
  --max-follow-ups 1
)
latest_dispatch_before="$(<"$FAKE_STATE/dispatch-count")"
if ! FAKE_UNAME=Darwin "$controller" "${fresh_latest_args[@]}" \
  >"$root/fresh-latest-needs-authority.stdout" 2>"$root/fresh-latest-needs-authority.stderr"; then
  cat "$root/fresh-latest-needs-authority.stderr" >&2
  exit 1
fi
jq -e \
  '.input_mode == "fresh"
   and .fresh.duration_ns == "5000"
   and .fresh.cutoff_received_at_ns == "1700000060000000000"
   and .fresh.max_candidates == "8"' \
  "$fresh_latest_cycle/controller-inputs.json" >/dev/null
test "$(<"$FAKE_STATE/dispatch-count")" == "$latest_dispatch_before"
test "$(<"$FAKE_STATE/fresh-preparation-count")" == 2

fresh_control_cycle="$fresh_case_root/control-cycle"
fresh_control_output="$fresh_case_root/control-output"
mkdir -p "$fresh_case_root/control-raw" "$fresh_case_root/control-reference" "$fresh_control_output"
fresh_control_args=(
  start
  --alpha-harness "$bin/alpha-harness"
  --aliyun "$bin/aliyun"
  --kubectl "$bin/kubectl"
  --fresh-inputs
  --fresh-raw-root "$fresh_case_root/control-raw"
  --fresh-reference-root "$fresh_case_root/control-reference"
  --fresh-market spot
  --fresh-start-received-at-ns 1700000000000000000
  --fresh-end-received-at-ns 1700000060000000000
  --fresh-symbol BTCUSDT
  --fresh-image-ref "registry.example/research@sha256:$image_digest"
  --fresh-mission-id fresh-control-window
  --fresh-output-root "$fresh_control_output"
  --fresh-output-prefix campaign-inputs/fresh-control-window
  --fresh-bucket-ms 1000
  --fresh-label-horizon-buckets 5
  --fresh-top-depth 5
  --fresh-materializer "$bin/signer"
  --fresh-binary-dir "$bin"
  --fresh-max-scan-entries 100
  --fresh-max-inputs 4
  --fresh-max-input-bytes 1000000
  --fresh-materializer-timeout-seconds 10
  --fresh-max-materializer-output-bytes 1024
  --source-revision "$source_revision"
  --image "registry.example/research@sha256:$image_digest"
  --campaign-root https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns
  --signer "$bin/signer"
  --work-dir "$fresh_control_cycle"
  --seed 7 --seed 11
  --max-follow-ups 1
)
control_dispatch_before="$(<"$FAKE_STATE/dispatch-count")"
if ! FAKE_UNAME=Darwin "$controller" "${fresh_control_args[@]}" \
  >"$root/fresh-control-needs-authority.stdout" 2>"$root/fresh-control-needs-authority.stderr"; then
  cat "$root/fresh-control-needs-authority.stderr" >&2
  exit 1
fi
jq -e '
  .status == "needs_authority"
  and .reason == "dispatch_control_missing"
  and .sign_finalize_dispatch_preserved == true
' "$fresh_control_cycle/generation-0/needs-authority.json" >/dev/null
jq -e '.fresh.market == "spot"' "$fresh_control_cycle/controller-inputs.json" >/dev/null
test -s "$fresh_control_cycle/generation-0/request.json"
test -s "$fresh_control_cycle/generation-0/submission.json"
test "$(<"$FAKE_STATE/dispatch-count")" == "$control_dispatch_before"

if ! FAKE_UNAME=Darwin "$controller" approve \
  --alpha-harness "$bin/alpha-harness" \
  --aliyun "$bin/aliyun" \
  --kubectl "$bin/kubectl" \
  --signer "$bin/signer" \
  --control "$bin/control" \
  --work-dir "$fresh_control_cycle" \
  >"$root/fresh-control-approve.stdout" 2>"$root/fresh-control-approve.stderr"; then
  cat "$root/fresh-control-approve.stderr" >&2
  exit 1
fi
test "$(<"$FAKE_STATE/dispatch-count")" == "$((control_dispatch_before + 1))"
jq -e '.fresh.market == "spot"' "$fresh_control_cycle/controller-inputs.json" >/dev/null
grep -Fqx spot "$FAKE_STATE/fresh-markets"
grep -Fqx "binary:$bin" "$FAKE_STATE/fresh-binary-dirs"
printf 'campaign fresh-input preparation: PASS\n'

study_case_root="$root/study-handoff"
study_cycle="$study_case_root/cycle"
study_target_inputs="$fresh_control_output/campaign-inputs/study-target/receipts/campaign-inputs.json"
study_target_materialization="$fresh_control_output/campaign-inputs/study-target/materialization.json"
study_target_control="$study_case_root/target-control.json"
study_horizon="$study_case_root/target-horizon.json"
mkdir -p "$study_case_root" "$fresh_control_output/campaign-inputs/study-target/receipts"
jq -n '{labels:{horizon_buckets:5,observation_frequency_millis:1000}}' >"$study_horizon"
jq -n --arg inputs "$study_target_inputs" --arg materialization "$study_target_materialization" \
  '{campaign_inputs_path:$inputs,materialization_path:$materialization}' >"$study_target_control"
study_args=("${fresh_control_args[@]}")
for ((study_index = 0; study_index < ${#study_args[@]}; study_index++)); do
  if [[ "${study_args[study_index]}" == --work-dir ]]; then
    study_args[study_index + 1]="$study_cycle"
  fi
done
study_args+=(--max-follow-ups 2)
study_args+=(
  --study-id study-test
  --study-target-family-id target-family
  --study-target-horizon "$study_horizon"
  --study-target-start-received-at-ns 1700000000000000000
  --study-target-end-received-at-ns 1700000060000000000
  --study-target-mission-id study-target
  --study-target-output-root "$fresh_control_output"
  --study-target-output-prefix campaign-inputs/study-target
  --study-target-bucket-ms 1000
  --study-target-top-depth 5
  --study-target-control "$study_target_control"
)
MONDAY_CAMPAIGN_CONTROL="$bin/control" "$controller" "${study_args[@]}" \
  >"$root/study-start.stdout" 2>"$root/study-start.stderr"
study_ack_args=(
  ack-readback --alpha-harness "$bin/alpha-harness" --aliyun "$bin/aliyun" --kubectl "$bin/kubectl"
  --campaign-pod-name pod-g0 --work-dir "$study_cycle"
)
: >"$FAKE_STATE/fail-study-propose-once"
if MONDAY_CAMPAIGN_CONTROL="$bin/control" FAKE_STUDY_PROPOSAL_STATUS=needs_authority \
  "$controller" "${study_ack_args[@]}" \
  >"$root/study-interrupted.stdout" 2>"$root/study-interrupted.stderr"; then
  echo "interrupted Study proposal unexpectedly completed" >&2
  exit 1
fi
test -s "$study_cycle/generation-0/study/proposal-report.json.partial"
test ! -e "$study_cycle/generation-0/study/proposal-report.json"
if ! MONDAY_CAMPAIGN_CONTROL="$bin/control" FAKE_STUDY_PROPOSAL_STATUS=needs_authority "$controller" "${study_ack_args[@]}" \
  >"$root/study-needs.stdout" 2>"$root/study-needs.stderr"; then
  cat "$root/study-needs.stderr" >&2
  exit 1
fi
test ! -e "$study_cycle/generation-0/study/proposal-report.json.partial"
jq -e '.status == "needs_authority" and (.reason | startswith("study_"))' \
  "$study_cycle/generation-0/needs-authority.json" >/dev/null
test ! -e "$study_cycle/generation-0/generation-complete"

# Explicit authority resume retries the cached report even with unchanged
# input/control paths and preserves the prior report evidence.
study_resume_ack_args=("${study_ack_args[@]}" --study-retry-authority)
if ! MONDAY_CAMPAIGN_CONTROL="$bin/control" FAKE_STUDY_PROPOSAL_STATUS=needs_authority FAKE_STUDY_PROPOSAL_REASON=second_gap "$controller" "${study_resume_ack_args[@]}" \
  >"$root/study-resumed.stdout" 2>"$root/study-resumed.stderr"; then
  cat "$root/study-resumed.stderr" >&2
  exit 1
fi
archive_count="$(find "$study_cycle/generation-0/study" -name 'proposal-report.needs-authority.*.json' | wc -l | tr -d ' ')"
test "$archive_count" -eq 1
if ! MONDAY_CAMPAIGN_CONTROL="$bin/control" FAKE_STUDY_PROPOSAL_STATUS=ready "$controller" "${study_resume_ack_args[@]}" \
  >"$root/study-authorized.stdout" 2>"$root/study-authorized.stderr"; then
  cat "$root/study-authorized.stderr" >&2
  exit 1
fi
if [[ ! -s "$study_cycle/generation-0/study-handoff.json" ]]; then
  cat "$root/study-authorized.stderr" >&2
  cat "$root/study-authorized.stdout" >&2
  exit 1
fi
jq -e '.outcome == "study_handoff" and .study_handoff_sha256 != ""' \
  "$study_cycle/generation-0/generation-complete" >/dev/null
journal_count="$(find "$study_cycle/generation-0/study" -name 'proposal-report.needs-authority.*.json' | wc -l | tr -d ' ')"
test "$journal_count" -eq 2
study_propose_before_restart="$(<"$FAKE_STATE/study-propose-count")"
MONDAY_CAMPAIGN_CONTROL="$bin/control" FAKE_STUDY_PROPOSAL_STATUS=ready \
  "$controller" "${study_args[@]}" \
  >"$root/study-restart.stdout" 2>"$root/study-restart.stderr"
test "$(<"$FAKE_STATE/study-propose-count")" == "$study_propose_before_restart"
study_approve_args=("${approve_args[@]}")
for ((study_index = 0; study_index < ${#study_approve_args[@]}; study_index++)); do
  if [[ "${study_approve_args[study_index]}" == --work-dir ]]; then
    study_approve_args[study_index + 1]="$study_cycle"
  fi
done
MONDAY_CAMPAIGN_CONTROL="$bin/control" "$controller" "${study_approve_args[@]}" \
  >"$root/study-approve.stdout" 2>"$root/study-approve.stderr"
test -s "$study_cycle/generation-1/request.json"
study_ack_g1_args=(
  ack-readback --alpha-harness "$bin/alpha-harness" --aliyun "$bin/aliyun" --kubectl "$bin/kubectl"
  --campaign-pod-name pod-g1 --work-dir "$study_cycle"
)
study_propose_before_g1="$(<"$FAKE_STATE/study-propose-count")"
if ! MONDAY_CAMPAIGN_CONTROL="$bin/control" FAKE_STUDY_TARGET_NO_CANDIDATE=1 FAKE_LEARN_OUTCOME=no_improvement \
  "$controller" "${study_ack_g1_args[@]}" \
  >"$root/study-ack-g1.stdout" 2>"$root/study-ack-g1.stderr"; then
  cat "$root/study-ack-g1.stderr" >&2
  exit 1
fi
jq -e '.termination_reason == "no_improvement"' "$study_cycle/cycle-result.json" >/dev/null
# One ACK process reuses the verified generation-0 Study handoff, reads the
# generation-1 target, observes no_candidate, then falls back to learning.
grep -Fq 'event=generation_checkpoint_reused generation=0' "$root/study-ack-g1.stderr"
grep -Fq 'event=generation_started generation=1' "$root/study-ack-g1.stderr"
grep -Fq 'event=stage_started generation=1 stage=campaign_learning' "$root/study-ack-g1.stderr"
test "$(<"$FAKE_STATE/study-propose-count")" == "$study_propose_before_g1"
printf 'campaign Study handoff recovery: PASS\n'
echo "campaign cycle controller test: PASS"
