#!/usr/bin/env bash
# One main SHA publishes once: earlier CI completions stay quiet, the first
# all-green wakeup publishes, and a later wakeup sees the published state.
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$script_dir/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
sha=$(printf 'a%.0s' {1..40})
other=$(printf 'b%.0s' {1..40})
digest=$(printf 'c%.0s' {1..64})

write_states() {
  printf 'monorepo_conclusion=%s\nprediction_conclusion=%s\nsecurity_conclusion=%s\n' \
    "$1" "$2" "$3" >"$work/states"
}
write_published() {
  printf 'ghcr=%s\nacr=%s\n' "$1" "$2" >"$work/published"
}
result_file=$work/out
decide() {
  result_file=$work/out
  : >"$result_file"
  SOURCE_SHA=$sha IS_CURRENT_MAIN=${1:-true} GITHUB_OUTPUT=$result_file \
    "$script_dir/decide-release-once.sh" "$work/states" "$work/published"
}
field() { sed -n "s/^$1=//p" "$result_file"; }

sim_gh=false
sim_ac=false
decisions=0
simulate() {
  local name=$1 monorepo=$2 prediction=$3 security=$4
  write_states "$monorepo" "$prediction" "$security"
  write_published "$sim_gh" "$sim_ac"
  decide true
  local reason ghcr acr
  reason=$(field reason)
  ghcr=$(field publish_ghcr)
  acr=$(field publish_acr)
  printf 'wakeup=%s checks=%s/%s/%s published=%s/%s -> ghcr=%s acr=%s reason=%s\n' \
    "$name" "$monorepo" "$prediction" "$security" "$sim_gh" "$sim_ac" "$ghcr" "$acr" "$reason"
  [[ $(field source_sha) == "$sha" ]]
  if [[ $ghcr == true || $acr == true ]]; then
    [[ $reason == publish ]]
    decisions=$((decisions + 1))
    [[ $ghcr == true ]] && sim_gh=true
    [[ $acr == true ]] && sim_ac=true
  else
    [[ $reason != publish ]]
  fi
}

printf '%s\n' '--- one SHA, three upstream completions ---'
simulate security-finished success missing in_progress
[[ $(field reason) == checks-pending && $(field publish_ghcr) == false && $(field publish_acr) == false ]]
simulate monorepo-finished success in_progress success
[[ $(field reason) == checks-pending ]]
simulate all-required-green success success success
[[ $(field reason) == publish && $(field publish_ghcr) == true && $(field publish_acr) == true ]]
simulate later-green-wakeup success success success
[[ $(field reason) == already-published && $(field publish_ghcr) == false && $(field publish_acr) == false ]]
[[ $decisions == 1 ]]
printf 'publish_decisions=%s\n' "$decisions"

printf '%s\n' '--- failed check stays quiet ---'
sim_gh=false
sim_ac=false
decisions=0
simulate monorepo-failed failure success success
[[ $(field reason) == checks-not-green && $decisions == 0 ]]

printf '%s\n' '--- stale main stays quiet ---'
write_states success success success
write_published false false
decide false
[[ $(field reason) == stale-main && $(field publish_ghcr) == false && $(field publish_acr) == false ]]

printf '%s\n' '--- existing GHCR tag publishes only ACR ---'
write_states success success success
write_published true false
decide true
[[ $(field reason) == publish && $(field publish_ghcr) == false && $(field publish_acr) == true ]]
write_published true true
decide true
[[ $(field reason) == already-published && $(field publish_ghcr) == false && $(field publish_acr) == false ]]

write_states success success banana
if decide true >"$work/invalid" 2>&1; then
  echo 'invalid check state was admitted' >&2
  exit 1
fi

ruby -ryaml - "$root/.github/workflows/release.yml" \
  "$root/.github/workflows/docker-publish.yml" \
  "$root/.github/workflows/acr-publish.yml" <<'RUBY'
release, docker, acr = ARGV.map { |path| YAML.safe_load(File.read(path)) }
on = release['on'] || release[true]
expected = ['Monorepo CI', 'Prediction Markets CI', 'Security & Quality (ENABLED)']
abort 'release is not main-only' unless on.keys == ['workflow_run']
wakeup = on.fetch('workflow_run')
abort 'release wakeup changed' unless wakeup.fetch('workflows').sort == expected && wakeup.fetch('types') == ['completed'] && wakeup.fetch('branches') == ['main']
abort 'release concurrency is not one queued group per SHA' unless release.fetch('concurrency') == {
  'group' => 'release-${{ github.event.workflow_run.head_sha }}',
  'cancel-in-progress' => false
}
ghcr = release.fetch('jobs').fetch('publish-ghcr')
acr_job = release.fetch('jobs').fetch('publish-acr')
abort 'GHCR is not the reusable publisher' unless ghcr.fetch('uses') == './.github/workflows/docker-publish.yml'
abort 'ACR is not the reusable publisher' unless acr_job.fetch('uses') == './.github/workflows/acr-publish.yml'
abort 'GHCR call is not gated on one admission output' unless ghcr.fetch('if').include?("outputs.publish_ghcr == 'true'")
abort 'ACR call is not gated on one admission output' unless acr_job.fetch('if').include?("outputs.publish_acr == 'true'")
[docker, acr].each do |doc|
  triggers = doc['on'] || doc[true]
  abort 'publisher still starts from workflow_run' if triggers.key?('workflow_run') || triggers.key?('pull_request')
  abort 'manual workflow_dispatch was removed' unless triggers.key?('workflow_dispatch')
  abort 'publisher is not reusable' unless triggers.dig('workflow_call', 'inputs', 'source_sha', 'required') == true
