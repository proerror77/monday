#!/usr/bin/env bash
# Operator recovery without longer artifact retention or new token permissions.
# Expired producer evidence requires a complete rerun, including the producer.
set -euo pipefail
run_id=${1:?workflow run ID required}
repo=${GITHUB_REPOSITORY:?repository required}
[[ $run_id =~ ^[0-9]+$ ]]
run=$(gh api "repos/$repo/actions/runs/$run_id")
[[ $(jq -r .status <<<"$run") == completed ]] || { echo 'wait for the active run to finish' >&2; exit 1; }
pages=$(gh api --paginate "repos/$repo/actions/runs/$run_id/jobs?filter=all&per_page=100")
jobs=$(jq -sce '.[0].total_count as $n | [.[].jobs[]] | if length==$n then . else error("incomplete jobs") end' <<<"$pages")
producer=$(jq -ce '[.[]|select(.name=="Loop nextest archive")] | (map(.run_attempt)|max) as $a | map(select(.run_attempt==$a)) | if length==1 then .[0] else error("ambiguous archive producer") end' <<<"$jobs")
if [[ $(jq -r .conclusion <<<"$producer") != success ]]; then
  gh run rerun "$run_id" --repo "$repo" --failed
  exit
fi
attempt=$(jq -er '.run_attempt|select(type=="number" and .>0 and floor==.)' <<<"$producer")
pages=$(gh api --paginate "repos/$repo/actions/runs/$run_id/artifacts?per_page=100")
artifacts=$(jq -sce '.[0].total_count as $n | [.[].artifacts[]] | if length==$n then . else error("incomplete artifacts") end' <<<"$pages")
if jq -e --arg archive "loop-nextest-archive-$run_id-$attempt" --arg plan "loop-nextest-expected-$run_id-$attempt" '
  ([.[]|select(.name==$archive and .expired==false)]|length)==1 and
  ([.[]|select(.name==$plan and .expired==false)]|length)==1' <<<"$artifacts" >/dev/null; then
  gh run rerun "$run_id" --repo "$repo" --failed
else
  echo 'Producer archive/plan expired or unavailable: rerunning all jobs, including the producer.' >&2
  gh run rerun "$run_id" --repo "$repo"
fi
