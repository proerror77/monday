#!/usr/bin/env bash
# Preserve bytes only after this job's image tests. Publication authenticates
# the final job result, source and attempt through Actions.
set -euo pipefail
image=${1:?tested image required} product=${2:?image product required} job_name=${3:?producer job name required}
destination=${4:?empty bundle directory required}
[[ $product == hft-core || $product == binance-lob-archiver ]] || exit 2
: "${GITHUB_REPOSITORY:?}" "${GITHUB_SHA:?}" "${GITHUB_RUN_ID:?}" "${GITHUB_RUN_ATTEMPT:?}"
[[ $GITHUB_SHA =~ ^[0-9a-f]{40}$ && $GITHUB_RUN_ID =~ ^[1-9][0-9]*$ && $GITHUB_RUN_ATTEMPT =~ ^[1-9][0-9]*$ ]]
test ! -e "$destination"
job=$(gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" |
  jq -er --arg name "$job_name" --argjson run "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" '
    [.[].jobs[]|select(.name==$name and .run_id==$run and .run_attempt==$attempt)] |
    if length==1 then .[0].id else error("ambiguous image producer") end')
image_id=$(docker image inspect --format '{{.Id}}' "$image")
test "$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$image")" = linux/amd64
test "$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$image")" = "$GITHUB_SHA"
mkdir -p "$destination"
docker save "$image" --output "$destination/image.tar"
jq -n --arg source "$GITHUB_SHA" --arg name "$product" --arg run "$GITHUB_RUN_ID" --arg attempt "$GITHUB_RUN_ATTEMPT" \
  --argjson job "$job" --arg job_name "$job_name" --arg image_id "$image_id" \
  --arg archive "$(sha256sum "$destination/image.tar" | awk '{print $1}')" \
  '{schema_version:"monday.tested_image.v2",source_sha:$source,image:$name,run_id:$run,run_attempt:$attempt,
    job_id:$job,job_name:$job_name,platform:"linux/amd64",image_id:$image_id,archive_sha256:$archive}' >"$destination/image.json"
