#!/usr/bin/env bash
set -euo pipefail
[[ $# == 0 || ( $# == 1 && $1 == --deferred-carry ) ]] || exit 2
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export RESEARCH_SCOPE_FIXTURE="$work" GITHUB_REPOSITORY=fixture/repo
mkdir -p "$work/bin" "$work/repo/.github/scripts" "$work/repo/rust_hft/apps/backtest/src" \
  "$work/repo/deployment/aliyun/research/scripts"
cat >"$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "${!#}" in
  */workflows/acr-publish.yml/runs\?*) cat "$RESEARCH_SCOPE_FIXTURE/runs" ;;
  */runs/100/attempts/2/jobs\?*) cat "$RESEARCH_SCOPE_FIXTURE/pair" ;;
  */runs/200/attempts/2/jobs\?*) cat "$RESEARCH_SCOPE_FIXTURE/controller" ;;
  */runs/300/attempts/2/jobs\?*) cat "$RESEARCH_SCOPE_FIXTURE/cex" ;;
  *) exit 91 ;;
esac
MOCK
chmod 0755 "$work/bin/gh"
export PATH="$work/bin:$PATH"
cd "$work/repo"
git init -q
git config user.name 'CI contract'
git config user.email ci@example.invalid
cp "$root/.github/scripts/research-release-products.json" .github/scripts/
printf 'baseline\n' >rust_hft/apps/backtest/src/example.rs
printf 'baseline\n' >deployment/aliyun/research/scripts/campaign-job-watch.sh
git add .
git commit -qm 'fixture baseline'
base=$(git rev-parse HEAD)
printf 'changed research\n' >rust_hft/apps/backtest/src/example.rs
git commit -qam 'fixture research A'
printf 'docs B\n' >README.md
git add README.md
git commit -qm 'fixture docs B'
head=$(git rev-parse HEAD)
publisher() {
  jq -n --arg sha "$1" '[{total_count:1,workflow_runs:[{id:100,run_attempt:2,head_sha:$sha,head_branch:"main",event:"workflow_run",path:".github/workflows/acr-publish.yml",head_repository:{full_name:"fixture/repo"},status:"completed",conclusion:"success"}]}]' >"$work/runs"
  jq -n --arg sha "$1" '[{jobs:[{id:7,run_id:100,run_attempt:2,name:("Research products published [cex-runner,controller,prediction-runner] ("+$sha+")"),status:"completed",conclusion:"success"}]}]' >"$work/pair"
}
plan() {
  : >"$work/plan"
  SELECTED_JOBS=,, SELECTED_RESEARCH_PRODUCT=none bash "$root/.github/scripts/select-main-research-scope.sh" "$head" "$work/plan" \
    "$root/.github/scripts/fixtures/rust-ci-scope/metadata.fixture"
}
# This focused fixture isolates scheduling from the authenticated baseline
# reader. The complete tests below still exercise that reader through gh.
isolated="$work/isolated/.github/scripts"
mkdir -p "$isolated"
for script in select-main-research-scope.sh select-rust-ci-scope.sh image-build-plan.sh research-release-products.sh research-release-products.json; do
  cp "$root/.github/scripts/$script" "$isolated/$script"
done
cat >"$isolated/read-research-publish-baseline.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ ! -f $RESEARCH_SCOPE_FIXTURE/baseline-failure ]] || exit 42
cp "$RESEARCH_SCOPE_FIXTURE/baselines" "$2"
MOCK
printf '%s\n' '{"cex-runner":"BOOTSTRAP","controller":"BOOTSTRAP","prediction-runner":"BOOTSTRAP"}' >"$work/baselines"
cp "$work/baselines" "$work/baselines-before"
direct_jobs=,ploy/rust-runner-lean,ploy/architecture-contracts,ploy/research-image-binaries,ploy/research-image-smoke,
focused_plan() {
  : >"$work/focused-plan"
  RESEARCH_CARRY_MODE=$1 MONDAY_RELEASE_POLICY_JSON=$2 SELECTED_RESEARCH_PRODUCT=${3:-none} SELECTED_JOBS=${4:-,,} \
    bash "$isolated/select-main-research-scope.sh" "$head" "$work/focused-plan" \
      "$root/.github/scripts/fixtures/rust-ci-scope/metadata.fixture" 2>"$work/focused-log"
}
for policy in '' '{}' '{"oss_by_product":null}' '{"oss_by_product":{}}'; do
  focused_plan defer-unconfigured "$policy"
  grep -Fqx research_product=none "$work/focused-plan"
  grep -Fqx jobs=,, "$work/focused-plan"
  grep -Fqx research_pending_product=cex-runner,controller,prediction-runner "$work/focused-plan"
  grep -Fqx research_deferred_product=cex-runner,controller,prediction-runner "$work/focused-plan"
  grep -Fqx research_carry_policy=unconfigured "$work/focused-plan"
  focused_plan defer-unconfigured "$policy" prediction-runner "$direct_jobs"
  grep -Fqx research_product=prediction-runner "$work/focused-plan"
  grep -Fqx "jobs=$direct_jobs" "$work/focused-plan"
  grep -Fqx research_deferred_product=cex-runner,controller "$work/focused-plan"
