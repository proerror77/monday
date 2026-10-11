#!/usr/bin/env bash
# Read-only public configuration migration. No credentials, API calls or grants.
set -euo pipefail
legacy_migration=false
if [[ ${1:-} == --migrate-exact-prefixes ]]; then
  legacy_migration=true
  shift
fi
if [[ $# != 2 ]]; then
  printf 'Usage: %s [--migrate-exact-prefixes] EXISTING_PUBLIC_POLICY_JSON REVIEWED_EXISTING_ROLE_MAP_JSON > CANDIDATE_POLICY_JSON\n' "$0" >&2
  exit 2
fi
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
umask 077
jq -ne --argjson legacy_migration "$legacy_migration" --slurpfile existing "$1" --slurpfile mapping "$2" '
  def text: type == "string" and length > 0;
  def fields($scope):
    type == "object" and
    (keys == (["bucket","region","endpoint","role_arn","oidc_provider_arn","audience","subject",$scope,"repository_id","owner_id"] | sort)) and
    all([.bucket,.region,.endpoint,.role_arn,.oidc_provider_arn,.audience,.subject][]; text) and
    (.bucket | test("^[a-z0-9-]{3,63}$")) and
    (.region | test("^[a-z0-9-]+$")) and
    (.endpoint == ("https://" + .bucket + ".oss-" + .region + ".aliyuncs.com/") or
      .endpoint == ("https://" + .bucket + ".oss-" + .region + "-internal.aliyuncs.com/")) and
    (.repository_id | type == "number" and . > 0 and floor == .) and
    (.owner_id | type == "number" and . > 0 and floor == .);
  def entry:
    fields("publication_namespaces") and
    .publication_namespaces == ["research/builds/","research/sources/"];
  def legacy_entry:
    fields("role_prefixes") and
    (.role_prefixes | type == "array" and length > 0 and . == (sort | unique) and
      all(.[]; type == "string" and test("^research/(sources/[0-9a-f]{40}|builds/[0-9a-f]{64})/$")));
  def approved_transition($old; $new):
    $new == $old or
    ($legacy_migration and ($old | legacy_entry) and
      $new == ($old | del(.role_prefixes) | .publication_namespaces = ["research/builds/","research/sources/"]));
  if ($existing | length) != 1 or ($existing[0] | type) != "object" or ($mapping | length) != 1 or
     ($mapping[0] | type != "object" or length == 0) then
    error("existing public policy and one reviewed existing-role map object required")
  elif any($mapping[0][]; entry | not) then
    error("role map requires all public OSS/OIDC fields, numeric immutable IDs and publication_namespaces exactly [research/builds/, research/sources/]; role_prefixes is obsolete. Review namespace base-role permissions independently; never guess cloud values")
  elif ($existing[0].oss_by_product != null and ($existing[0].oss_by_product | type) != "object") then
    error("existing oss_by_product must be an object")
  elif any(($existing[0].oss_by_product // {} | to_entries[]);
      . as $entry | approved_transition($entry.value; $mapping[0][$entry.key]) | not) then
    error("existing entries cannot be removed or changed; an exact-prefix to namespace transition requires --migrate-exact-prefixes and must preserve every identity field")
  else $existing[0] + {oss_by_product:$mapping[0]} end
' "$1" > "$work/candidate.json"
while IFS= read -r product; do
  jq -e --arg product "$product" -f "$root/.github/scripts/select-research-oss-policy.jq" "$work/candidate.json" >/dev/null
done < <(jq -r '.oss_by_product | keys[]' "$work/candidate.json")
cat "$work/candidate.json"
