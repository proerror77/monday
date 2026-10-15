#!/usr/bin/env bash
# Promote only bytes from the successful exact-source main image producer.
set -euo pipefail
source_sha=${1:?} destination=${2:?} mode=${3:-required}
product=${4:-hft-core}
case "$product" in
  hft-core) workflow=security-enabled.yml; job_name="Container Security - Image Scan (hft-core)" ;;
  binance-lob-archiver) workflow=ci.yml; job_name="Production Image And Kubernetes Contract" ;;
  *) exit 2 ;;
esac
[[ $source_sha =~ ^[0-9a-f]{40}$ && ${GITHUB_REPOSITORY:-} == proerror77/monday ]] || exit 2
[[ $mode == required || $mode == optional ]] || exit 2
mkdir -p "$destination"
retry_work=$(mktemp -d "$destination/.github-read.XXXXXX")
trap 'rm -rf "$retry_work"' EXIT
# Retry only transient read failures. Commit no partial JSON or download bytes.
read_github() {
  local stage=$1 output=$2 attempt status
  shift 2
  for attempt in 1 2 3; do
    if "$@" >"$retry_work/response" 2>"$retry_work/error"; then
      mv "$retry_work/response" "$output"
      return 0
    else status=$?; fi
    printf 'GitHub read %s failed (attempt %s/3, exit %s).\n' "$stage" "$attempt" "$status" >&2
    cat "$retry_work/error" >&2
    if (( attempt == 3 )) || ! grep -Eq 'HTTP (500|502|503|504)([^0-9]|$)' "$retry_work/error"; then
      # Only complete, authenticated absence may return the optional rebuild code.
      return 1
    fi
    sleep "$attempt"
  done
}
download_bundle() {
  local partial
  partial=$(mktemp -d "$retry_work/bundle.XXXXXX")
  gh run download "$run_id" --repo "$GITHUB_REPOSITORY" --name "$name" --dir "$partial" || return $?
  mv "$partial" "$destination/bundle"
}
missing() { echo 'No successful exact-source tested image artifact is available.' >&2; [[ $mode != optional ]] || exit 75; exit 1; }
read_github workflow-runs "$destination/runs.json" gh api "repos/$GITHUB_REPOSITORY/actions/workflows/$workflow/runs?head_sha=$source_sha&event=push&per_page=100"
run=$(jq -c --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" --arg path ".github/workflows/$workflow" '
  [.workflow_runs[] | select(.head_sha==$sha and .head_branch=="main" and .event=="push" and
    .path==$path and .head_repository.full_name==$repo)] | max_by(.id)
' "$destination/runs.json")
[[ $run != null ]] || missing
jq -e '.status=="completed" and .conclusion=="success" and (.id|type=="number" and .>0 and floor==.) and (.run_attempt|type=="number" and .>0 and floor==.)' <<<"$run" >/dev/null || { echo 'Latest image-producing workflow did not succeed' >&2; exit 1; }
run_id=$(jq -r .id <<<"$run")
attempt=$(jq -r .run_attempt <<<"$run")
printf 'Read tested %s image for source %s from run %s attempt %s.\n' "$product" "$source_sha" "$run_id" "$attempt"
read_github producer-jobs "$destination/jobs.json" gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$run_id/attempts/$attempt/jobs?per_page=100"
job=$(jq -er --arg name "$job_name" --argjson run "$run_id" --argjson attempt "$attempt" '
  [.[].jobs[]?|select(.name==$name and .run_id==$run and .run_attempt==$attempt and .status=="completed" and .conclusion=="success")] |
  if length==1 then .[0].id else error("missing/ambiguous successful image producer") end' "$destination/jobs.json")
name="tested-image-$product-$source_sha-$attempt"
read_github artifact-list "$destination/artifacts.json" gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$run_id/artifacts?per_page=100"
# A partial listing must not authorize the optional publisher to rebuild.
# Count every page before deciding whether the retained image is absent.
jq -e '
  type=="array" and length>0 and
  all(.[]; type=="object" and (.total_count|type=="number" and .>=0 and floor==.) and
    (.artifacts|type=="array")) and
  ([.[].total_count]|unique|length)==1 and
  ([.[].artifacts[]]|length)==.[0].total_count and
  all(.[].artifacts[]; type=="object" and (.id|type=="number" and .>0 and floor==.) and
    (.name|type=="string" and length>0) and (.expired|type=="boolean")) and
  ([.[].artifacts[].id]|unique|length)==.[0].total_count
' "$destination/artifacts.json" >/dev/null || { echo 'Incomplete or ambiguous tested image artifact listing' >&2; exit 1; }
count=$(jq --arg name "$name" '[.[].artifacts[]|select(.name==$name and .expired==false)]|length' "$destination/artifacts.json")
[[ $count != 0 ]] || missing
[[ $count == 1 ]] || { echo 'Ambiguous retained tested image artifact' >&2; exit 1; }
[[ ! -e $destination/bundle && ! -L $destination/bundle ]] || exit 1
read_github artifact-download "$retry_work/download-output" download_bundle
manifest="$destination/bundle/image.json"
[[ -f $manifest && ! -L $manifest && -f $destination/bundle/image.tar && ! -L $destination/bundle/image.tar ]] || exit 1
jq -e --arg sha "$source_sha" --arg run "$run_id" --arg attempt "$attempt" --arg image "$product" --argjson job "$job" --arg name "$job_name" '
  .schema_version=="monday.tested_image.v2" and .source_sha==$sha and .image==$image and
  .job_id==$job and .job_name==$name and .platform=="linux/amd64" and
  .run_id==$run and .run_attempt==$attempt and
  (.image_id|test("^sha256:[0-9a-f]{64}$")) and (.archive_sha256|test("^[0-9a-f]{64}$"))
' "$manifest" >/dev/null
[[ $(sha256sum "$destination/bundle/image.tar" | awk '{print $1}') == "$(jq -r .archive_sha256 "$manifest")" ]] || { echo 'tested image archive hash mismatch' >&2; exit 1; }
test "$(find "$destination/bundle" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" -eq 2
[[ $(wc -c <"$destination/bundle/image.json") -le 65536 && $(wc -c <"$destination/bundle/image.tar") -le 4294967296 ]]
image_id=$(jq -r .image_id "$manifest")
docker load --input "$destination/bundle/image.tar" >/dev/null
[[ $(docker image inspect --format '{{.Id}}' "$image_id") == "$image_id" ]]
[[ $(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$image_id") == "$source_sha" ]]
[[ $(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$image_id") == linux/amd64 ]]
read_github final-run "$destination/final-run.json" gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run_id"
jq -e --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" --argjson run "$run_id" --argjson attempt "$attempt" '
  .id==$run and .run_attempt==$attempt and .head_sha==$sha and .head_branch=="main" and
  .head_repository.full_name==$repo and .status=="completed" and .conclusion=="success"' "$destination/final-run.json" >/dev/null
printf '%s\n' "$image_id" >"$destination/image-id.txt"
printf 'Verified tested image from authenticated run %s attempt %s.\n' "$run_id" "$attempt"