done
config_jobs=,ploy/strategy-config-contracts,ploy/architecture-contracts,
focused_plan defer-unconfigured '{}' none "$config_jobs"
grep -Fqx research_product=none "$work/focused-plan"
grep -Fqx "jobs=$config_jobs" "$work/focused-plan"
focused_plan defer-unconfigured '{}' all "$direct_jobs"
grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/focused-plan"
grep -Fqx research_deferred_product=none "$work/focused-plan"
# Any nonempty map restores the whole pending selection. It grants no product
# permission, including when only one product or an unknown product is present.
for policy in '{"oss_by_product":{"controller":{}}}' '{"oss_by_product":{"unknown":false}}'; do
  focused_plan defer-unconfigured "$policy"
  grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/focused-plan"
  grep -Fqx research_deferred_product=none "$work/focused-plan"
  grep -Fqx research_carry_policy=configured "$work/focused-plan"
done
for policy in 'broken json' '{} {}' 'null' '[]' ' ' '{"oss_by_product":false}' '{"oss_by_product":[]}' '{"oss_by_product":""}'; do
  focused_plan defer-unconfigured "$policy"
  grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/focused-plan"
  grep -Fqx research_carry_policy=invalid "$work/focused-plan"
done
focused_plan always 'broken json'
grep -Fqx research_carry_policy=unchecked "$work/focused-plan"
grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/focused-plan"
: >"$work/default-plan"
env -u RESEARCH_CARRY_MODE MONDAY_RELEASE_POLICY_JSON='broken json' SELECTED_RESEARCH_PRODUCT=none SELECTED_JOBS=,, \
  bash "$isolated/select-main-research-scope.sh" "$head" "$work/default-plan"
