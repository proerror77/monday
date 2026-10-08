#!/usr/bin/env bash
# Decide whether this main SHA still needs GHCR and/or ACR publication.
# Pending or unsuccessful required checks exit 0 with both sides false.
set -euo pipefail
states=${1:?usage: decide-release-once.sh STATES PUBLISHED}
published=${2:?missing publication state}
output=${GITHUB_OUTPUT:-/dev/stdout}
source_sha=${SOURCE_SHA:?missing SOURCE_SHA}
is_current_main=${IS_CURRENT_MAIN:-true}
[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid release source SHA' >&2; exit 1; }
[[ $is_current_main == true || $is_current_main == false ]] || { echo 'invalid main identity flag' >&2; exit 1; }

publish_ghcr=false
publish_acr=false
reason=unpublished
if [[ $is_current_main != true ]]; then
  reason=stale-main
else
  pending=false
  rejected=false
  for check in monorepo prediction security; do
    state=$(sed -n "s/^${check}_conclusion=//p" "$states")
    case "$state" in
      success|skipped) ;;
      missing|queued|in_progress|waiting|pending|requested) pending=true ;;
      failure|neutral|cancelled|timed_out|action_required|stale|startup_failure) rejected=true ;;
      *) printf 'invalid check state for %s: %s\n' "$check" "${state:-empty}" >&2; exit 1 ;;
    esac
  done
  if [[ $rejected == true ]]; then
    reason=checks-not-green
  elif [[ $pending == true ]]; then
    reason=checks-pending
  else
    ghcr_published=$(sed -n 's/^ghcr=//p' "$published")
    acr_published=$(sed -n 's/^acr=//p' "$published")
    [[ $ghcr_published == true || $ghcr_published == false ]] || { echo 'invalid GHCR publication state' >&2; exit 1; }
    [[ $acr_published == true || $acr_published == false ]] || { echo 'invalid ACR publication state' >&2; exit 1; }
    [[ $ghcr_published == true ]] || publish_ghcr=true
    [[ $acr_published == true ]] || publish_acr=true
    if [[ $publish_ghcr == false && $publish_acr == false ]]; then
      reason=already-published
    else
      reason=publish
    fi
  fi
fi
printf 'release %s: %s ghcr=%s acr=%s\n' "$source_sha" "$reason" "$publish_ghcr" "$publish_acr" >&2
{
  printf 'publish_ghcr=%s\n' "$publish_ghcr"
  printf 'publish_acr=%s\n' "$publish_acr"
  printf 'source_sha=%s\n' "$source_sha"
  printf 'reason=%s\n' "$reason"
} >>"$output"
