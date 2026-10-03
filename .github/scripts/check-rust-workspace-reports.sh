#!/usr/bin/env bash
# Preserve security/license coverage over every independent lock resolution.
set -euo pipefail
mode=${1:?audit or license required}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../rust_hft" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
result=0
while IFS=$'\t' read -r id manifest; do
  directory=${manifest%/Cargo.toml}
  case "$mode" in
    audit)
      if ! (cd "$root" && cargo audit --file "$directory/Cargo.lock" --json) >"$work/$id.json"; then result=1; fi
      jq -e '.vulnerabilities.list | arrays' "$work/$id.json" >/dev/null
      ;;
    license) (cd "$root/$directory" && cargo license --json) >"$work/$id.json" ;;
    *) exit 2 ;;
  esac
done < <(jq -r '.workspaces[] | [.id,.manifest] | @tsv' "$root/workspaces.json")
if [[ $mode == audit ]]; then
  jq -s '{workspaces:length,vulnerabilities:{
    list:([.[].vulnerabilities.list[]] | unique_by([.advisory.id,.package.name,.package.version])),
    found:any(.[];.vulnerabilities.found),count:([.[].vulnerabilities.list[]] | unique_by([.advisory.id,.package.name,.package.version]) | length)}}' "$work/"*.json
else
  jq -s 'add | unique_by([.name,.version,.license])' "$work/"*.json
fi
exit "$result"
