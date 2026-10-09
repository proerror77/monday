#!/usr/bin/env bash
# Metadata fixtures test routing and rejection; they are not release evidence.
set -euo pipefail
[[ $# == 0 || ( $# == 1 && $1 == --metadata-only ) ]] || exit 2
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$script_dir/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export RELEASE_SOURCE_FIXTURE=$work GITHUB_REPOSITORY=owner/repo GITHUB_EVENT_NAME=workflow_run
export GITHUB_EVENT_PATH=$work/event.json
source_sha=$(printf 'a%.0s' {1..40})
controller_sha=$(printf 'b%.0s' {1..40})
mkdir "$work/bin"
cat >"$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
endpoint=
for arg; do [[ $arg == repos/* ]] && endpoint=$arg; done
printf '%s\n' "$endpoint" >>"$RELEASE_SOURCE_FIXTURE/calls"
[[ ! -f $RELEASE_SOURCE_FIXTURE/api-failure ]] || exit 42
case "$endpoint" in
  repos/owner/repo/actions/runs/500/attempts/2) cat "$RELEASE_SOURCE_FIXTURE/release.json" ;;
  repos/owner/repo/actions/runs/500/attempts/2/jobs\?per_page=100) cat "$RELEASE_SOURCE_FIXTURE/jobs.json" ;;
  repos/owner/repo/actions/runs/100/attempts/3) cat "$RELEASE_SOURCE_FIXTURE/ci.json" ;;
  repos/owner/repo/git/ref/heads/main) cat "$RELEASE_SOURCE_FIXTURE/main" ;;
  *) printf 'unexpected source API endpoint: %s\n' "$endpoint" >&2; exit 1 ;;
esac
MOCK
chmod +x "$work/bin/gh"
export PATH="$work/bin:$PATH"
reset_case() {
  rm -f "$work/out" "$work/api-failure" "$work/admitted"
  : >"$work/calls"
  printf '{"workflow_run":{"id":500,"run_attempt":2}}\n' >"$work/event.json"
  jq -n --arg head "$controller_sha" '{id:500,run_attempt:2,head_sha:$head,head_branch:"main",
    repository:{full_name:"owner/repo"},head_repository:{full_name:"owner/repo"},
    path:".github/workflows/release.yml",event:"workflow_run",status:"completed",conclusion:"success"}' >"$work/release.json"
  jq -n --arg source "$source_sha" --arg head "$controller_sha" '[{jobs:[{
    run_id:500,run_attempt:2,head_sha:$head,status:"completed",conclusion:"success",
    name:("Release source v1 ["+$source+"] [100/3]") }]}]' >"$work/jobs.json"
  jq -n --arg source "$source_sha" '{id:100,run_attempt:3,head_sha:$source,head_branch:"main",
    repository:{full_name:"owner/repo"},head_repository:{full_name:"owner/repo"},
    path:".github/workflows/ploy-ci.yml",event:"push",status:"completed",conclusion:"success"}' >"$work/ci.json"
  printf '%s\n' "$controller_sha" >"$work/main"
}
edit_case() {
  jq "$2" "$work/$1.json" >"$work/edit"
  mv "$work/edit" "$work/$1.json"
}
read_source() { bash "$script_dir/read-release-source.sh" "$work/out"; }
reject_case() {
  if read_source >"$work/rejected.log" 2>&1; then
    printf 'invalid Release source accepted: %s\n' "$1" >&2; exit 1
  fi
  [[ ! -s $work/out ]]
}

# CI source A must remain A when Release is orchestrated from newer main B.
reset_case
read_source
grep -Fqx "source_sha=$source_sha" "$work/out"
grep -Fqx source_ci_run_id=100 "$work/out"
grep -Fqx source_ci_run_attempt=3 "$work/out"
resolved=$(sed -n 's/^source_sha=//p' "$work/out")
bash "$script_dir/read-acr-publish-source.sh" "$resolved" 900 "$work/admitted"
grep -Fqx automation_state=stale "$work/admitted"
if grep -Eq 'check-runs|workflows/ploy-ci.yml/runs|/artifacts' "$work/calls"; then
  echo 'stale original source queried newer-main checks or binaries' >&2; exit 1
fi
# The matching controller case and all three upstream workflows remain valid.
for workflow in ci.yml ploy-ci.yml security-enabled.yml; do
  reset_case
  edit_case release ".head_sha=\"$source_sha\""
  edit_case jobs ".[0].jobs[0].head_sha=\"$source_sha\""
  edit_case ci ".path=\".github/workflows/$workflow\""
  read_source
  grep -Fqx "source_sha=$source_sha" "$work/out"
done
# GHCR failure cannot replace the independent ACR admission decision.
reset_case
edit_case release '.conclusion="failure"'
read_source
grep -Fqx "source_sha=$source_sha" "$work/out"

for update in 'del(.workflow_run.id)' 'del(.workflow_run.run_attempt)' \
  '.workflow_run.id="500"' '.workflow_run.run_attempt=0'; do
  reset_case; edit_case event "$update"; reject_case "event $update"
done
for document in release ci; do
  for update in '.id=999' '.run_attempt=9' '.repository.full_name="foreign/repo"' \
    '.head_repository.full_name="foreign/repo"' '.path=".github/workflows/other.yml"' \
    '.event="pull_request"' '.head_branch="develop"' '.status="in_progress"' 'del(.head_sha)' \
    'del(.run_attempt)' 'del(.repository)' 'del(.head_repository)'; do
    reset_case; edit_case "$document" "$update"; reject_case "$document $update"
  done
done
for update in '.[0].jobs=[]' '.[0].jobs += .[0].jobs' '. + .' '.[0].jobs[0].run_id=999' \
  '.[0].jobs[0].run_attempt=1' '.[0].jobs[0].head_sha="wrong"' \
  '.[0].jobs[0].status="in_progress"' '.[0].jobs[0].conclusion="failure"' \
  '.[0].jobs[0].name="Release source v1 [invalid] [100/3]"' \
  '.[0].jobs[0].name |= sub("100/3";"100/1")' \
  'del(.[0].jobs[0].run_attempt)' 'del(.[0].jobs[0].head_sha)' '.[0].jobs=null'; do
  reset_case; edit_case jobs "$update"; reject_case "marker $update"
done
reset_case
edit_case ci ".head_sha=\"$controller_sha\""
reject_case 'original CI source differs from marker'
reset_case
touch "$work/api-failure"
reject_case 'API failure'
reset_case
if GITHUB_EVENT_NAME=workflow_dispatch read_source; then echo 'manual request entered automatic decoder' >&2; exit 1; fi
[[ ! -s $work/out ]]
printf '%s\n' .github/scripts/read-release-source.sh .github/scripts/test-release-source.sh >"$work/paths"
bash "$script_dir/select-rust-ci-scope.sh" --event pull_request --changed-files "$work/paths" --output "$work/scope"
grep -Fqx research_product=none "$work/scope"
grep -Fqx 'jobs=,ci/ci-contracts,ploy/commit-hygiene,ploy/workflow-lint,' "$work/scope"
printf 'PASS: exact Release/CI attempts preserve original source and reject missing, stale or ambiguous metadata\n'
[[ ${1:-} != --metadata-only ]] || exit 0

ruby -ryaml - "$root/.github/workflows/release.yml" "$root/.github/workflows/acr-publish.yml" <<'RUBY'
release, acr = ARGV.map { |file| YAML.load_file(file) }
marker = release.fetch('jobs').fetch('source')
abort 'original admission name changed' unless release.dig('jobs','admit','name') == 'Admit release'
abort 'marker depends on publication' if marker.key?('needs')
abort 'marker has write permissions' unless marker.fetch('permissions') == {}
abort 'marker source changed' unless marker.fetch('name') == 'Release source v1 [${{ github.event.workflow_run.head_sha }}] [${{ github.event.workflow_run.id }}/${{ github.event.workflow_run.run_attempt }}]'
abort 'marker does not retain main-push guard' unless marker.fetch('if') == release.dig('jobs','admit','if')
steps = acr.fetch('jobs').fetch('selector').fetch('steps')
index = steps.index { |step| step['id'] == 'release-source' }
reader_index = steps.index { |step| step['id'] == 'source-jobs' }
decoder = steps.fetch(index)
abort 'source decoder missing or too late' unless index < reader_index
abort 'manual request uses automatic decoder' unless decoder.fetch('if') == "github.event_name == 'workflow_run'"
abort 'automatic source decoder changed' unless decoder.fetch('run') == 'bash .github/scripts/read-release-source.sh "$GITHUB_OUTPUT"'
reader = steps.fetch(reader_index)
abort 'binary readback lost original source' unless reader.dig('env','SOURCE_SHA') == "${{ github.event_name == 'workflow_dispatch' && github.sha || steps.release-source.outputs.source_sha }}"
selection = steps.find { |step| step['id'] == 'source' }
abort 'ACR selection lost original source' unless selection.dig('env','HEAD_SHA') == '${{ steps.release-source.outputs.source_sha }}'
abort 'manual current source changed' unless selection.dig('env','CURRENT_SHA') == '${{ github.sha }}'
RUBY
printf 'PASS: native marker and ACR workflow source bindings retain manual and publication boundaries\n'
