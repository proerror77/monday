#!/usr/bin/env bash
# Read whether this SHA was already published. Absence is false.
# An unreadable Release history or GHCR answer exits 1 so a second wakeup
# cannot publish without knowing the first result. Require the ACR proof marker.
set +x
set -euo pipefail
source_sha=${1:?usage: read-release-published.sh SOURCE_SHA OUTPUT}
output=${2:?missing output}
[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid release source SHA' >&2; exit 1; }
: "${GITHUB_REPOSITORY:?missing repository}"
: "${GITHUB_RUN_ID:?missing run id}"
: "${GITHUB_RUN_ATTEMPT:?missing run attempt}"
[[ $GITHUB_RUN_ID =~ ^[1-9][0-9]*$ && $GITHUB_RUN_ATTEMPT =~ ^[1-9][0-9]*$ ]] || { echo 'invalid run id' >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
ghcr=false
acr=false
owner=${GITHUB_REPOSITORY%%/*}

if ! gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/actions/workflows/release.yml/runs?head_sha=$source_sha&per_page=20" \
  >"$work/runs.json"; then
  echo 'release history is unreadable' >&2
  exit 1
fi
jq -e 'type=="array"' "$work/runs.json" >/dev/null
jq -r --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" --argjson current "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" '
  [.[].workflow_runs[]?
    | select(.head_sha==$sha and .head_repository.full_name==$repo
      and .path==".github/workflows/release.yml" and .event=="workflow_run"
      and (.status=="completed" or (.id==$current and $attempt>1)))]
  | .[] | .id as $id
  | range(1; (if $id==$current then $attempt else .run_attempt+1 end))
  | [$id, .] | @tsv
' "$work/runs.json" >"$work/prior.tsv"
while IFS=$'\t' read -r prior_id prior_attempt; do
  [[ -n $prior_id ]] || continue
  [[ $prior_id =~ ^[1-9][0-9]*$ && $prior_attempt =~ ^[1-9][0-9]*$ ]] || exit 1
  if ! gh api --paginate --slurp \
    "repos/$GITHUB_REPOSITORY/actions/runs/$prior_id/attempts/$prior_attempt/jobs?per_page=100" \
    >"$work/jobs.json"; then
    echo 'release job history is unreadable' >&2
    exit 1
  fi
  if jq -e --argjson run "$prior_id" --argjson attempt "$prior_attempt" '
    [.[].jobs[]? | select(.run_id==$run and .run_attempt==$attempt and .status=="completed"
      and .conclusion=="success" and .name=="Publish GHCR / build-and-push")] | length>0
  ' "$work/jobs.json" >/dev/null; then
    ghcr=true
  fi
done <"$work/prior.tsv"

# ACR retains its native workflow identity for the existing Build issuer.
gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/actions/workflows/acr-publish.yml/runs?head_sha=$source_sha&branch=main&status=success&per_page=100" >"$work/acr-runs.json"
jq -r --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" '
  [.[].workflow_runs[]? | select(.head_sha==$sha and .head_branch=="main"
    and .head_repository.full_name==$repo and .path==".github/workflows/acr-publish.yml"
    and (.event=="workflow_run" or .event=="workflow_dispatch")
    and .status=="completed" and .conclusion=="success")]
  | .[] | [.id,.run_attempt] | @tsv
' "$work/acr-runs.json" >"$work/acr-prior.tsv"
while IFS=$'\t' read -r prior_id prior_attempt; do
  [[ -n $prior_id ]] || continue
  [[ $prior_id =~ ^[1-9][0-9]*$ && $prior_attempt =~ ^[1-9][0-9]*$ ]] || exit 1
  gh api --paginate --slurp \
    "repos/$GITHUB_REPOSITORY/actions/runs/$prior_id/attempts/$prior_attempt/jobs?per_page=100" >"$work/acr-jobs.json"
  if jq -e --arg sha "$source_sha" --argjson run "$prior_id" --argjson attempt "$prior_attempt" '
    [.[].jobs[]? | select(.run_id==$run and .run_attempt==$attempt and .status=="completed"
      and .conclusion=="success" and (.name | startswith("Research products published ["))
      and (.name | endswith("] ("+$sha+")")))] | length>0
  ' "$work/acr-jobs.json" >/dev/null; then acr=true; fi
done <"$work/acr-prior.tsv"

lookup_ghcr() {
  local url=$1 status=0
  status=0
  gh api --paginate --slurp "$url" >"$work/versions.json" 2>"$work/versions.err" || status=$?
  if [[ $status != 0 ]]; then
    if grep -Eq 'HTTP 404|Not Found' "$work/versions.err"; then
      return 2
    fi
    echo 'GHCR package lookup failed' >&2
    cat "$work/versions.err" >&2
    return 1
  fi
  if jq -e --arg tag "sha-$source_sha" '
    [.[].[]?.metadata.container.tags[]? | select(. == $tag)] | length > 0
  ' "$work/versions.json" >/dev/null; then
    jq -r --arg tag "sha-$source_sha" '
      [.[].[]? | select(any(.metadata.container.tags[]?; . == $tag)) | .name] | first // empty
    ' "$work/versions.json" | sed "s/^/ghcr sha-${source_sha} digest=/" >&2
    return 0
  fi
  return 2
}

if [[ $ghcr == false ]]; then
  status=0
  lookup_ghcr "/users/$owner/packages/container/hft/versions?per_page=100" || status=$?
  if [[ $status == 0 ]]; then
    ghcr=true
  elif [[ $status == 2 ]]; then
    status=0
    lookup_ghcr "/orgs/$owner/packages/container/hft/versions?per_page=100" || status=$?
    if [[ $status == 0 ]]; then
      ghcr=true
    elif [[ $status != 2 ]]; then
      exit 1
    fi
  else
    exit 1
  fi
fi

# Registry manifests alone do not prove that signed research release evidence
# was published. Only the successful product completion marker admits ACR reuse.

printf 'ghcr=%s\nacr=%s\n' "$ghcr" "$acr" >"$output"
