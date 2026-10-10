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
  */workflows/acr-publish.yml/runs\?*) fixture=runs ;;
  */runs/100/attempts/2/jobs\?*) fixture=pair ;;
  */runs/101/attempts/2/jobs\?*) fixture=empty ;;
  */runs/200/attempts/2/jobs\?*) fixture=controller ;;
  */runs/300/attempts/2/jobs\?*) fixture=cex ;;
  *) exit 91 ;;
esac
printf '%s\n' "$fixture" >>"$RESEARCH_SCOPE_FIXTURE/api-calls"
if [[ -f $RESEARCH_SCOPE_FIXTURE/api-failure && $fixture == "$(cat "$RESEARCH_SCOPE_FIXTURE/api-failure-target")" ]]; then
  remaining=$(cat "$RESEARCH_SCOPE_FIXTURE/api-failure-count")
  if (( remaining > 0 )); then
    printf '%s\n' "$((remaining - 1))" >"$RESEARCH_SCOPE_FIXTURE/api-failure-count"
    if [[ $(cat "$RESEARCH_SCOPE_FIXTURE/api-failure") == hang ]]; then
      printf '%s\n' "$$" >"$RESEARCH_SCOPE_FIXTURE/api-hang-pid"
      sleep 60
      exit 1
    fi
    printf '[{"partial":true}]\n'
    printf 'private debug data must not reach diagnostics\n' >&2
    printf 'gh: fixture failure (HTTP %s)\n' "$(cat "$RESEARCH_SCOPE_FIXTURE/api-failure")" >&2
    exit 1
  fi
fi
cat "$RESEARCH_SCOPE_FIXTURE/$fixture"
MOCK
printf '[{"total_count":0,"jobs":[]}]\n' >"$work/empty"
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
  jq -n --arg sha "$1" '[{total_count:1,jobs:[{id:7,run_id:100,run_attempt:2,head_sha:$sha,name:("Research products published [cex-runner,controller,prediction-runner] ("+$sha+")"),status:"completed",conclusion:"success"}]}]' >"$work/pair"
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
# A parser change remains an unpublished input after a later docs-only push.
previous_head=$head
mkdir -p .github/scripts/vendor/tomlrb
printf 'changed parser provenance\n' >.github/scripts/vendor/tomlrb/LICENSE.txt
git add -f .github/scripts/vendor/tomlrb/LICENSE.txt
git commit -qm 'fixture cache parser change'
printf 'docs after parser\n' >>README.md
git commit -qam 'fixture docs after parser'
head=$(git rev-parse HEAD)
jq -n --arg base "$previous_head" '{"cex-runner":$base,"controller":$base,"prediction-runner":$base}' >"$work/baselines"
focused_plan always '{}'
grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/focused-plan"
focused_plan defer-unconfigured '{}'
grep -Fqx research_pending_product=cex-runner,controller,prediction-runner "$work/focused-plan"
grep -Fqx research_deferred_product=cex-runner,controller,prediction-runner "$work/focused-plan"
git checkout -q --detach "$previous_head"
head=$previous_head
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
jq -n --arg sha "$controller_head" '[{total_count:1,jobs:[{id:8,run_id:200,run_attempt:2,head_sha:$sha,name:("Research products published [controller] ("+$sha+")"),status:"completed",conclusion:"success"}]}]' >"$work/controller"
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
jq -n --arg sha "$head" '[{total_count:1,jobs:[{id:9,run_id:300,run_attempt:2,head_sha:$sha,name:("Research products published [cex-runner] ("+$sha+")"),status:"completed",conclusion:"success"}]}]' >"$work/cex"
plan
grep -Fqx research_product=prediction-runner "$work/plan"
publisher "$pair_head"
jq '.[0].total_count=2 | .[0].jobs += [.[0].jobs[0] | .id=8]' "$work/pair" >"$work/edit"
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

