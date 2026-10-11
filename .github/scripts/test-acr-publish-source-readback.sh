#!/usr/bin/env bash
set -euo pipefail
[[ $# == 0 || ( $# == 1 && $1 == --deferred-carry ) ]] || exit 2
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export FAKE_ACR_STATE=$work GITHUB_REPOSITORY=owner/repo
source_sha=1111111111111111111111111111111111111111
other_sha=2222222222222222222222222222222222222222
mkdir "$work/bin"
cat > "$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
endpoint=
for arg; do [[ $arg == repos/* ]] && endpoint=$arg; done
printf '%s\n' "$endpoint" >> "$FAKE_ACR_STATE/calls"
[[ ! -f $FAKE_ACR_STATE/api-failure ]] || exit 42
case "$endpoint" in
  */git/ref/heads/main) cat "$FAKE_ACR_STATE/main" ;;
  */check-runs\?*) cat "$FAKE_ACR_STATE/checks" ;;
  */workflows/ploy-ci.yml/runs\?*) cat "$FAKE_ACR_STATE/prediction" ;;
  */runs/100/attempts/2/jobs\?*|*/runs/100/attempts/3/jobs\?*) cat "$FAKE_ACR_STATE/jobs" ;;
  */workflows/acr-publish.yml/runs\?*) cat "$FAKE_ACR_STATE/publishers" ;;
  */runs/80/attempts/3/jobs\?*|*/runs/300/attempts/3/jobs\?*) cat "$FAKE_ACR_STATE/prior-jobs" ;;
  */runs/100/artifacts\?*) cat "$FAKE_ACR_STATE/artifacts" ;;
  *) printf 'unexpected endpoint %s\n' "$endpoint" >&2; exit 1 ;;
esac
MOCK
chmod +x "$work/bin/gh"
export PATH="$work/bin:$PATH"
reset_fixtures() {
  rm -f "$work/api-failure" "$work/out"
  : > "$work/calls"
  printf '%s\n' "$source_sha" > "$work/main"
  jq -n '[{check_runs:[
    {id:1,name:"Monorepo CI gate",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}},
    {id:2,name:"Prediction Markets CI gate",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}},
    {id:3,name:"Security Summary Report",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}}
  ]}]' > "$work/checks"
  jq -n --arg sha "$source_sha" '[{workflow_runs:[
    {id:100,run_attempt:2,head_sha:$sha,head_branch:"main",event:"push",path:".github/workflows/ploy-ci.yml",head_repository:{full_name:"owner/repo"},status:"completed",conclusion:"success"},
    {id:90,run_attempt:1,head_sha:$sha,head_branch:"main",event:"push",path:".github/workflows/ploy-ci.yml",head_repository:{full_name:"owner/repo"},status:"completed",conclusion:"success"}
  ]}]' > "$work/prediction"
  jq -n '[{jobs:[{run_id:100,run_attempt:2,name:"Research image binaries",status:"completed",conclusion:"success"},
    {run_id:100,run_attempt:2,name:"Research image smoke",status:"completed",conclusion:"success"}]}]' > "$work/jobs"
  jq -n '[{workflow_runs:[]}]' > "$work/publishers"
  jq -n --arg sha "$source_sha" '[{artifacts:[{name:("research-image-release-"+$sha+"-cex-runner,controller,prediction-runner"),expired:false,workflow_run:{id:100,head_sha:$sha}}]}]' > "$work/artifacts"
}
edit_fixture() {
  jq "$2" "$work/$1" > "$work/edited"
  mv "$work/edited" "$work/$1"
}
read_state() {
  "$script_dir/read-acr-publish-source.sh" "$source_sha" 200 "$work/out"
  test "$(sed -n 's/^automation_state=//p' "$work/out")" = "$1"
}
reject() {
  if "$script_dir/read-acr-publish-source.sh" "$source_sha" 200 "$work/out" >/dev/null 2>&1; then
    printf 'unexpected admission: %s\n' "$1" >&2; exit 1
  fi
}
# Use the real reader and cumulative planner with a baseline fixture. Native
# baseline authentication remains covered by test-main-research-scope.sh.
scope_repo="$work/scope-repo"
mkdir -p "$scope_repo/.github/scripts"
for script in read-acr-publish-source.sh read-release-required-checks.sh select-main-research-scope.sh select-rust-ci-scope.sh image-build-plan.sh research-release-products.sh research-release-products.json research-publication-budget.sh research-publication-budget.jq; do
  cp "$script_dir/$script" "$scope_repo/.github/scripts/$script"