end
abort 'GHCR lost tag publication' unless (docker['on'] || docker[true]).dig('push', 'tags') == ['v*']
abort 'GHCR lost the hft-core scope guard' unless File.read(ARGV[1]).include?('any(.include[]; .name=="hft-core")')
RUBY

docker_extra="$work/docker-extra.yml"
sed 's@.name=="hft-core"@.name=="research-runner"@' \
  "$root/.github/workflows/docker-publish.yml" >"$docker_extra"
if ruby -ryaml -e 'abort unless YAML.safe_load(File.read(ARGV[0])).to_s.include?(%q{any(.include[]; .name=="hft-core")})' "$docker_extra"; then
  echo 'Docker Publish contract accepted an unrelated image path' >&2
  exit 1
fi
release_pr="$work/release-pr.yml"
ruby -ryaml - "$root/.github/workflows/release.yml" "$release_pr" <<'RUBY'
doc = YAML.safe_load(File.read(ARGV[0]))
triggers = doc['on'] || doc[true]
triggers['pull_request'] = {'branches' => ['main']}
File.write(ARGV[1], doc.to_yaml)
RUBY
if ruby -ryaml -e 'on=(YAML.safe_load(File.read(ARGV[0]))["on"]||YAML.safe_load(File.read(ARGV[0]))[true]); abort unless on.keys==["workflow_run"]' "$release_pr"; then
  echo 'Release entry accepted a pull request wakeup' >&2
  exit 1
fi

