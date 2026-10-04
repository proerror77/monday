#!/usr/bin/env bash
set -euo pipefail
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
printf 'PASS: unpublished A survives docs B; separate product readbacks suppress completed work; malformed or ambiguous baselines fail\n'