done
mkdir -p "$scope_repo/rust_hft/scripts"
cp "$script_dir/fixtures/rust-ci-scope/metadata.fixture" "$scope_repo/rust_hft/scripts/metadata.fixture"
cat >"$scope_repo/rust_hft/scripts/workspace-metadata.sh" <<'MOCK'
#!/usr/bin/env bash
cat "$(dirname "$0")/metadata.fixture"
MOCK
chmod +x "$scope_repo/rust_hft/scripts/workspace-metadata.sh"
cat >"$scope_repo/.github/scripts/read-research-publish-baseline.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ ! -f $FAKE_ACR_STATE/baseline-failure ]] || exit 42
if [[ ${3:-images} == builds && -f $FAKE_ACR_STATE/archive-base ]]; then
  cp "$FAKE_ACR_STATE/archive-base" "$2"
else cp "$FAKE_ACR_STATE/pending-base" "$2"; fi
MOCK
git -C "$scope_repo" init -q
git -C "$scope_repo" add .
git -C "$scope_repo" -c user.name='CI contract' -c user.email=ci@example.invalid commit -qm 'scope fixture'
original_source=$source_sha
source_sha=$(git -C "$scope_repo" rev-parse HEAD)
scope_reader="$scope_repo/.github/scripts/read-acr-publish-source.sh"
printf '%s\n' '{"cex-runner":"BOOTSTRAP","controller":"BOOTSTRAP","prediction-runner":"BOOTSTRAP"}' >"$work/pending-base"
cp "$work/pending-base" "$work/pending-before"
for policy in '' '{"oss_by_product":{"controller":{}}}'; do
  reset_fixtures
  edit_fixture jobs '.[0].jobs |= map(.conclusion="skipped")'
  printf '[{"artifacts":[]}]\n' >"$work/artifacts"
  if (cd "$scope_repo" && RESEARCH_CARRY_MODE=defer-unconfigured MONDAY_RELEASE_POLICY_JSON="$policy" \
      SELECTED_RESEARCH_PRODUCT=prediction-runner SELECTED_JOBS=,ploy/rust-runner-lean, \
      bash "$scope_reader" "$source_sha" 200 "$work/out") >"$work/deferred-log" 2>&1; then
    echo 'pending publication became a successful out-of-scope run' >&2; exit 1
  fi
  grep -Fq 'pending research products cex-runner,controller,prediction-runner' "$work/deferred-log"
  if grep -Fq '/artifacts' "$work/calls"; then echo 'skipped producer attempted artifact reuse' >&2; exit 1; fi
done
cmp "$work/pending-before" "$work/pending-base"
touch "$work/baseline-failure"
if (cd "$scope_repo" && bash "$scope_reader" "$source_sha" 200 "$work/out") >"$work/rejected-baseline" 2>&1; then
  echo 'pending baseline failure became out of scope' >&2; exit 1
fi
rm "$work/baseline-failure"
printf '{}\n' >"$work/pending-base"
if (cd "$scope_repo" && bash "$scope_reader" "$source_sha" 200 "$work/out") >"$work/rejected-baseline" 2>&1; then
  echo 'missing pending baseline became out of scope' >&2; exit 1
