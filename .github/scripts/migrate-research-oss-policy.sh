#!/usr/bin/env bash
# Read-only public configuration migration. No credentials, API calls or grants.
set -euo pipefail
if [[ $# != 2 ]]; then
  printf 'Usage: %s EXISTING_PUBLIC_POLICY_JSON REVIEWED_EXISTING_ROLE_MAP_JSON > CANDIDATE_POLICY_JSON\n' "$0" >&2
  exit 2
fi
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
umask 077
jq -ne --slurpfile existing "$1" --slurpfile mapping "$2" '
  def text: type == "string" and length > 0;
  def entry:
    type == "object" and
    all([.bucket,.region,.endpoint,.role_arn,.oidc_provider_arn,.audience,.subject][]; text) and
    (.repository_id | type == "number" and . > 0 and floor == .) and
    (.owner_id | type == "number" and . > 0 and floor == .) and
    (.role_prefixes | type == "array" and length > 0 and . == (sort | unique) and
      all(.[]; type == "string" and test("^research/(sources/[0-9a-f]{40}|builds/[0-9a-f]{64})/$")));
  if ($existing | length) != 1 or ($existing[0] | type) != "object" or ($mapping | length) != 1 or
     ($mapping[0] | type != "object" or length == 0) then
    error("existing public policy and one reviewed existing-role map object required")
  elif any($mapping[0][]; entry | not) then
    error("role map requires all public OSS/OIDC fields, numeric immutable IDs and sorted exact source/Build prefixes; obtain values from existing configuration, never guess or expand scope")
  elif ($existing[0].oss_by_product != null and $existing[0].oss_by_product != {} and $existing[0].oss_by_product != $mapping[0]) then
    error("existing oss_by_product differs; reconcile its reviewed mappings explicitly rather than replacing them")
  else $existing[0] + {oss_by_product:$mapping[0]} end
' "$1" > "$work/candidate.json"
while IFS= read -r product; do
  jq -e --arg product "$product" -f "$root/.github/scripts/select-research-oss-policy.jq" "$work/candidate.json" >/dev/null
done < <(jq -r '.oss_by_product | keys[]' "$work/candidate.json")
cat "$work/candidate.json"
