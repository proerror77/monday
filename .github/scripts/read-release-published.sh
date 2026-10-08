#!/usr/bin/env bash
# Read whether this SHA was already published. Absence is false.
# An unreadable Release history or GHCR answer exits 1 so a second wakeup
# cannot publish without knowing the first result. ACR credential or HTTP 404
# answers are "not proven", because the ACR workflow repeats its own login.
set +x
set -euo pipefail
source_sha=${1:?usage: read-release-published.sh SOURCE_SHA OUTPUT}
output=${2:?missing output}
[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid release source SHA' >&2; exit 1; }
: "${GITHUB_REPOSITORY:?missing repository}"
: "${GITHUB_RUN_ID:?missing run id}"
[[ $GITHUB_RUN_ID =~ ^[1-9][0-9]*$ ]] || { echo 'invalid run id' >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
ghcr=false
acr=false
short=${source_sha:0:7}
owner=${GITHUB_REPOSITORY%%/*}

if ! gh api --paginate --slurp \
  "repos/$GITHUB_REPOSITORY/actions/workflows/release.yml/runs?head_sha=$source_sha&per_page=20" \
  >"$work/runs.json"; then
  echo 'release history is unreadable' >&2
  exit 1
fi
jq -e 'type=="array"' "$work/runs.json" >/dev/null
jq -r --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" --argjson current "$GITHUB_RUN_ID" '
  [.[].workflow_runs[]?
    | select(.head_sha==$sha and .head_repository.full_name==$repo
      and .path==".github/workflows/release.yml" and .event=="workflow_run"
      and .status=="completed" and .id != $current)]
  | .[] | [.id, .run_attempt] | @tsv
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
      and .conclusion=="success" and .name=="Publish GHCR")] | length>0
  ' "$work/jobs.json" >/dev/null; then
    ghcr=true
  fi
  if jq -e --argjson run "$prior_id" --argjson attempt "$prior_attempt" '
    [.[].jobs[]? | select(.run_id==$run and .run_attempt==$attempt and .status=="completed"
      and .conclusion=="success" and .name=="Publish ACR")] | length>0
  ' "$work/jobs.json" >/dev/null; then
    acr=true
  fi
done <"$work/prior.tsv"

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
  if jq -e --arg tag "sha-$short" '
    [.[].[]?.metadata.container.tags[]? | select(. == $tag)] | length > 0
  ' "$work/versions.json" >/dev/null; then
    jq -r --arg tag "sha-$short" '
      [.[].[]? | select(any(.metadata.container.tags[]?; . == $tag)) | .name] | first // empty
    ' "$work/versions.json" | sed "s/^/ghcr sha-${short} digest=/" >&2
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

if [[ $acr == false && -n ${ACR_REGISTRY:-} && -n ${ACR_USERNAME:-} && -n ${ACR_PASSWORD:-} ]]; then
  umask 077
  printf 'machine %s\nlogin %s\npassword %s\n' \
    "$ACR_REGISTRY" "$ACR_USERNAME" "$ACR_PASSWORD" >"$work/netrc"
  all_present=true
  for repository in research-runner prediction-research-runner campaign-cycle-controller; do
    status=0
    code=$(curl -sS --netrc-file "$work/netrc" -D "$work/headers" -o "$work/body" -w '%{http_code}' \
      -H 'Accept: application/vnd.docker.distribution.manifest.v2+json' \
      "https://${ACR_REGISTRY}/v2/wildcard0923/${repository}/manifests/${source_sha}") || status=$?
    if [[ $status != 0 ]]; then
      printf 'ACR manifest lookup failed for %s\n' "$repository" >&2
      all_present=false
      break
    fi
    digest=$(awk 'tolower($1)=="docker-content-digest:" {print $2}' "$work/headers" | tr -d '\r')
    if [[ $code == 200 && $digest =~ ^sha256:[0-9a-f]{64}$ ]]; then
      printf 'acr %s digest=%s\n' "$repository" "$digest" >&2
      continue
    fi
    if [[ $code == 404 || $code == 401 ]]; then
      all_present=false
      break
    fi
    printf 'ACR manifest lookup for %s returned HTTP %s\n' "$repository" "$code" >&2
    rm -f "$work/netrc"
    exit 1
  done
  rm -f "$work/netrc"
  [[ $all_present == true ]] && acr=true
fi

printf 'ghcr=%s\nacr=%s\n' "$ghcr" "$acr" >"$output"
