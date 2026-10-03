#!/usr/bin/env bash
# Promote only the image produced by successful exact-source main security CI.
set -euo pipefail
source_sha=${1:?} destination=${2:?} mode=${3:-required}
[[ $source_sha =~ ^[0-9a-f]{40}$ && ${GITHUB_REPOSITORY:-} == proerror77/monday ]] || exit 2
[[ $mode == required || $mode == optional ]] || exit 2
mkdir -p "$destination"
missing() { echo 'No successful exact-source tested image artifact is available.' >&2; [[ $mode != optional ]] || exit 75; exit 1; }
gh api "repos/$GITHUB_REPOSITORY/actions/workflows/security-enabled.yml/runs?head_sha=$source_sha&event=push&per_page=100" >"$destination/runs.json"
run=$(jq -c --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" '
  [.workflow_runs[] | select(.head_sha==$sha and .head_branch=="main" and .event=="push" and
    .path==".github/workflows/security-enabled.yml" and .head_repository.full_name==$repo)] | max_by(.id)
' "$destination/runs.json")
[[ $run != null ]] || missing
jq -e '.status=="completed" and .conclusion=="success" and (.id|type=="number" and .>0 and floor==.) and (.run_attempt|type=="number" and .>0 and floor==.)' <<<"$run" >/dev/null || { echo 'Latest image-producing workflow did not succeed' >&2; exit 1; }
run_id=$(jq -r .id <<<"$run")
attempt=$(jq -r .run_attempt <<<"$run")
name="tested-image-hft-core-$source_sha-$attempt"
gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run_id/artifacts?per_page=100" >"$destination/artifacts.json"
count=$(jq --arg name "$name" '[.artifacts[]|select(.name==$name and .expired==false)]|length' "$destination/artifacts.json")
[[ $count == 1 ]] || missing
gh run download "$run_id" --repo "$GITHUB_REPOSITORY" --name "$name" --dir "$destination/bundle"
manifest="$destination/bundle/image.json"
[[ -f $manifest && ! -L $manifest && -f $destination/bundle/image.tar && ! -L $destination/bundle/image.tar ]] || exit 1
jq -e --arg sha "$source_sha" --arg run "$run_id" --arg attempt "$attempt" '
  .schema_version=="monday.tested_image.v1" and .source_sha==$sha and .image=="hft-core" and
  .run_id==$run and .run_attempt==$attempt and
  (.image_id|test("^sha256:[0-9a-f]{64}$")) and (.archive_sha256|test("^[0-9a-f]{64}$"))
' "$manifest" >/dev/null
[[ $(sha256sum "$destination/bundle/image.tar" | awk '{print $1}') == "$(jq -r .archive_sha256 "$manifest")" ]] || { echo 'tested image archive hash mismatch' >&2; exit 1; }
image_id=$(jq -r .image_id "$manifest")
docker load --input "$destination/bundle/image.tar" >/dev/null
[[ $(docker image inspect --format '{{.Id}}' "$image_id") == "$image_id" ]]
[[ $(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$image_id") == "$source_sha" ]]
printf '%s\n' "$image_id" >"$destination/image-id.txt"
printf 'Verified tested image from security run %s attempt %s.\n' "$run_id" "$attempt"
