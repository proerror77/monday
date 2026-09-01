#!/usr/bin/env bash
set -euo pipefail

root="$(mktemp -d)"
trap 'rm -rf -- "$root"' EXIT
bin="$root/bin"
mkdir "$bin" "$root/input"
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

case "$1 $2" in
  "mission campaign-freeze")
    output="$(value_after --output "$@")"
    generation=0
    [[ " $* " != *" --research-plan "* ]] || generation=1
    jq -n --arg generation "$generation" '{
      schema_version:"cex-campaign-freeze-v1",
      campaign_inputs_sha256:("a" * 64),
      canonical_request:{
        campaign_id:("campaign-g" + $generation),
        campaign_result_readback_url:("https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/g" + $generation + "/campaign-result.json")
      },
      signing_plan:{actions:[]}
    }' >"$output"
    jq -n --arg generation "$generation" '{campaign_id:("campaign-g" + $generation)}'
    ;;
  "mission campaign-finalize")
    freeze="$(value_after --freeze "$@")"
    request_out="$(value_after --request-out "$@")"
    submission_out="$(value_after --submission-out "$@")"
    generation="$(jq -r '.canonical_request.campaign_id | sub("campaign-g"; "")' "$freeze")"
    jq '.canonical_request' "$freeze" >"$request_out"
    printf '{}\n' >"$submission_out"
    jq -n --arg generation "$generation" '{
      campaign_id:("campaign-g" + $generation),
      request_sha256:("request-g" + $generation),
      job_name:("job-g" + $generation)
    }'
    ;;
  "mission dispatch")
    printf '{"submitted":true}\n'
    ;;
  "mission campaign-learn")
    output="$(value_after --output "$@")"
    printf '{"schema_version":"cex-campaign-research-plan-v1"}\n' >"$output"
    printf '{"learned":true}\n'
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
jq '.canonical_request' "$freeze" >"$output"
EOF

cat >"$bin/kubectl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ " $* " == *" wait "* ]]; then
  exit 0
fi
printf '{"status":{"conditions":[{"type":"Complete","status":"True"}]}}\n'
EOF

cat >"$bin/aliyun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2" == "ossutil cp" ]]
source_object="$3"
destination="$4"
generation=0
[[ "$source_object" != *"/g1/"* ]] || generation=1
termination="campaign_no_candidate"
[[ "$generation" != 1 ]] || termination="campaign_finalized"
jq -n --arg generation "$generation" --arg termination "$termination" '{
  campaign_id:("campaign-g" + $generation),
  request_sha256:("request-g" + $generation),
  termination_reason:$termination
}' >"$destination"
EOF

chmod +x "$bin/alpha-harness" "$bin/signer" "$bin/kubectl" "$bin/aliyun"

controller="$(cd "$(dirname "$0")" && pwd)/scripts/campaign-cycle-controller.sh"
"$controller" \
  --alpha-harness "$bin/alpha-harness" \
  --aliyun "$bin/aliyun" \
  --kubectl "$bin/kubectl" \
  --campaign-inputs "$root/campaign-inputs.json" \
  --input-root "$root/input" \
  --source-revision aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --image registry.example/research@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --campaign-root https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns \
  --signer "$bin/signer" \
  --work-dir "$root/cycle" \
  --seed 7 --seed 11 \
  --max-follow-ups 1 >/dev/null

jq -e '.generation == 1 and .termination_reason == "campaign_finalized"' \
  "$root/cycle/cycle-result.json" >/dev/null
test -s "$root/cycle/generation-0/next-research-plan.json"
for sensitive in \
  "$root/cycle/generation-0/signed-request.json" \
  "$root/cycle/generation-0/request.json" \
  "$root/cycle/generation-0/submission.json" \
  "$root/cycle/generation-1/signed-request.json" \
  "$root/cycle/generation-1/request.json" \
  "$root/cycle/generation-1/submission.json"; do
  test ! -e "$sensitive"
done

echo "campaign cycle controller test: PASS"
