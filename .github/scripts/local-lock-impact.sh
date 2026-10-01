#!/usr/bin/env bash
# Only canonical edits to existing local dependency lists may narrow checks.
# Unknown formats, external versions/checksums and new packages return 3.
set -euo pipefail
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
if [[ ${1:-} == --files ]]; then
  before=$2 after=$3
else
  base=${1:-} head=${2:-} path=${3:-}
  [[ $base =~ ^[0-9a-f]{40}$ && $head =~ ^[0-9a-f]{40}$ ]] || exit 3
  case "$path" in rust_hft/Cargo.lock|rust_hft/prediction-markets/Cargo.lock) ;; *) exit 3 ;; esac
  before=$tmp/before after=$tmp/after
  git show "$base:$path" >"$before" || exit 3
  git show "$head:$path" >"$after" || exit 3
fi
jq -ecn --rawfile before "$before" --rawfile after "$after" '
  def parse:
    split("[[package]]\n") as $parts |
    if ($parts|length)<2 or ($parts[0]|test("(?m)^version = [34]$"))==false then error("unknown lock format") else . end |
    [$parts[1:][] |
      . as $block |
      ([capture("(?m)^name = \"(?<value>[A-Za-z0-9_-]+)\"$")][0].value // error("missing name")) as $name |
      ([capture("(?m)^version = \"(?<value>[^\"]+)\"$")][0].value // error("missing version")) as $version |
      ([capture("(?m)^source = \"(?<value>[^\"]+)\"$")][0].value // "") as $source |
      {key:([$name,$version,$source]|tojson),name:$name,block:$block,
       stable:(if $source!="" then $block else $block|sub("(?m)^dependencies = \\[\n( \"[^\"\n]+\",\n)*\\]\n";"") end)}
    ] as $entries |
    if ($entries|map(.key)|unique|length)!=($entries|length) then error("duplicate package") else
      {header:$parts[0],entries:($entries|map({key:.key,value:.})|from_entries)} end;
  ($before|parse) as $a | ($after|parse) as $b |
  if $a.header!=$b.header or ($a.entries|keys)!=($b.entries|keys) then error("lock graph changed") else . end |
  [$a.entries|keys[] | . as $key | $a.entries[$key] as $old | $b.entries[$key] as $new |
    if $old.stable!=$new.stable then error("non-local lock change") else
      select($old.block!=$new.block) | $old.name end] | unique
' || { echo 'lock impact requires broad checks' >&2; exit 3; }