# Partial failed responses must be discarded before a bounded GET retry.
reader="$root/.github/scripts/read-research-publish-baseline.sh"
retry_fixture() {
  publisher "$head"
  printf '%s\n' "$1" >"$work/api-failure"
  printf '%s\n' "$2" >"$work/api-failure-count"
  printf '%s\n' "${3:-runs}" >"$work/api-failure-target"
  : >"$work/api-calls"
  rm -f "$work/retry-baseline"
}
read_baseline() { bash "$reader" "$head" "$work/retry-baseline" 2>"$work/retry-log"; }
retry_fixture 502 1
read_baseline
jq -e --arg sha "$head" 'all(.[]; .==$sha)' "$work/retry-baseline" >/dev/null
[[ $(grep -c '^runs$' "$work/api-calls") == 2 ]]
grep -Fq 'resource=workflow_runs attempt=1 status=502 retry=true' "$work/retry-log"
! grep -Fq 'private debug' "$work/retry-log"
retry_fixture 503 1 pair
read_baseline
[[ $(grep -c '^pair$' "$work/api-calls") == 2 ]]
retry_fixture hang 1
read_baseline
[[ $(grep -c '^runs$' "$work/api-calls") == 2 ]]
grep -Fq 'status=timeout retry=true' "$work/retry-log"
if kill -0 "$(cat "$work/api-hang-pid")" 2>/dev/null; then
  echo 'timed-out GET process remained alive' >&2; exit 1
fi
for status in 500 502 503 504; do
  retry_fixture "$status" 10
  if read_baseline; then echo 'persistent GET failure admitted' >&2; exit 1; fi
  [[ $(grep -c '^runs$' "$work/api-calls") == 3 && ! -e $work/retry-baseline ]]
done
for status in 401 403 404 429; do
  retry_fixture "$status" 10
  if read_baseline; then echo 'permanent API rejection retried or admitted' >&2; exit 1; fi
  [[ $(grep -c '^runs$' "$work/api-calls") == 1 && ! -e $work/retry-baseline ]]
done
rm "$work/api-failure"
publisher "$head"
cp "$work/runs" "$work/complete-runs"
cp "$work/pair" "$work/complete-jobs"
for broken in '[{}]' '[]' 'not JSON'; do
  printf '%s\n' "$broken" >"$work/runs"
  : >"$work/api-calls"
  rm -f "$work/retry-baseline"
  if read_baseline; then echo 'malformed pagination admitted' >&2; exit 1; fi
  [[ $(grep -c '^runs$' "$work/api-calls") == 1 && ! -e $work/retry-baseline ]]
done
cp "$work/complete-runs" "$work/runs"
for edit in '.[0].total_count=2' '.[0].jobs[0].run_attempt=1' '.[0].jobs[0].run_id=101' \
  '.[0].jobs[0].head_sha="0000000000000000000000000000000000000000"' \
  '.[0].total_count=2 | .[0].jobs += [.[0].jobs[0]]'; do
  jq "$edit" "$work/complete-jobs" >"$work/pair"
  rm -f "$work/retry-baseline"
  if read_baseline; then echo 'partial or mismatched job response admitted' >&2; exit 1; fi
  [[ ! -e $work/retry-baseline ]]
done
cp "$work/complete-jobs" "$work/pair"
# Accept complete multiple pages and reject truncation even when JSON is valid.
jq '.[0].total_count=2 | . + [.[0] | .workflow_runs[0].id=101]' "$work/complete-runs" >"$work/runs"
read_baseline
jq '.[0].total_count=2' "$work/complete-runs" >"$work/runs"
rm -f "$work/retry-baseline"
if read_baseline; then echo 'truncated run history admitted' >&2; exit 1; fi
[[ ! -e $work/retry-baseline ]]
printf 'PASS: bounded GET retry discards partial bytes; permanent failures, incomplete pages and mismatched identities fail closed\n'
