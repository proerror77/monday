#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$script_dir/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
sha=1111111111111111111111111111111111111111
jq -n --arg sha "$sha" '
  ["cex-runner","controller","prediction-runner"] as $products
  | {"cex-runner":"monday-research-cex",controller:"monday-research-controller","prediction-runner":"monday-research-prediction"} as $names
  | {schema:"monday.automatic_research_publication.v1",repository_id:12,owner_id:34,subject_prefix:"repo:fixture/repo",
      products:($products | to_entries | map({key:.value,value:{name:$names[.value],environment_id:(.key+101)}}) | from_entries)} as $config
  | {source_sha:$sha,products:($products|join(",")),repository_name:"fixture/repo",run_id:56,run_attempt:1,config:$config,
      repository:{id:12,full_name:"fixture/repo",owner:{id:34},visibility:"public"},main:{object:{sha:$sha}},
      run:{id:56,run_attempt:1,head_sha:$sha,head_branch:"main",path:".github/workflows/acr-publish.yml",event:"workflow_run",repository:{id:12},head_repository:{id:12}},
      oidc:{use_default:true,include_claim_keys:[]},
      environments:[$products[] as $p | {product:$p,
        environment:{id:$config.products[$p].environment_id,name:$names[$p],protection_rules:[{id:201,type:"branch_policy"}],
          deployment_branch_policy:{protected_branches:false,custom_branch_policies:true}},
        branches:{total_count:1,branch_policies:[{name:"main",type:"branch"}]},
        policy:{oss:{role_arn:("role/"+$p),publication_namespaces:["research/builds/","research/sources/"],subject:("repo:fixture/repo:environment:"+$names[$p])}}}]}
' > "$work/valid.json"
jq -e -f "$script_dir/automatic-research-publication.jq" "$work/valid.json" > "$work/accepted.json"
jq -e '.products==["cex-runner","controller","prediction-runner"] and (.environments|length)==3' "$work/accepted.json" >/dev/null
for mutation in 'del(.oidc.include_claim_keys)' '.oidc={use_default:true,use_immutable_subject:false,sub_claim_prefix:"repo:fixture/repo"}'; do
  jq "$mutation" "$work/valid.json" > "$work/compatible.json"
  jq -e -f "$script_dir/automatic-research-publication.jq" "$work/compatible.json" > "$work/compatible-admission.json"
  cmp "$work/accepted.json" "$work/compatible-admission.json"
done
negative_count=0
while IFS= read -r mutation; do
  [[ -n $mutation ]] || continue
  jq "$mutation" "$work/valid.json" > "$work/invalid.json"
  if jq -e -f "$script_dir/automatic-research-publication.jq" "$work/invalid.json" > "$work/denied.json" 2>/dev/null; then
    echo "automatic publication accepted invalid infrastructure: $mutation" >&2; exit 1
  fi
  [[ ! -s "$work/denied.json" ]]
  negative_count=$((negative_count+1))