fi
# A fully published source remains a genuine no-op.
jq -n --arg source "$source_sha" '{"cex-runner":$source,"controller":$source,"prediction-runner":$source}' >"$work/pending-base"
(cd "$scope_repo" && bash "$scope_reader" "$source_sha" 200 "$work/out")
grep -Fqx automation_state=out_of_scope "$work/out"
# Delivered images cannot hide an authorized missing signed Build. Use the
# production reader/planner and a docs-only current-main B: skipped software
# rejects pending archive; a successful exact-B producer admits B, never A.
jq -n --arg source "$source_sha" '{"cex-runner":$source,"controller":$source,"prediction-runner":$source}' >"$work/archive-base"
mkdir -p "$scope_repo/rust_hft/apps/backtest/src"
printf 'new software inputs A\n' >"$scope_repo/rust_hft/apps/backtest/src/example.rs"
git -C "$scope_repo" add .
git -C "$scope_repo" -c user.name='CI contract' -c user.email=ci@example.invalid commit -qm 'archive fixture software A'
delivered_source=$(git -C "$scope_repo" rev-parse HEAD)
jq -n --arg source "$delivered_source" '{"cex-runner":$source,"controller":$source,"prediction-runner":$source}' >"$work/pending-base"
printf 'docs B\n' >"$scope_repo/README.md"
git -C "$scope_repo" add .
git -C "$scope_repo" -c user.name='CI contract' -c user.email=ci@example.invalid commit -qm 'archive fixture docs B'
source_sha=$(git -C "$scope_repo" rev-parse HEAD)
(cd "$scope_repo" && bash .github/scripts/research-publication-budget.sh estimate "$source_sha" cex-runner 744 "$work/archive-estimate.json")
now=$(date -u +%s)
source_time=$(git -C "$scope_repo" show -s --format=%ct "$source_sha")
archive_allowance=$(jq -c --argjson now "$now" --argjson source_time "$source_time" '
 {schema:"monday.research-publication-operations-policy.v1",repository,publisher_workflow:".github/workflows/acr-publish.yml",
 products,not_before:($source_time-60),expires_at:($now+3600),history_anchor_run_id:99,history_anchor_run_number:1,
 history_retention_required:true,currency:"CNY",price_model:.pricing.model,storage_hours:.pricing.storage_hours,
 max_estimated_micro_cny:.pricing.estimated_micro_cny,max_oss_requests:.total.oss_requests,
 max_request_body_bytes:.total.request_body_bytes,max_response_body_bytes:.total.response_body_bytes,
 max_new_storage_bytes:.total.new_storage_bytes}' "$work/archive-estimate.json")
reset_fixtures
edit_fixture jobs '.[0].jobs |= map(.conclusion="skipped")'
if (cd "$scope_repo" && MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY=$archive_allowance \
    bash "$scope_reader" "$source_sha" 200 "$work/out") >"$work/pending-archive-log" 2>&1; then
  echo 'authorized pending Build became out of scope after OCI success' >&2
  cat "$work/pending-archive-log" >&2
  exit 1
fi
grep -Fq 'pending research products cex-runner' "$work/pending-archive-log"
# An unavailable planner dependency fails closed; command substitution must
# not turn a selector error into an empty pending set.
mv "$scope_repo/rust_hft/scripts/workspace-metadata.sh" "$work/metadata-helper"
reset_fixtures
edit_fixture jobs '.[0].jobs |= map(.conclusion="skipped")'
if (cd "$scope_repo" && MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY=$archive_allowance \
    bash "$scope_reader" "$source_sha" 200 "$work/out") >"$work/missing-metadata-log" 2>&1; then
  echo 'failed archive planner became out of scope' >&2; exit 1
fi
test ! -e "$work/out"
mv "$work/metadata-helper" "$scope_repo/rust_hft/scripts/workspace-metadata.sh"
reset_fixtures
edit_fixture artifacts '.[0].artifacts[0].name |= sub("cex-runner,controller,prediction-runner$";"cex-runner")'
(cd "$scope_repo" && MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY=$archive_allowance \
  bash "$scope_reader" "$source_sha" 200 "$work/out")
grep -Fqx automation_state=ready "$work/out"
grep -Fqx "main_sha=$source_sha" "$work/out"
grep -Fqx research_product=cex-runner "$work/out"
jq -e --arg source "$delivered_source" 'all(.[]; .==$source)' "$work/pending-base" >/dev/null
rm "$work/archive-base"
# Recovery can rerun all jobs of the same current-main push when its previous
# attempt had no artifact. Authenticate the new attempt, never an old job.
reset_fixtures
edit_fixture prediction '.[0].workflow_runs[0].run_attempt=3'
edit_fixture jobs '.[0].jobs |= map(.run_attempt=3)'
(cd "$scope_repo" && bash "$scope_reader" "$source_sha" 200 "$work/out")
grep -Fqx automation_state=ready "$work/out"
grep -Fqx artifact_run_id=100 "$work/out"
grep -Fq '/runs/100/attempts/3/jobs?' "$work/calls"
# An explicit manual rebuild uses the publisher's own authenticated producer.
"$script_dir/select-acr-publish-source.sh" --event workflow_dispatch --target prediction-research-runner \
  --rebuild true --current-ref refs/heads/main --current-sha "$source_sha" --current-run-id 200 \
  --main-sha "$source_sha" --monorepo-conclusion success --prediction-conclusion success \
  --security-conclusion success --output "$work/manual-rebuild"
grep -Fqx research_mode=rebuild "$work/manual-rebuild"
grep -Fqx artifact_run_id=200 "$work/manual-rebuild"
grep -Fqx published_products=prediction-runner "$work/manual-rebuild"
"$script_dir/select-acr-publish-source.sh" --event workflow_dispatch --target research-products \
  --rebuild true --current-ref refs/heads/main --current-sha "$source_sha" --current-run-id 200 \
  --main-sha "$source_sha" --monorepo-conclusion success --prediction-conclusion success \
  --security-conclusion success --output "$work/union-rebuild"
grep -Fqx publish_target=research-products "$work/union-rebuild"
grep -Fqx research_mode=rebuild "$work/union-rebuild"
grep -Fqx artifact_run_id=200 "$work/union-rebuild"
grep -Fqx "source_sha=$source_sha" "$work/union-rebuild"
grep -Fqx research_product=cex-runner,controller,prediction-runner "$work/union-rebuild"
grep -Fqx published_products=cex-runner,controller,prediction-runner "$work/union-rebuild"
source_sha=$original_source
printf 'PASS: skipped research work retains pending publication failure and exact-source recovery paths\n'
[[ ${1:-} != --deferred-carry ]] || exit 0
reset_fixtures
read_state ready
grep -Fqx artifact_run_id=100 "$work/out"
# Different completing workflow IDs must never replace Prediction provenance.
"$script_dir/select-acr-publish-source.sh" --event workflow_run --conclusion success \
  --source-event push --head-branch main --head-sha "$source_sha" --run-id 100 \
  --automation-state ready --binaries-conclusion success --smoke-conclusion success \
  --main-sha "$source_sha" --monorepo-conclusion success --prediction-conclusion success \
  --security-conclusion success --output "$work/selected"
grep -Fqx research_mode=artifact "$work/selected"
grep -Fqx artifact_run_id=100 "$work/selected"
for pending in missing queued in_progress waiting pending requested; do
  reset_fixtures
  if [[ $pending == missing ]]; then edit_fixture checks '.[0].check_runs |= .[1:]'
  else edit_fixture checks ".[0].check_runs[0].status=\"$pending\""; fi
  read_state deferred
  [[ $(wc -l < "$work/calls" | tr -d ' ') == 2 ]]
  "$script_dir/select-acr-publish-source.sh" --event workflow_run --conclusion success \
    --source-event push --head-branch main --head-sha "$source_sha" --run-id 200 \
    --automation-state deferred --output "$work/deferred-$pending"
  grep -Fqx publish_target=none "$work/deferred-$pending"
done
reset_fixtures
edit_fixture checks '.[0].check_runs[0].app.id=1'
read_state deferred
reset_fixtures
edit_fixture checks '.[0].check_runs[0].conclusion="skipped"'
read_state ready
for failure in failure cancelled timed_out; do
  reset_fixtures
  edit_fixture checks ".[0].check_runs[0].conclusion=\"$failure\""
  reject "required-$failure"
done
reset_fixtures
printf '%s\n' "$other_sha" > "$work/main"
read_state stale
[[ $(wc -l < "$work/calls" | tr -d ' ') == 1 ]]
reset_fixtures
edit_fixture prediction '.[0].workflow_runs[0].status="in_progress"'
read_state deferred
reset_fixtures
edit_fixture prediction '.[0].workflow_runs[0].conclusion="failure"'
reject latest-producer-failed
reset_fixtures
edit_fixture prediction '.[0].workflow_runs |= map(.head_repository.full_name="foreign/repo")'
reject foreign-producer
reset_fixtures
edit_fixture jobs '.[0].jobs[1].conclusion="skipped"'
reject partial-smoke
reset_fixtures
edit_fixture jobs '.[0].jobs += [.[0].jobs[0]]'
reject duplicate-job
reset_fixtures
edit_fixture jobs '.[0].jobs[0].run_attempt=1'
reject stale-attempt
reset_fixtures
edit_fixture artifacts '.[0].artifacts[0].expired=true'
reject expired-artifact
reset_fixtures
edit_fixture artifacts '.[0].artifacts[0].workflow_run.id=90'
reject wrong-producer-artifact

reset_fixtures
jq -n --arg sha "$source_sha" '[{workflow_runs:[{id:80,run_attempt:3,head_sha:$sha,head_branch:"main",event:"workflow_run",path:".github/workflows/acr-publish.yml",head_repository:{full_name:"owner/repo"},status:"completed",conclusion:"success"}]}]' > "$work/publishers"
cp "$work/publishers" "$work/publisher-base"
jq -n --arg sha "$source_sha" '[{jobs:[{run_id:80,run_attempt:3,name:("Research products published [cex-runner,controller,prediction-runner] ("+$sha+")"),status:"completed",conclusion:"success"}]}]' > "$work/prior-jobs"
cp "$work/prior-jobs" "$work/marker-base"
# A manual CEX-only publication cannot suppress the pending Prediction product.
edit_fixture prior-jobs '.[0].jobs[0].name="Research products published [cex-runner] (1111111111111111111111111111111111111111)"'
read_state ready
cp "$work/marker-base" "$work/prior-jobs"
rm "$work/out"
read_state already_published
if ! grep -Fq /artifacts "$work/calls"; then
  echo 'product coverage was not checked against the producer artifact metadata' >&2; exit 1
fi
# Rerunning an older publisher must still recognize a newer completed run.
edit_fixture publishers '.[0].workflow_runs[0].id=300'
edit_fixture prior-jobs '.[0].jobs[0].run_id=300'
rm "$work/out"
read_state already_published
# The current run is never its own prior completion evidence.
edit_fixture publishers '.[0].workflow_runs[0].id=200'
edit_fixture prior-jobs '.[0].jobs[0].run_id=200'
rm "$work/out"
read_state ready
for mismatch in skipped-marker other-source previous-attempt wrong-run untrusted-workflow foreign-repository partial-pair; do
  cp "$work/publisher-base" "$work/publishers"
  cp "$work/marker-base" "$work/prior-jobs"
  rm "$work/out"
  case "$mismatch" in
    skipped-marker) edit_fixture prior-jobs '.[0].jobs[0].conclusion="skipped"' ;;
    other-source) edit_fixture prior-jobs '.[0].jobs[0].name="Research products published [cex-runner,controller,prediction-runner] (2222222222222222222222222222222222222222)"' ;;
    previous-attempt) edit_fixture prior-jobs '.[0].jobs[0].run_attempt=2' ;;
    wrong-run) edit_fixture prior-jobs '.[0].jobs[0].run_id=79' ;;
    untrusted-workflow) edit_fixture publishers '.[0].workflow_runs[0].path=".github/workflows/foreign.yml"' ;;
    foreign-repository) edit_fixture publishers '.[0].workflow_runs[0].head_repository.full_name="foreign/repo"' ;;
    partial-pair) edit_fixture publishers '.[0].workflow_runs[0].conclusion="failure"' ;;
  esac
  read_state ready