grep -Fqx research_carry_policy=unchecked "$work/default-plan"
grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/default-plan"
cmp "$work/baselines-before" "$work/baselines"
# A real unpublished source change remains pending while a later docs change
# defers its build; repairing configuration restores it from the same baseline.
jq -n --arg base "$base" '{"cex-runner":$base,"controller":$base,"prediction-runner":$base}' >"$work/baselines"
focused_plan defer-unconfigured '{}'
grep -Fqx research_pending_product=cex-runner "$work/focused-plan"
grep -Fqx research_deferred_product=cex-runner "$work/focused-plan"
focused_plan defer-unconfigured '{"oss_by_product":{"cex-runner":{}}}'
grep -Fqx research_product=cex-runner "$work/focused-plan"
touch "$work/baseline-failure"
if focused_plan defer-unconfigured '{}'; then echo 'baseline failure hidden by deferral' >&2; exit 1; fi
rm "$work/baseline-failure"
printf '{}\n' >"$work/baselines"
if focused_plan defer-unconfigured '{}'; then echo 'missing baseline hidden by deferral' >&2; exit 1; fi
printf 'PASS: carry deferral preserves direct products/jobs, original baselines and conservative policy classification\n'
[[ ${1:-} != --deferred-carry ]] || exit 0
publisher "$base"
plan
grep -Fqx research_product=cex-runner "$work/plan"
grep -Fq ',ploy/research-image-binaries,ploy/research-image-smoke,' "$work/plan"
publisher "$head"
plan
grep -Fqx research_product=none "$work/plan"
# A controller publication advances its own baseline, without losing runner
# changes or rebuilding that controller forever on later documentation commits.
pair_head=$head
printf 'controller change\n' >deployment/aliyun/research/scripts/campaign-job-watch.sh
git commit -qam 'fixture controller C'
controller_head=$(git rev-parse HEAD)
printf 'docs D\n' >>README.md
git commit -qam 'fixture docs D'
head=$(git rev-parse HEAD)
plan
grep -Fqx research_product=controller "$work/plan"
jq --arg sha "$controller_head" '.[0].total_count=2 | .[0].workflow_runs += [.[0].workflow_runs[0] | .id=200 | .head_sha=$sha]' "$work/runs" >"$work/edit"
mv "$work/edit" "$work/runs"
jq -n --arg sha "$controller_head" '[{jobs:[{id:8,run_id:200,run_attempt:2,name:("Research products published [controller] ("+$sha+")"),status:"completed",conclusion:"success"}]}]' >"$work/controller"
plan
grep -Fqx research_product=none "$work/plan"
# Prediction impact survives a later documentation commit and a newer CEX-only
# publication. Each product retains its last independently verified source.
publisher "$head"
mkdir -p rust_hft/prediction-markets/crates/ploy-research/src
printf 'prediction change\n' >rust_hft/prediction-markets/crates/ploy-research/src/lib.rs
git add .
git commit -qm 'fixture prediction E'
printf 'docs F\n' >>README.md
git commit -qam 'fixture docs F'
head=$(git rev-parse HEAD)
plan
grep -Fqx research_product=prediction-runner "$work/plan"
jq --arg sha "$head" '.[0].total_count=2 | .[0].workflow_runs += [.[0].workflow_runs[0] | .id=300 | .head_sha=$sha]' "$work/runs" >"$work/edit"
mv "$work/edit" "$work/runs"
jq -n --arg sha "$head" '[{jobs:[{id:9,run_id:300,run_attempt:2,name:("Research products published [cex-runner] ("+$sha+")"),status:"completed",conclusion:"success"}]}]' >"$work/cex"
plan
grep -Fqx research_product=prediction-runner "$work/plan"
publisher "$pair_head"
jq '.[0].jobs += [.[0].jobs[0]]' "$work/pair" >"$work/edit"
mv "$work/edit" "$work/pair"
if plan >"$work/rejected" 2>&1; then echo 'ambiguous baseline admitted' >&2; exit 1; fi
publisher "$pair_head"
jq '.[0].jobs[0].name="Research products published [cex-runner,controller,prediction-runner] (wrong-source)"' "$work/pair" >"$work/edit"
mv "$work/edit" "$work/pair"
if plan >"$work/rejected" 2>&1; then echo 'wrong baseline source admitted' >&2; exit 1; fi
publisher "$head"
jq --arg sha "$head" '.[0].jobs[0].name=("Research release complete [paired] ("+$sha+")")' "$work/pair" >"$work/edit"
mv "$work/edit" "$work/pair"
plan
grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/plan"
printf '[{"total_count":0,"workflow_runs":[]}]\n' >"$work/runs"
plan
grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/plan"
printf '[{}]\n' >"$work/runs"
if plan >"$work/rejected" 2>&1; then echo 'missing API baseline metadata admitted' >&2; exit 1; fi
# A helper-only main build fails, then a documentation push must retain its
# cumulative unpublished impact for every product. A failed run is no baseline.
publisher "$head"
helper_base=$head
printf 'recipe helper change\n' >.github/scripts/build-research-recipes.sh
git add .github/scripts/build-research-recipes.sh
git commit -qm 'fixture helper-only main G'
helper_head=$(git rev-parse HEAD)
jq --arg sha "$helper_head" '.[0].total_count=2 | .[0].workflow_runs += [.[0].workflow_runs[0] | .id=400 | .head_sha=$sha | .conclusion="failure"]' "$work/runs" >"$work/edit"
mv "$work/edit" "$work/runs"
printf 'docs H\n' >>README.md
git commit -qam 'fixture documentation after failed helper build'
head=$(git rev-parse HEAD)
plan
grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/plan"
grep -Fq ',ploy/research-image-binaries,ploy/research-image-smoke,' "$work/plan"
# Only a verified successful publication advances the helper baseline.
publisher "$head"
plan
grep -Fqx research_product=none "$work/plan"
[[ $helper_base != "$head" ]]
printf 'PASS: unpublished A survives docs B; separate product readbacks suppress completed work; malformed or ambiguous baselines fail\n'