done <<'CASES'
.config=null
.config.schema="unknown"
.config.repository_id=0
.config.owner_id=0
.repository.id=999
.repository.owner.id=999
.repository.full_name="foreign/repo"
.repository.visibility="private"
.source_sha="bad"
.main.object.sha=("2"*40)
.run.id=57
.run.run_attempt=2
.run.head_sha=("2"*40)
.run.head_branch="feature"
.run.path=".github/workflows/other.yml"
.run.event="pull_request"
.run.repository.id=999
.run.head_repository.id=999
.oidc.use_default=false
.oidc.use_default=null
del(.oidc.use_default)
.config.subject_prefix="repo:foreign/repo"
.oidc.use_immutable_subject=true
.oidc.use_immutable_subject=null
.oidc.use_immutable_subject="false"
.oidc.sub_claim_prefix="repo:foreign/repo"
.oidc.sub_claim_prefix=null
.products="controller,cex-runner"
.products="cex-runner,cex-runner"
.products="foreign"
.products=""
.config.products.controller.environment_id=101
.config.products.controller.environment_id=0
del(.config.products["prediction-runner"])
.config.products.foreign={name:"foreign",environment_id:104}
.environments=[]
.environments[1]=.environments[0]
.config.products.controller.name="foreign"
.environments[1].environment=null
.environments[1].environment.id=999
.environments[1].environment.name="foreign"
.environments[1].environment.deployment_branch_policy=null
.environments[1].branches.total_count=2
.environments[1].branches.branch_policies[0].name="*"
.environments[1].branches.branch_policies[0].type="tag"
.environments[1].environment.protection_rules=[{type:"required_reviewers",reviewers:[{type:"User"}]}]
.environments[1].environment.protection_rules=[{type:"wait_timer"}]
.environments[1].environment.protection_rules=[{type:"custom"}]
.environments[1].environment.protection_rules=[{type:"unknown"}]
.environments[1].environment.protection_rules=[{type:"branch_policy"},{type:"branch_policy"}]
del(.environments[1].environment.protection_rules)
.environments[1].policy.oss.subject="repo:fixture/repo:ref:refs/heads/main"
.environments[1].policy.oss.subject=.environments[0].policy.oss.subject
CASES
jq '.config' "$work/valid.json" > "$work/config.json"
jq '{oss_by_product:(.environments | map({key:.product,value:.policy.oss}) | from_entries)}' "$work/valid.json" > "$work/policy.json"
mkdir "$work/bin" "$work/runner"
cat > "$work/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
[[ $# == 4 && $1 == api && $2 == --method && $3 == GET ]]
printf '%s\n' "$4" >> "$TEST_API_TRACE"
case "$4" in
  repos/fixture/repo) jq '.repository' "$TEST_BUNDLE" ;;
  repos/fixture/repo/git/ref/heads/main)
    if [[ ${TEST_FAILURE:-} == main-drift && $(wc -l < "$TEST_API_TRACE") -gt 4 ]]; then
      jq '.main.object.sha=("2"*40) | .main' "$TEST_BUNDLE"
    else jq '.main' "$TEST_BUNDLE"; fi ;;
  repos/fixture/repo/actions/runs/56) jq '.run' "$TEST_BUNDLE" ;;
  repos/fixture/repo/actions/oidc/customization/sub) jq '.oidc' "$TEST_BUNDLE" ;;
  repos/fixture/repo/environments/*/deployment-branch-policies?per_page=100)
    name=${4#repos/fixture/repo/environments/}; name=${name%%/*}
    jq --arg name "$name" '.environments[] | select(.environment.name==$name) | .branches' "$TEST_BUNDLE" ;;
  repos/fixture/repo/environments/*)
    name=${4##*/}
    [[ ${TEST_FAILURE:-} != deleted ]]
    jq --arg name "$name" '.environments[] | select(.environment.name==$name) | .environment' "$TEST_BUNDLE" ;;
  *) exit 1 ;;
esac
GH
chmod +x "$work/bin/gh"
export PATH="$work/bin:$PATH" TEST_BUNDLE="$work/valid.json" TEST_API_TRACE="$work/api.log"
export GITHUB_REPOSITORY=fixture/repo GITHUB_REF=refs/heads/main GITHUB_RUN_ID=56 GITHUB_RUN_ATTEMPT=1
export RUNNER_TEMP="$work/runner" SOURCE_SHA="$sha" PRODUCTS=cex-runner,controller,prediction-runner
export MONDAY_RESEARCH_AUTOMATIC_PUBLICATION MONDAY_RESEARCH_RELEASE_POLICY
MONDAY_RESEARCH_AUTOMATIC_PUBLICATION=$(cat "$work/config.json")
MONDAY_RESEARCH_RELEASE_POLICY=$(cat "$work/policy.json")
bash "$script_dir/read-automatic-research-publication.sh" "$PRODUCTS" "$sha" "$work/readback.json"
jq -e '.run_id==56 and (.environments|length)==3' "$work/readback.json" >/dev/null
for failure in deleted main-drift; do
  : > "$TEST_API_TRACE"
  if TEST_FAILURE="$failure" bash "$script_dir/read-automatic-research-publication.sh" "$PRODUCTS" "$sha" "$work/rejected.json" >/dev/null 2>&1; then exit 1; fi
  [[ ! -e "$work/rejected.json" ]]