done
reset_fixtures
touch "$work/api-failure"
reject api-failure
# Execute the actual manual admission block: the shared reader must not replace
# the preceding main_sha output, and the diagnostic exception stays check-free.
awk '
  /^      - name: Read authenticated release admission$/ { selected=1; next }
  selected && /^        run: \|$/ { printing=1; next }
  printing && /^      - name:/ { exit }
  printing { sub(/^          /, ""); print }
' "$script_dir/../workflows/acr-publish.yml" > "$work/manual-admission.sh"
reset_fixtures
(cd "$script_dir/../.." && GITHUB_OUTPUT="$work/manual-output" RUNNER_TEMP="$work" \
  CURRENT_SHA="$source_sha" PUBLISH_TARGET=hft-trading bash "$work/manual-admission.sh")
grep -Fqx "main_sha=$source_sha" "$work/manual-output"
grep -Fqx monorepo_conclusion=success "$work/manual-output"
grep -Fqx prediction_conclusion=success "$work/manual-output"
grep -Fqx security_conclusion=success "$work/manual-output"
reset_fixtures
(cd "$script_dir/../.." && GITHUB_OUTPUT="$work/diagnostic-output" RUNNER_TEMP="$work" \
  CURRENT_SHA="$source_sha" PUBLISH_TARGET=research-source-test bash "$work/manual-admission.sh")
