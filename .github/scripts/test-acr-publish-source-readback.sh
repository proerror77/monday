#!/usr/bin/env bash
set -euo pipefail
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
  */runs/100/attempts/2/jobs\?*) cat "$FAKE_ACR_STATE/jobs" ;;
  */workflows/acr-publish.yml/runs\?*) cat "$FAKE_ACR_STATE/publishers" ;;
  */runs/80/attempts/3/jobs\?*) cat "$FAKE_ACR_STATE/prior-jobs" ;;
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
  jq -n --arg sha "$source_sha" '[{artifacts:[{name:("research-image-release-"+$sha),expired:false,workflow_run:{id:100,head_sha:$sha}}]}]' > "$work/artifacts"
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
for failure in failure skipped cancelled timed_out; do
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
edit_fixture jobs '.[0].jobs |= map(.conclusion="skipped")'
read_state out_of_scope
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
jq -n --arg sha "$source_sha" '[{jobs:[{run_id:80,run_attempt:3,name:("Research release complete ("+$sha+")"),status:"completed",conclusion:"success"}]}]' > "$work/prior-jobs"
cp "$work/prior-jobs" "$work/marker-base"
read_state already_published
if grep -Fq /artifacts "$work/calls"; then
  echo 'completed publication needlessly revisited binary artifacts' >&2
  exit 1
fi
for mismatch in skipped-marker other-source previous-attempt wrong-run untrusted-workflow foreign-repository partial-pair; do
  cp "$work/publisher-base" "$work/publishers"
  cp "$work/marker-base" "$work/prior-jobs"
  rm "$work/out"
  case "$mismatch" in
    skipped-marker) edit_fixture prior-jobs '.[0].jobs[0].conclusion="skipped"' ;;
    other-source) edit_fixture prior-jobs '.[0].jobs[0].name="Research release complete (different)"' ;;
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
