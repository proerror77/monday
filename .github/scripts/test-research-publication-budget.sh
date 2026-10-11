#!/usr/bin/env bash
# Exercise offline admission and deny insufficient or renewable approvals.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$script_dir/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
source_sha=$(git -C "$root" rev-parse HEAD)
products=cex-runner,controller,prediction-runner
bash "$script_dir/research-publication-budget.sh" estimate "$source_sha" "$products" 744 "$work/estimate.json"
jq -e '
 .basis=="catalog-static-upper-bound" and .source_archive.bytes>0 and
 [.allocations[].executable_count]==[7,5,5] and [.allocations[].build_count]==[4,3,3] and
 .total.object_count==50 and .total.oss_requests==156 and .total.native_requests==168 and
 .total.request_body_bytes==(.allocations|map(.request_body_bytes)|add) and
 .total.response_body_bytes==(.allocations|map(.response_body_bytes)|add) and
 .pricing.estimated_micro_cny==(.pricing.request_micro_cny+.pricing.egress_micro_cny+.pricing.storage_micro_cny) and
 .pricing.invoice_hard_cap==false and .pricing.storage_horizon_is_deletion==false
' "$work/estimate.json" >/dev/null
# Freeze only the test clock; production admission reads the runner clock.
mkdir "$work/tools"
cat >"$work/tools/date" <<'EOF'
#!/usr/bin/env bash
test "$*" = '-u +%s'
printf '%s\n' 1791680400
EOF
chmod +x "$work/tools/date"
jq -n --arg products "$products" '{oss_by_product:($products|split(",")|
 map({key:.,value:{bucket:"fixture-bucket",region:"ap-northeast-1",endpoint:"https://fixture-bucket.oss-ap-northeast-1.aliyuncs.com/"}})|from_entries)}' >"$work/oss-policy.json"
jq '{schema:"monday.research-publication-budget-policy.v1", repository,source_sha,products,
 publisher_run_number:123,publisher_run_attempt:1,publisher_workflow:".github/workflows/acr-publish.yml",
 expires_at:1791684000,currency:"CNY",price_model:.pricing.model,storage_hours:.pricing.storage_hours,
 max_estimated_micro_cny:.pricing.estimated_micro_cny,max_oss_requests:.total.oss_requests,
 max_request_body_bytes:.total.request_body_bytes,max_response_body_bytes:.total.response_body_bytes,
 max_new_storage_bytes:.total.new_storage_bytes}' "$work/estimate.json" >"$work/policy.json"
run_admit() {
  PATH="$work/tools:$PATH" RUNNER_TEMP="$work" GITHUB_REPOSITORY=proerror77/monday GITHUB_RUN_ID=456 \
    GITHUB_RUN_NUMBER="${2:-123}" GITHUB_RUN_ATTEMPT="${3:-1}" \
    GITHUB_WORKFLOW_REF="${4:-proerror77/monday/.github/workflows/acr-publish.yml@refs/heads/main}" \
    MONDAY_RESEARCH_PUBLICATION_BUDGET="$(cat "$1")" \
    MONDAY_RELEASE_POLICY_JSON="$(cat "$work/oss-policy.json")" \
    bash "$script_dir/research-publication-budget.sh" admit "$source_sha" "$products" 744 "$work/admission.json" \
      >"$work/out" 2>"$work/err"
}
run_admit "$work/policy.json"
jq -e '.admission.single_run and .admission.publisher_run_id==456 and .admission.publisher_run_number==123' \
  "$work/admission.json" >/dev/null
jq -e '.schema=="monday.oss-publication-budget.v1" and .limits.requests==168 and
 .publisher_run_id==456 and .publisher_run_attempt==1 and .expires_at_ms==1791684000000 and
 .publication_namespaces==["research/builds/","research/sources/"] and
 .limits.requests==(.allocations|map(.limits.requests)|add) and
 .limits.request_payload_bytes==(.allocations|map(.limits.request_payload_bytes)|add) and
 .limits.response_payload_bytes==(.allocations|map(.limits.response_payload_bytes)|add)' \
 "$work/research-publication-native-budget.json" >/dev/null
reject() {
  rm -f "$work/admission.json" "$work/research-publication-native-budget.json"
  if run_admit "$@"; then echo 'unsafe budget admitted' >&2; exit 1; fi
  test ! -e "$work/admission.json"
  test ! -e "$work/research-publication-native-budget.json"
}
negatives=0
for expression in \
  'null' '[]' '.unexpected=true' 'del(.max_new_storage_bytes)' \
  '.schema="foreign"' '.repository="foreign/repo"' '.source_sha=("a"*40)' \
  '.products=["cex-runner"]' '.products|=reverse' '.currency="USD"' \
  '.price_model="cheaper"' '.storage_hours=1' '.publisher_run_number=124' \
  '.publisher_run_number="123"' '.publisher_run_attempt=2' '.publisher_workflow=".github/workflows/ci.yml"' \
  '.expires_at=1791680400' '.expires_at=1792288801' '.expires_at=null' '.expires_at=1794268801' \
  '.max_estimated_micro_cny-=1' '.max_estimated_micro_cny=0' '.max_estimated_micro_cny=0.5' \
  '.max_estimated_micro_cny=1000000001' '.max_oss_requests-=1' '.max_oss_requests=1001' \
  '.max_request_body_bytes-=1' '.max_response_body_bytes-=1' '.max_new_storage_bytes-=1' \
  '.max_request_body_bytes=107374182401' '.max_response_body_bytes=0' '.max_new_storage_bytes=-1'; do
  jq "$expression" "$work/policy.json" >"$work/invalid.json"
  reject "$work/invalid.json"
  negatives=$((negatives+1))