grep -Fqx "main_sha=$source_sha" "$work/diagnostic-output"
[[ $(wc -l < "$work/calls" | tr -d ' ') == 1 ]]
printf 'ACR event-driven source readback tests passed\n'

# Manual research publication reuses the independently verified producer, never
# the current publisher's run ID and never an implicit compilation fallback.
reset_fixtures
"$script_dir/read-acr-publish-source.sh" "$source_sha" 200 "$work/reusable" reuse
grep -Fqx automation_state=ready "$work/reusable"
grep -Fqx artifact_run_id=100 "$work/reusable"
if grep -Fq '/workflows/acr-publish.yml/runs' "$work/calls"; then exit 1; fi
manual_reuse() {
  "$script_dir/select-acr-publish-source.sh" --event workflow_dispatch --target "${3:-research-runner}" \
    --rebuild false --current-ref refs/heads/main --current-sha "$source_sha" --current-run-id 200 \
    --main-sha "$source_sha" --monorepo-conclusion success --prediction-conclusion success \
    --security-conclusion success --run-id 100 --automation-state "$1" \
    --binaries-conclusion success --smoke-conclusion "$2" --output "$work/manual-reuse"
}
manual_reuse ready success
grep -Fqx research_mode=artifact "$work/manual-reuse"
grep -Fqx artifact_run_id=100 "$work/manual-reuse"
grep -Fqx published_products=cex-runner "$work/manual-reuse"
manual_reuse ready success research-products
grep -Fqx research_mode=artifact "$work/manual-reuse"
grep -Fqx artifact_run_id=100 "$work/manual-reuse"
grep -Fqx "source_sha=$source_sha" "$work/manual-reuse"
grep -Fqx publish_target=research-products "$work/manual-reuse"
grep -Fqx published_products=cex-runner,controller,prediction-runner "$work/manual-reuse"
# The same verified union may publish only Prediction. A CEX-only producer
# cannot be reused for that target or for an all-products publication.
"$script_dir/select-acr-publish-source.sh" --event workflow_dispatch --target prediction-research-runner \
  --product prediction-runner --rebuild false --current-ref refs/heads/main --current-sha "$source_sha" --current-run-id 200 \
  --main-sha "$source_sha" --monorepo-conclusion success --prediction-conclusion success --security-conclusion success \
  --run-id 100 --automation-state ready --binaries-conclusion success --smoke-conclusion success --output "$work/prediction-reuse"
