#!/usr/bin/env bash
set -euo pipefail

root="$(mktemp -d)"
trap 'rm -rf -- "$root"' EXIT
bin="$root/bin"
export FAKE_STATE="$root/state"
mkdir "$bin" "$root/input" "$FAKE_STATE"
touch "$root/campaign-inputs.json"

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
          schema_version:"cex-campaign-request-v4",
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
              mission_readback_url:($object_root + "/r1/mission.json"),
              result_readback_url:($object_root + "/r1/results.zip")
            },
            {
              round_id:"r2",seed:11,
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
    printf '{"schema_version":"cex-campaign-research-plan-v2"}\n' >"$output"
    jq -n '{failure_class:"no_trades_after_costs",learning_directive_sha256:("9" * 64),search_policy_revision_id:("cex-search-policy-" + ("1" * 64)),research_plan_sha256:("e" * 64)}'
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
  *" get pods "*)
    jq -n --arg request_sha256 "$request_sha256" '{items:[{
      metadata:{annotations:{"research.monday/request-sha256":$request_sha256}},
      status:{
        phase:"Succeeded",
        containerStatuses:[{
          name:"alpha-campaign",
          imageID:("registry.example/research@sha256:" + ("a" * 64)),
          state:{terminated:{exitCode:0}}
        }]
      }
    }]}'
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
source_object="$3"
destination="$4"
generation=0
[[ "$source_object" != *"/g1/"* ]] || generation=1

sha_text() {
  if command -v shasum >/dev/null; then
    printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
  else
    printf '%s' "$1" | sha256sum | awk '{print $1}'
  fi
}

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
      schema_version:"cex-campaign-result-v7",
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
        {round_id:"r1",seed:7,mission_sha256:$mission_r1_sha,request_sha256:$request_sha256,result_bundle_sha256:$bundle_r1_sha,result_readback_bundle_sha256:$bundle_r1_sha,consumed_trials:1},
        {round_id:"r2",seed:11,mission_sha256:$mission_r2_sha,request_sha256:$request_sha256,result_bundle_sha256:$bundle_r2_sha,result_readback_bundle_sha256:$bundle_r2_sha,consumed_trials:1}
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

chmod +x "$bin/alpha-harness" "$bin/signer" "$bin/kubectl" "$bin/aliyun"

controller="$(cd "$(dirname "$0")" && pwd)/scripts/campaign-cycle-controller.sh"
source_revision="$(printf 'a%.0s' {1..40})"
image_digest="$(printf 'a%.0s' {1..64})"
controller_args=(
  --alpha-harness "$bin/alpha-harness"
  --aliyun "$bin/aliyun"
  --kubectl "$bin/kubectl"
  --campaign-inputs "$root/campaign-inputs.json"
  --input-root "$root/input"
  --source-revision "$source_revision"
  --image "registry.example/research@sha256:$image_digest"
  --campaign-root https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns
  --signer "$bin/signer"
  --work-dir "$root/cycle"
  --seed 7 --seed 11
  --max-follow-ups 1
)

if "$controller" "${controller_args[@]}" >"$root/first.stdout" 2>"$root/first.stderr"; then
  echo "first controller run unexpectedly succeeded" >&2
  exit 1
fi
test -s "$root/cycle/generation-0/request.json"
test "$(<"$FAKE_STATE/signer-count")" == 1
test "$(<"$FAKE_STATE/dispatch-count")" == 1
grep -Fq 'schema_version=monday.research_event.v1 component=campaign-cycle-controller event=cycle_failed generation=0 stage=oss_result_readback' "$root/first.stderr"

if "$controller" "${controller_args[@]}" >"$root/second.stdout" 2>"$root/second.stderr"; then
  echo "controller accepted a mismatched learning-directive digest" >&2
  exit 1
fi
grep -Fq 'schema_version=monday.research_event.v1 component=campaign-cycle-controller event=cycle_failed generation=1 stage=oss_result_readback' "$root/second.stderr"

if ! "$controller" "${controller_args[@]}" >"$root/third.stdout" 2>"$root/third.stderr"; then
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
  "$root/cycle/cycle-result.json" >/dev/null
test -s "$root/cycle/generation-0/next-research-plan.json"
test "$(<"$FAKE_STATE/signer-count")" == 2
test "$(<"$FAKE_STATE/dispatch-count")" == 2
test "$(wc -l <"$FAKE_STATE/deleted-secrets" | tr -d ' ')" == 2
test "$(grep -c -- '--timeout=7h' "$FAKE_STATE/job-waits")" == 2
for generation in 0 1; do
  test -e "$root/cycle/generation-$generation/provenance-readback-complete"
  test -e "$root/cycle/generation-$generation/result-readback-complete"
  for round_index in 0 1; do
    test -s "$root/cycle/generation-$generation/round-readback/round-$round_index-mission.json"
    test -s "$root/cycle/generation-$generation/round-readback/round-$round_index-results.zip"
  done
  for sensitive in signed-request.json request.json submission.json; do
    test ! -e "$root/cycle/generation-$generation/$sensitive"
  done
done

echo "campaign cycle controller test: PASS"