done
ruby -ryaml -rjson - "$repo/.github/workflows/acr-publish.yml" "$work" <<'RUBY'
workflow=YAML.safe_load(File.read(ARGV[0])); jobs=workflow.fetch('jobs')
gate=jobs.fetch('research-environments'); pub=jobs.fetch('publish'); ordinary=jobs.fetch('publish-ordinary')
abort 'automatic gate gains cloud/write permissions' unless gate.fetch('permissions')=={'actions'=>'read','contents'=>'read'}
abort 'publisher can start without infrastructure gate' unless pub.fetch('needs').include?('research-environments') && pub.fetch('if').include?("needs.research-environments.result == 'success'")
abort 'publisher uses a constant/unvalidated environment' unless pub.fetch('environment')=={'name'=>'${{ matrix.environment_name }}'} && pub.fetch('strategy').fetch('matrix')=='${{ fromJSON(needs.research-environments.outputs.matrix) }}'
abort 'shared source first writes can run concurrently' unless pub.fetch('strategy').fetch('max-parallel')==1
abort 'ordinary publications acquired an environment or OIDC' if ordinary.key?('environment') || ordinary.fetch('permissions').key?('id-token')
abort 'manual rebuild bypasses environment admission' unless jobs.fetch('research-runner-binaries').fetch('needs')==['selector','research-environments']
steps=pub.fetch('steps'); recheck=steps.find {|s|s['name']=='Recheck automatic publication before credential use'}
abort 'recheck is not the first step after checkout' unless steps.index(recheck)==1
abort 'automatic recheck receives secrets' if recheck.to_s.include?('secrets.')
budgets=steps.select {|s|s['name']=='Admit cumulative research publication budget before cloud use'}
abort 'missing or ambiguous budget gate' unless budgets.length==1
budget=budgets.fetch(0)
abort 'budget gate gained credentials or lost selection' unless budget.fetch('if')=='matrix.research_artifact' && budget.fetch('env')=={
  'MONDAY_RESEARCH_PUBLICATION_BUDGET'=>'${{ vars.MONDAY_RESEARCH_PUBLICATION_BUDGET }}',
  'MONDAY_RELEASE_POLICY_JSON'=>'${{ vars.MONDAY_RESEARCH_RELEASE_POLICY }}',
  'SOURCE_SHA'=>'${{ needs.selector.outputs.source_sha }}',
  'PRODUCTS'=>'${{ needs.selector.outputs.research_products }}'
} && budget.fetch('run').include?('research-publication-budget.sh admit')
comparison=Marshal.load(Marshal.dump(steps-[recheck,budget]))
retention=comparison.find {|s|s['name']=='Retain native Build release projection'}
paths=retention.fetch('with').fetch('path').lines
budget_paths=["${{ runner.temp }}/research-publication-budget-admission.json\n", "${{ runner.temp }}/research-publication-native-budget.json\n"]
abort 'native budget evidence paths are missing or duplicated' unless budget_paths.all? {|p|paths.count(p)==1}
retention.fetch('with')['path']=(paths-budget_paths).join
abort 'research and ordinary publication checks diverged' unless comparison==ordinary.fetch('steps')
abort 'manual approval/dispatch entered automatic workflow' if workflow.to_s.match?(/review_source_sha|dispatch-research|prepare-research-publication-review|research-publication-environment-probe|actions:\s*write/)
selector=jobs.fetch('selector').fetch('steps').find {|s|s['id']=='select'}
File.write(File.join(ARGV[1],'selector.sh'),selector.fetch('run'))
File.write(File.join(ARGV[1],'gate.sh'),gate.fetch('steps').find {|s|s['id']=='environments'}.fetch('run'))
File.write(File.join(ARGV[1],'recheck.sh'),recheck.fetch('run'))
RUBY
cd "$repo"
export GITHUB_OUTPUT="$work/output" TARGET=research-products PUBLISHED_PRODUCTS="$PRODUCTS"
bash "$work/selector.sh"
export SOURCE_MATRIX
SOURCE_MATRIX=$(sed -n 's/^matrix=//p' "$GITHUB_OUTPUT")
: > "$GITHUB_OUTPUT"
bash "$work/gate.sh"
sed -n 's/^matrix=//p' "$GITHUB_OUTPUT" | jq -e '.include|length==3 and all(.[];has("environment_id") and has("environment_name"))' >/dev/null
export EXPECTED_PRODUCT=controller EXPECTED_ENVIRONMENT_ID=102
bash "$work/recheck.sh" >/dev/null
rm "$RUNNER_TEMP/automatic-publication-admission.json" "$RUNNER_TEMP/automatic-publication-recheck.json"
post_count=0
for mutation in '.environments[1].environment.id=999' '.environments[1].environment.protection_rules=[{type:"required_reviewers"}]' '.environments[1].branches.branch_policies[0].name="*"' '.environments[1].policy.oss.subject="wrong"'; do
  jq "$mutation" "$work/valid.json" > "$work/changed.json"
  MONDAY_RESEARCH_RELEASE_POLICY=$(jq -c '{oss_by_product:(.environments | map({key:.product,value:.policy.oss}) | from_entries)}' "$work/changed.json")
  : > "$GITHUB_OUTPUT"
  if TEST_BUNDLE="$work/changed.json" bash "$work/gate.sh" >/dev/null 2>&1; then exit 1; fi
  [[ ! -s "$GITHUB_OUTPUT" && ! -e "$RUNNER_TEMP/automatic-publication-admission.json" ]]
  if TEST_BUNDLE="$work/changed.json" bash "$work/recheck.sh" >/dev/null 2>&1; then exit 1; fi
  [[ ! -e "$RUNNER_TEMP/automatic-publication-recheck.json" ]]
  post_count=$((post_count+1))
done
printf 'Automatic publication: %s negative fixtures, GET-only admission, real workflow gate and %s credential-step drift rejections passed\n' "$negative_count" "$post_count"
