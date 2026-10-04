#!/usr/bin/env bash
# All package owners come from Cargo; the registry lists workspace entrances.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
registry="$root/workspaces.json"
jq -e '.schema == "monday.cargo_workspaces.v1" and
  ([.workspaces[].id] | length == (unique | length)) and
  all(.workspaces[]; (.id | test("^[a-z]+$")) and
    (.manifest | test("^([a-z-]+/)+Cargo[.]toml$")))' "$registry" >/dev/null
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
while IFS=$'\t' read -r id manifest; do
  cargo metadata --manifest-path "$root/$manifest" --locked --no-deps --format-version 1 >"$work/$id.json"
  jq --arg id "$id" --arg manifest "$manifest" '
    . as $metadata | {
      id:$id, manifest:$manifest, workspace_root:.workspace_root,
      packages:[.packages[] | select(.id as $id | $metadata.workspace_members | index($id)) |
        . + {workspace_id:$id,workspace_manifest:$manifest}]
    }' "$work/$id.json" >"$work/$id-owned.json"
done < <(jq -r '.workspaces[] | [.id,.manifest] | @tsv' "$registry")
jq -se '
  {workspaces:map({id,manifest,workspace_root}),packages:[.[].packages[]]} |
  if ([.packages[].name] | length != (unique | length)) or
     ([.packages[].manifest_path] | length != (unique | length))
  then error("package belongs to multiple registered workspaces") else . end
' "$work/"*-owned.json
