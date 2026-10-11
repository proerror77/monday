#!/usr/bin/env bash
# Authenticated same-source tools readback. No compilation, signing or cloud mutation.
set -euo pipefail
run=${1:?producer run required}
source_sha=${2:?source SHA required}
release=${3:?empty output directory required}
[[ $# == 3 ]] || exit 2
root=$(cd "$(dirname "$0")/../.." && pwd)
: "${GITHUB_REPOSITORY:?repository required}"
[[ $run =~ ^[1-9][0-9]*$ && $source_sha =~ ^[0-9a-f]{40}$ ]]
test "$(bash "$root/.github/scripts/research-release-source-sha.sh")" = "$source_sha"
git -c safe.directory="$root" -C "$root" diff --quiet HEAD -- .
test ! -e "$release"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run" >"$work/run.json"
jq -e --arg sha "$source_sha" --arg repo "$GITHUB_REPOSITORY" --argjson run "$run" '
  .id==$run and .head_sha==$sha and .head_branch=="main" and .head_repository.full_name==$repo and
  ((.path==".github/workflows/ploy-ci.yml" and .event=="push" and .status=="completed" and .conclusion=="success") or
   (.path==".github/workflows/acr-publish.yml" and .event=="workflow_dispatch" and (.status!="completed" or .conclusion=="success")))' "$work/run.json" >/dev/null
attempt=$(jq -er '.run_attempt' "$work/run.json")
[[ $attempt =~ ^[1-9][0-9]*$ ]]
path=$(jq -er '.path' "$work/run.json")
name='Research release tools'
gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$run/attempts/$attempt/jobs?per_page=100" | jq -s . >"$work/jobs.json"
job=$(jq -er --arg name "$name" --argjson run "$run" --argjson attempt "$attempt" '
  [.[].jobs[]?|select(.name==$name and .run_id==$run and .run_attempt==$attempt and .status=="completed" and .conclusion=="success")]
  | if length==1 then .[0].id else error("missing/ambiguous successful software producer") end' "$work/jobs.json")
[[ $job =~ ^[1-9][0-9]*$ ]]
gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$run/artifacts?per_page=100" | jq -s . >"$work/artifacts.json"
artifact=$(jq -er --arg name "research-release-tools-$source_sha" --arg sha "$source_sha" --argjson run "$run" '
  [.[].artifacts[]?|select(.name==$name)] | if length==1 and (.[0]|.expired==false and .workflow_run.id==$run and .workflow_run.head_sha==$sha and .size_in_bytes>0 and .size_in_bytes<=1073741824)
  then .[0].id else error("missing/expired/ambiguous release artifact") end' "$work/artifacts.json")
[[ $artifact =~ ^[1-9][0-9]*$ ]]
gh api "repos/$GITHUB_REPOSITORY/actions/artifacts/$artifact/zip" >"$work/release.zip"
unzip -Z -1 "$work/release.zip" | LC_ALL=C sort >"$work/entries"
printf '%s\n' research-release-tools.tar >"$work/expected"
diff -u "$work/expected" "$work/entries"
# ZIP transport changes modes to 0644. The fixed tar preserves executable modes.
unzip -p "$work/release.zip" research-release-tools.tar | head -c 1073774593 >"$work/research-release-tools.tar"
test "$(wc -c <"$work/research-release-tools.tar")" -le 1073774592
ruby "$root/.github/scripts/research-release-bundle.rb" unpack-tools "$work/research-release-tools.tar" "$work/verified-tools"
ruby "$root/.github/scripts/research-release-tools-artifact.rb" verify "$work/verified-tools" "$source_sha" "$run" "$attempt" "$job" "$path"
# Fail closed if the producer was rerun or changed while we downloaded bytes.
gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run" >"$work/reread.json"
jq -e --slurpfile before "$work/run.json" '.id==$before[0].id and .run_attempt==$before[0].run_attempt and .head_sha==$before[0].head_sha and .head_repository.full_name==$before[0].head_repository.full_name and .path==$before[0].path and .event==$before[0].event and (.status!="completed" or .conclusion=="success")' "$work/reread.json" >/dev/null
gh api "repos/$GITHUB_REPOSITORY/actions/jobs/$job" | jq -e --argjson run "$run" --argjson attempt "$attempt" '.run_id==$run and .run_attempt==$attempt and .status=="completed" and .conclusion=="success"' >/dev/null
test ! -e "$release"
mv "$work/verified-tools" "$release"
printf 'verified release tools run=%s attempt=%s job=%s source=%s\n' "$run" "$attempt" "$job" "$source_sha"