export GITHUB_REPOSITORY=owner/repo GITHUB_RUN_ID=99
mkdir -p "$work/bin"
cat >"$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
endpoint=
for arg; do [[ $arg == repos/* || $arg == /users/* || $arg == /orgs/* ]] && endpoint=$arg; done
printf '%s\n' "$endpoint" >> "${FAKE_RELEASE_STATE:?}/calls"
[[ ! -f ${FAKE_RELEASE_STATE}/api-failure ]] || exit 42
case "$endpoint" in
  */git/ref/heads/main) cat "$FAKE_RELEASE_STATE/main" ;;
  */check-runs\?*) cat "$FAKE_RELEASE_STATE/checks" ;;
  */workflows/release.yml/runs\?*) cat "$FAKE_RELEASE_STATE/runs" ;;
  */runs/*/attempts/*/jobs\?*) cat "$FAKE_RELEASE_STATE/jobs" ;;
  */users/*/packages/container/hft/versions\?*) cat "$FAKE_RELEASE_STATE/user-versions" ;;
  */orgs/*/packages/container/hft/versions\?*) cat "$FAKE_RELEASE_STATE/org-versions" ;;
  *) printf 'unexpected endpoint %s\n' "$endpoint" >&2; exit 1 ;;
esac
MOCK
cat >"$work/bin/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
headerfile= bodyfile= url=
args=("$@")
i=0
while [[ $i -lt ${#args[@]} ]]; do
  case "${args[$i]}" in
    -D) i=$((i + 1)); headerfile=${args[$i]} ;;
    -o) i=$((i + 1)); bodyfile=${args[$i]} ;;
    https:*) url=${args[$i]} ;;
  esac
  i=$((i + 1))
done
repo=${url#*wildcard0923/}
repo=${repo%%/*}
code=$(cat "$FAKE_RELEASE_STATE/acr-$repo")
if [[ $code == 200 ]]; then
  printf 'HTTP/1.1 200 OK\r\nDocker-Content-Digest: sha256:%s\r\n' "$(cat "$FAKE_RELEASE_STATE/digest")" >"$headerfile"
else
  printf 'HTTP/1.1 %s\r\n' "$code" >"$headerfile"
fi
printf '%s' "$code" >"$bodyfile"
printf '%s' "$code"
MOCK
chmod 0755 "$work/bin/gh" "$work/bin/curl"
export PATH="$work/bin:$PATH" FAKE_RELEASE_STATE=$work
reset_api() {
  : >"$work/calls"
  rm -f "$work/api-failure"
  jq -n --arg sha "$sha" '{object:{sha:$sha}}' >"$work/main"
  jq -n '[{check_runs:[
    {id:1,name:"Monorepo CI gate",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}},
    {id:2,name:"Prediction Markets CI gate",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}},
    {id:3,name:"Security Summary Report",status:"completed",conclusion:"success",app:{id:15368,slug:"github-actions"}}
  ]}]' >"$work/checks"
  jq -n '[{total_count:0,workflow_runs:[]}]' >"$work/runs"
  jq -n '[{jobs:[]}]' >"$work/jobs"
  jq -n '[[]]' >"$work/user-versions"
  jq -n '[[]]' >"$work/org-versions"
  printf '%s\n' 404 >"$work/acr-research-runner"
  printf '%s\n' 404 >"$work/acr-prediction-research-runner"
  printf '%s\n' 404 >"$work/acr-campaign-cycle-controller"
  printf '%s\n' "$digest" >"$work/digest"
}
admit() {
  result_file=$work/gate
  : >"$result_file"
  GITHUB_OUTPUT=$result_file "$script_dir/release-orchestrator-admit.sh" "$1"
}

reset_api
jq -n --argjson run "$(jq '.[0]' "$work/checks")" '[$run | .check_runs[1].status="in_progress" | .check_runs[1].conclusion=null]' >"$work/checks"
admit "$sha"
[[ $(field reason) == checks-pending ]]
if grep -Eq 'packages/container|workflows/release.yml/runs' "$work/calls"; then
  echo 'pending checks looked up publication state' >&2
  exit 1
fi

reset_api
jq -n --arg sha "$other" '{object:{sha:$sha}}' >"$work/main"
admit "$sha"
[[ $(field reason) == stale-main ]]
if grep -q check-runs "$work/calls"; then
  echo 'stale main read required checks' >&2
  exit 1
fi

reset_api
# user package 404 falls through to the org package, where the sha tag exists.
jq -n --arg tag "sha-${sha:0:7}" --arg digest "sha256:$digest" \
  '[[{name:$digest,metadata:{container:{tags:[$tag,"main"]}}}]]' >"$work/org-versions"
printf 'gh: Not Found (HTTP 404)\n' >"$work/user-versions"
# Make the user endpoint fail as 404 by exiting after printing the message.
cat >"$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
endpoint=
for arg; do [[ $arg == repos/* || $arg == /users/* || $arg == /orgs/* ]] && endpoint=$arg; done
printf '%s\n' "$endpoint" >> "${FAKE_RELEASE_STATE:?}/calls"
[[ ! -f ${FAKE_RELEASE_STATE}/api-failure ]] || exit 42
case "$endpoint" in
  */git/ref/heads/main) cat "$FAKE_RELEASE_STATE/main" ;;
  */check-runs\?*) cat "$FAKE_RELEASE_STATE/checks" ;;
  */workflows/release.yml/runs\?*) cat "$FAKE_RELEASE_STATE/runs" ;;
  */runs/*/attempts/*/jobs\?*) cat "$FAKE_RELEASE_STATE/jobs" ;;
  */users/*/packages/container/hft/versions\?*)
    printf 'gh: Not Found (HTTP 404)\n' >&2
    exit 1 ;;
  */orgs/*/packages/container/hft/versions\?*) cat "$FAKE_RELEASE_STATE/org-versions" ;;
  *) printf 'unexpected endpoint %s\n' "$endpoint" >&2; exit 1 ;;
esac
MOCK
chmod 0755 "$work/bin/gh"
export ACR_REGISTRY=registry.example ACR_USERNAME=user ACR_PASSWORD=secret
admit "$sha"
[[ $(field reason) == publish && $(field publish_ghcr) == false && $(field publish_acr) == true ]]
for repository in research-runner prediction-research-runner campaign-cycle-controller; do
  printf '%s\n' 200 >"$work/acr-$repository"
done
admit "$sha"
[[ $(field reason) == already-published && $(field publish_ghcr) == false && $(field publish_acr) == false ]]

reset_api
jq -n --arg sha "$sha" '[{total_count:1,workflow_runs:[{id:50,run_attempt:1,head_sha:$sha,head_branch:"main",event:"workflow_run",path:".github/workflows/release.yml",head_repository:{full_name:"owner/repo"},status:"completed",conclusion:"success"}]}]' >"$work/runs"
jq -n '[{jobs:[
  {run_id:50,run_attempt:1,name:"Publish GHCR",status:"completed",conclusion:"success"},
  {run_id:50,run_attempt:1,name:"Publish ACR",status:"completed",conclusion:"success"},
  {run_id:50,run_attempt:1,name:"Admit release",status:"completed",conclusion:"success"}
]}]' >"$work/jobs"
unset ACR_REGISTRY ACR_USERNAME ACR_PASSWORD
admit "$sha"
[[ $(field reason) == already-published ]]

reset_api
jq -n --arg sha "$sha" '[{total_count:1,workflow_runs:[{id:50,run_attempt:1,head_sha:$sha,head_branch:"main",event:"workflow_run",path:".github/workflows/release.yml",head_repository:{full_name:"owner/repo"},status:"completed",conclusion:"success"}]}]' >"$work/runs"
jq -n '[{jobs:[
  {run_id:50,run_attempt:1,name:"Publish GHCR",status:"completed",conclusion:"skipped"},
  {run_id:50,run_attempt:1,name:"Publish ACR",status:"completed",conclusion:"failure"}
]}]' >"$work/jobs"
admit "$sha"
[[ $(field reason) == publish && $(field publish_ghcr) == true && $(field publish_acr) == true ]]

printf 'release once contract passed\n'
