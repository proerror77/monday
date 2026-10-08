#!/usr/bin/env bash
# Record source identities after immutable image readback. This does not sign.
set -euo pipefail
source_sha=${1:?expected source SHA}
base_digest=${2:?expected base image config digest}
published_image=${3:?expected immutable published image}
output=${4:?expected output path}
: "${GITHUB_REPOSITORY:?missing repository}"
: "${GITHUB_RUN_ID:?missing run ID}"
: "${GITHUB_RUN_ATTEMPT:?missing run attempt}"
[[ $source_sha =~ ^[0-9a-f]{40}$ && $base_digest =~ ^sha256:[0-9a-f]{64}$ ]] || exit 1
[[ $published_image =~ @sha256:[0-9a-f]{64}$ ]] || exit 1
[[ $(git rev-parse HEAD) == "$source_sha" ]] || { echo 'record checkout differs from source' >&2; exit 1; }
tree_sha=$(git rev-parse 'HEAD^{tree}')
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/commits/$source_sha/pulls?per_page=100" >"$work/pulls.json"
jq -r --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" '
  [ .[][] | select(.merged_at != null and .merge_commit_sha==$sha
    and .base.ref=="main" and .base.repo.full_name==$repo) ]
  | unique_by(.number) | .[] | [.number,.html_url,.head.sha] | @tsv
' "$work/pulls.json" >"$work/pulls.tsv"
: >"$work/records.jsonl"
while IFS=$'\t' read -r number url head_sha; do
  [[ -n $number ]] || continue
  [[ $number =~ ^[1-9][0-9]*$ && $head_sha =~ ^[0-9a-f]{40}$ ]] || exit 1
  gh api "repos/$GITHUB_REPOSITORY/git/commits/$head_sha" >"$work/head.json"
  jq -e --arg sha "$head_sha" '.sha==$sha and (.tree.sha|test("^[0-9a-f]{40}$"))' "$work/head.json" >/dev/null
  jq -cn --argjson number "$number" --arg url "$url" --arg head "$head_sha" \
    --arg tree "$(jq -r '.tree.sha' "$work/head.json")" \
    '{number:$number,url:$url,head_sha:$head,tree_sha:$tree}' >>"$work/records.jsonl"
done <"$work/pulls.tsv"
jq -n --arg repo "$GITHUB_REPOSITORY" --arg sha "$source_sha" --arg tree "$tree_sha" \
  --arg base "$base_digest" --arg image "$published_image" \
  --arg run "$GITHUB_RUN_ID" --arg attempt "$GITHUB_RUN_ATTEMPT" \
  --slurpfile prs "$work/records.jsonl" \
  '{schema_version:"monday.release_record.v1",repository:$repo,pull_requests:$prs,
    main_commit:$sha,main_tree_sha:$tree,source_sha:$sha,source_tree_sha:$tree,
    base_image_digest:$base,base_image_digest_kind:"oci-config",
    published_image:$image,run_id:$run,run_attempt:$attempt}' >"$output"
