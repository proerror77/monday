#!/usr/bin/env bash
# A successful no-op publisher is not a baseline. Only an authenticated marker
# after image readback proves publication. This reader does not dispatch work.
set -euo pipefail
head=${1:?current source required} output=${2:?baseline output required}
: "${GITHUB_REPOSITORY:?}"
[[ $head =~ ^[0-9a-f]{40}$ ]]
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# The product catalog starts a new release contract. Older bundle markers do
# not prove publication of these products. Query its complete publication era.
migration=$(git log --diff-filter=A -1 --format=%H -- .github/scripts/research-release-products.json)
[[ $migration =~ ^[0-9a-f]{40}$ ]] || { echo 'research product migration is missing' >&2; exit 1; }
since=$(git show -s --format=%ct "$migration")
since=$(ruby -e 'puts Time.at(Integer(ARGV[0])).utc.strftime("%Y-%m-%dT%H:%M:%SZ")' "$since")
gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/workflows/acr-publish.yml/runs?branch=main&status=success&created=%3E%3D$since&per_page=100" >"$work/runs.json"
jq -e 'length>0 and all(.[]; (.workflow_runs|type=="array") and (.total_count|type=="number" and .>=0))' "$work/runs.json" >/dev/null
jq -r --arg repo "$GITHUB_REPOSITORY" '
  [.[].workflow_runs[]? | select(.head_branch=="main" and .head_repository.full_name==$repo and
    .path==".github/workflows/acr-publish.yml" and (.event=="workflow_run" or .event=="workflow_dispatch") and
    .status=="completed" and .conclusion=="success")] | sort_by(.id) | reverse |
  .[] | [.id,.run_attempt,.head_sha] | @tsv' "$work/runs.json" >"$work/runs.tsv"
runner=BOOTSTRAP
controller=BOOTSTRAP
while IFS=$'\t' read -r run attempt source; do
  [[ $run =~ ^[1-9][0-9]*$ && $attempt =~ ^[1-9][0-9]*$ && $source =~ ^[0-9a-f]{40}$ ]] || exit 1
  git merge-base --is-ancestor "$migration" "$source" || continue
  gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$run/attempts/$attempt/jobs?per_page=100" >"$work/jobs.json"
  marker=$(jq -er --argjson run "$run" --argjson attempt "$attempt" --arg source "$source" '
    [.[].jobs[]? | select(.run_id==$run and .run_attempt==$attempt and .status=="completed" and .conclusion=="success" and
      (.name | startswith("Research release complete")))] |
    if length==0 then "none" elif length!=1 then error("ambiguous publication marker") else .[0].name |
      if .==("Research release complete [paired] ("+$source+")") or .==("Research release complete [runner] ("+$source+")") or .==("Research release complete ("+$source+")") then "paired"
      elif .==("Research release complete [controller] ("+$source+")") then "controller"
      else error("publication marker source mismatch") end end' "$work/jobs.json")
  if [[ $marker != none ]]; then
    git cat-file -e "$source^{commit}"
    git merge-base --is-ancestor "$source" "$head" || { echo 'published research source is outside current history' >&2; exit 1; }
    [[ $controller != BOOTSTRAP ]] || controller=$source
    if [[ $marker == paired && $runner == BOOTSTRAP ]]; then runner=$source; fi
    [[ $runner == BOOTSTRAP || $controller == BOOTSTRAP ]] || break
  fi
done <"$work/runs.tsv"
# No paired publication exists in authenticated history. Bootstrap both actual
# images from current source. A later no-op commit also selects both until this
# first readback succeeds; it cannot erase pending work.
jq -n --arg runner "$runner" --arg controller "$controller" '{runner:$runner,controller:$controller}' >"$output"
