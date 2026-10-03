#!/usr/bin/env bash
# Authenticated cross-run software readback. No cloud execution or dispatch.
set -euo pipefail
run=${1:?producer run required}
source_sha=${2:?source SHA required}
release=${3:?empty output directory required}
product=${4:-paired}
root=$(cd "$(dirname "$0")/../.." && pwd)
: "${GITHUB_REPOSITORY:?repository required}"
[[ $run =~ ^[1-9][0-9]*$ && $source_sha =~ ^[0-9a-f]{40}$ ]]
test ! -e "$release"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run" >"$work/run.json"
jq -e --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" --argjson run "$run" '
  .id==$run and .head_sha==$sha and .head_branch=="main" and .head_repository.full_name==$repo and
  ((.path==".github/workflows/ploy-ci.yml" and .event=="push" and .status=="completed" and .conclusion=="success") or
   (.path==".github/workflows/acr-publish.yml" and .event=="workflow_dispatch"))' "$work/run.json" >/dev/null
attempt=$(jq -er '.run_attempt' "$work/run.json")
[[ $attempt =~ ^[1-9][0-9]*$ ]]
path=$(jq -er '.path' "$work/run.json")
if [[ $path == .github/workflows/ploy-ci.yml ]]; then name='Research image binaries'; else name='Research release binaries'; fi
gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$run/attempts/$attempt/jobs?per_page=100" >"$work/jobs.json"
job=$(jq -er --arg name "$name" --argjson run "$run" --argjson attempt "$attempt" '
  [.[].jobs[]?|select(.name==$name and .run_id==$run and .run_attempt==$attempt and .status=="completed" and .conclusion=="success")]
  | if length==1 then .[0].id else error("missing/ambiguous successful software producer") end' "$work/jobs.json")
[[ $job =~ ^[1-9][0-9]*$ ]]
gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$run/artifacts?per_page=100" >"$work/artifacts.json"
artifact=$(jq -er --arg name "research-image-release-$source_sha-$product" --arg sha "$source_sha" --argjson run "$run" '
  [.[].artifacts[]?|select(.name==$name)] | if length==1 and (.[0]|.expired==false and .workflow_run.id==$run and .workflow_run.head_sha==$sha and .size_in_bytes>0 and .size_in_bytes<=1073741824)
  then .[0].id else error("missing/expired/ambiguous release artifact") end' "$work/artifacts.json")
[[ $artifact =~ ^[1-9][0-9]*$ ]]
gh api "repos/$GITHUB_REPOSITORY/actions/artifacts/$artifact/zip" >"$work/release.zip"
unzip -Z -1 "$work/release.zip" | LC_ALL=C sort >"$work/entries"
printf '%s\n' research-image-release.tar >"$work/expected"
diff -u "$work/expected" "$work/entries"
# ZIP transport changes modes to 0644. The fixed tar preserves executable modes.
unzip -p "$work/release.zip" research-image-release.tar | head -c 1073774593 >"$work/research-image-release.tar"
test "$(wc -c <"$work/research-image-release.tar")" -le 1073774592
ruby "$root/.github/scripts/research-release-bundle.rb" unpack "$work/research-image-release.tar" "$release" "$product"
"$root/.github/scripts/research-image-release-artifact.sh" verify "$release" "$source_sha" "$run" "$root/rust_hft" "$attempt" "$job" "$product"
# Fail closed if the producer was rerun or changed while we downloaded bytes.
gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run" >"$work/reread.json"
jq -e --slurpfile before "$work/run.json" '.id==$before[0].id and .run_attempt==$before[0].run_attempt and .head_sha==$before[0].head_sha and .head_repository.full_name==$before[0].head_repository.full_name and .path==$before[0].path and .event==$before[0].event' "$work/reread.json" >/dev/null
gh api "repos/$GITHUB_REPOSITORY/actions/jobs/$job" | jq -e --argjson run "$run" --argjson attempt "$attempt" '.run_id==$run and .run_attempt==$attempt and .status=="completed" and .conclusion=="success"' >/dev/null
printf 'verified software run=%s attempt=%s job=%s source=%s\n' "$run" "$attempt" "$job" "$source_sha"
