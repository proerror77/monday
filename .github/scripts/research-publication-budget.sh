#!/usr/bin/env bash
# Offline estimation. No credentials, cloud requests, or allocation renewal.
set -euo pipefail
mode=${1:?expected estimate or admit}
[[ $mode == estimate || $mode == admit ]] || exit 2
source_sha=${2:?exact committed source required}
products=${3:?canonical selected products required}
storage_hours=${4:?storage estimate horizon required}
output=${5:?output required}
[[ $source_sha =~ ^[0-9a-f]{40}$ ]]
[[ $storage_hours =~ ^[1-9][0-9]{0,3}$ ]] && ((storage_hours <= 8760))
root=$(cd "$(dirname "$0")/../.." && pwd)
script_dir=$root/.github/scripts
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
test "$(git -C "$root" rev-parse "$source_sha^{commit}")" = "$source_sha"
git -C "$root" archive --format=tar --output="$work/source.tar" "$source_sha"
source_bytes=$(wc -c <"$work/source.tar" | tr -d ' ')
((source_bytes > 0 && source_bytes <= 536870912))
source_digest=$(shasum -a 256 "$work/source.tar" | awk '{print $1}')
git -C "$root" show "$source_sha:.github/scripts/research-release-products.json" >"$work/catalog.json"
repository=${GITHUB_REPOSITORY:-proerror77/monday}
jq -e --arg mode estimate --argjson policy null --argjson run_id 0 --argjson run_number 0 --argjson attempt 0 --argjson now 0 \
  --arg repository "$repository" \
  --arg source_sha "$source_sha" --arg source_digest "$source_digest" --arg products "$products" \
  --argjson source_bytes "$source_bytes" --argjson storage_hours "$storage_hours" \
  -f "$script_dir/research-publication-budget.jq" "$work/catalog.json" >"$work/estimate.json"
if [[ $mode == admit ]]; then
  : "${MONDAY_RESEARCH_PUBLICATION_BUDGET:?one public budget approval required}"
  : "${MONDAY_RELEASE_POLICY_JSON:?public OSS price target required}"
  : "${GITHUB_RUN_ID:?publisher run required}" "${GITHUB_RUN_NUMBER:?publisher sequence required}" "${GITHUB_RUN_ATTEMPT:?publisher attempt required}"
  [[ $GITHUB_RUN_ID =~ ^[1-9][0-9]{0,15}$ && $GITHUB_RUN_NUMBER =~ ^[1-9][0-9]{0,15}$ && $GITHUB_RUN_ATTEMPT == 1 ]]
  [[ ${GITHUB_WORKFLOW_REF:-} == "$repository/.github/workflows/acr-publish.yml@refs/heads/main" ]]
  printf '%s' "$MONDAY_RELEASE_POLICY_JSON" | jq -es --arg products "$products" '
    if length==1 and (.[0]|type)=="object" then .[0] else error("one OSS price target required") end |
    .oss_by_product as $map | all($products|split(",")[];
      $map[.] | .region=="ap-northeast-1" and (.bucket|type)=="string" and
      (.bucket|test("^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$")) and
      .endpoint==("https://"+.bucket+".oss-ap-northeast-1.aliyuncs.com/"))
  ' >/dev/null
  printf '%s' "$MONDAY_RESEARCH_PUBLICATION_BUDGET" |
    jq -es 'if length==1 then .[0] else error("one budget policy required") end' >"$work/policy.json"
  jq -e --arg mode admit \
    --arg repository "$repository" --arg source_sha "$source_sha" --arg products "$products" \
    --arg source_digest "$source_digest" --argjson source_bytes "$source_bytes" --argjson storage_hours "$storage_hours" \
    --argjson run_id "$GITHUB_RUN_ID" --argjson run_number "$GITHUB_RUN_NUMBER" --argjson attempt "$GITHUB_RUN_ATTEMPT" --argjson now "$(date -u +%s)" \
    --argjson policy "$(cat "$work/policy.json")" \
    -f "$script_dir/research-publication-budget.jq" "$work/estimate.json" >"$work/admission.json"
  mv "$work/admission.json" "$work/estimate.json"
  : "${RUNNER_TEMP:?native budget directory required}"
  jq -e --arg mode native --argjson policy null --argjson run_id 0 --argjson run_number 0 --argjson attempt 0 --argjson now 0 \
    --arg repository "$repository" --arg source_sha "$source_sha" --arg products "$products" \
    --arg source_digest "$source_digest" --argjson source_bytes "$source_bytes" --argjson storage_hours "$storage_hours" \
    -f "$script_dir/research-publication-budget.jq" "$work/estimate.json" >"$work/native.json"
  cp "$work/native.json" "$RUNNER_TEMP/research-publication-native-budget.json"
fi
# Write only a complete estimate; callers cannot mistake a partial file for admission.
cp "$work/estimate.json" "$output"