done
printf '{invalid\n' >"$work/invalid.json"
reject "$work/invalid.json"
cat "$work/policy.json" "$work/policy.json" >"$work/invalid.json"
reject "$work/invalid.json"
reject "$work/policy.json" 124
reject "$work/policy.json" 123 2
reject "$work/policy.json" 123 1 proerror77/monday/.github/workflows/acr-publish.yml@refs/heads/develop
for selection in none controller,cex-runner cex-runner,cex-runner foreign; do
  if bash "$script_dir/research-publication-budget.sh" estimate "$source_sha" "$selection" 744 "$work/invalid-estimate.json" \
    >"$work/out" 2>"$work/err"; then echo 'unsafe catalog selection admitted' >&2; exit 1; fi
done
workflow=$root/.github/workflows/acr-publish.yml
ruby -ryaml - "$workflow" "$work" <<'RUBY'
w=YAML.safe_load(File.read(ARGV.fetch(0)))
steps=w.fetch('jobs').fetch('publish').fetch('steps')
selected=steps.select { |s|s['name']=='Admit cumulative research publication budget before cloud use' }
abort 'missing or duplicate workflow budget gate' unless selected.length==1
publication=selected.fetch(0)
File.write(File.join(ARGV.fetch(1),'workflow-budget.sh'),publication.fetch('run'))
gate_steps=w.fetch('jobs').fetch('research-environments').fetch('steps')
early=gate_steps.select {|s|s['name']=='Admit publication budget before manual binary preparation'}
abort 'missing or duplicate preparation budget gate' unless early.length==1 && gate_steps.index(early.fetch(0))==1
abort 'preparation budget gate differs or gained conditional bypass' unless early.fetch(0).fetch('env')==publication.fetch('env') && early.fetch(0).fetch('run')==publication.fetch('run') && !early.fetch(0).key?('if')
File.write(File.join(ARGV.fetch(1),'preparation-budget.sh'),early.fetch(0).fetch('run'))
RUBY
run_workflow() {
  (
    cd "$root"
    PATH="$work/tools:$PATH" RUNNER_TEMP="$work" SOURCE_SHA="$source_sha" PRODUCTS="$products" \
      GITHUB_REPOSITORY=proerror77/monday GITHUB_RUN_ID=456 GITHUB_RUN_NUMBER=123 GITHUB_RUN_ATTEMPT=1 \
      GITHUB_WORKFLOW_REF=proerror77/monday/.github/workflows/acr-publish.yml@refs/heads/main \
      MONDAY_RESEARCH_PUBLICATION_BUDGET="$(cat "$1")" \
      MONDAY_RELEASE_POLICY_JSON="$(cat "$work/oss-policy.json")" \
      bash "${2:-$work/workflow-budget.sh}" >"$work/out" 2>"$work/err"
  )
}
run_workflow "$work/policy.json"
run_workflow "$work/policy.json" "$work/preparation-budget.sh"
test -s "$work/research-publication-budget-admission.json"
for expression in 'null' '.max_estimated_micro_cny=1' '.publisher_run_number=124'; do
  jq "$expression" "$work/policy.json" >"$work/invalid.json"
  rm -f "$work/research-publication-budget-admission.json" "$work/research-publication-native-budget.json"
  if run_workflow "$work/invalid.json"; then echo 'workflow budget gate was bypassed' >&2; exit 1; fi
  test ! -e "$work/research-publication-budget-admission.json"
  test ! -e "$work/research-publication-native-budget.json"
  if run_workflow "$work/invalid.json" "$work/preparation-budget.sh"; then echo 'preparation budget gate was bypassed' >&2; exit 1; fi
  test ! -e "$work/research-publication-budget-admission.json"
done
cp "$work/oss-policy.json" "$work/valid-oss-policy.json"
for expression in '.oss_by_product.controller.region="cn-hangzhou"' \
  '.oss_by_product.controller.endpoint="https://fixture-bucket.oss-accelerate.aliyuncs.com/"' \
  '.oss_by_product.controller.endpoint="https://foreign.invalid/"'; do
  jq "$expression" "$work/valid-oss-policy.json" >"$work/oss-policy.json"
  reject "$work/policy.json"
done
cat "$work/valid-oss-policy.json" "$work/valid-oss-policy.json" >"$work/oss-policy.json"
reject "$work/policy.json"
printf '{invalid\n' >"$work/oss-policy.json"
reject "$work/policy.json"
printf 'Budget: same-source static inventory, exact-boundary admission and %s negative approvals passed\n' "$((negatives+5))"
printf 'Both actual workflow gates: admitted boundaries and three denied configurations each passed\n'
printf 'Foreign regions, accelerated endpoints, malformed and multiple price targets rejected\n'