grep -Fqx published_products=prediction-runner "$work/prediction-reuse"
for target in prediction-research-runner research-products all; do
  if "$script_dir/select-acr-publish-source.sh" --event workflow_dispatch --target "$target" \
    --product cex-runner --rebuild false --current-ref refs/heads/main --current-sha "$source_sha" --current-run-id 200 \
    --main-sha "$source_sha" --monorepo-conclusion success --prediction-conclusion success --security-conclusion success \
    --run-id 100 --automation-state ready --binaries-conclusion success --smoke-conclusion success --output "$work/wrong-product" >"$work/rejection" 2>&1; then
    echo 'foreign product reuse accepted' >&2; exit 1
  fi
done
for target in research-runner research-products; do
  for state in deferred stale out_of_scope; do
    if manual_reuse "$state" success "$target"; then exit 1; fi
  done
  if manual_reuse ready failure "$target"; then exit 1; fi
done
reset_fixtures
edit_fixture artifacts '.[0].artifacts[0].expired=true'
if "$script_dir/read-acr-publish-source.sh" "$source_sha" 200 "$work/expired-reuse" reuse; then exit 1; fi
ruby -ryaml - "$script_dir/../workflows/acr-publish.yml" "$script_dir/../workflows/ploy-ci.yml" <<'RUBY'
acr,ploy=ARGV.map { |p| YAML.load_file(p) }
reader=acr['jobs']['selector']['steps'].find { |s| s['id']=='source-jobs' }
abort 'manual reuse skips authenticated readback' unless reader['if'].include?("github.event_name == 'workflow_dispatch'") && reader['if'].include?("inputs.rebuild_research_runner != true")
abort 'union reuse skips authenticated readback' unless reader['if'].include?("inputs.target == 'research-products'")
[[acr,'research-runner-binaries'],[ploy,'research-image-binaries']].each do |doc,id|
  upload=doc['jobs'][id]['steps'].find { |s| s.fetch('uses','').include?('actions/upload-artifact@') }
  abort 'software retention is shorter than the release window' unless upload['with']['retention-days']==7
end
RUBY
printf 'PASS: manual publication reuses current verified software, rejects missing/expired proof, and retains it for seven days\n'
