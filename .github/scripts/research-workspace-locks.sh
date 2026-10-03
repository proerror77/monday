#!/usr/bin/env bash
# Hash every owning lock, including shared protocol consumers' lock resolution.
set -euo pipefail
root=${1:?rust_hft directory required}
result='{}'
while IFS= read -r manifest; do
  [[ $manifest =~ ^([a-z-]+/)+Cargo\.toml$ ]] || exit 2
  lock=${manifest%Cargo.toml}Cargo.lock
  test -f "$root/$lock"
  digest=$(sha256sum "$root/$lock" | awk '{print $1}')
  result=$(jq -c --arg file "$lock" --arg digest "$digest" '. + {($file):$digest}' <<<"$result")
done < <(jq -r '.workspaces[].manifest' "$root/workspaces.json")
jq -S . <<<"$result"
