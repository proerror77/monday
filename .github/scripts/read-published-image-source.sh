#!/usr/bin/env bash
# Registry metadata only: no layer pull or local image build.
set -euo pipefail
head=${1:?exact source required}
[[ $head =~ ^[0-9a-f]{40}$ ]] || exit 2
metadata=$(docker buildx imagetools inspect ghcr.io/proerror77/hft:main --format '{{json .}}')
source=$(jq -er '
  if (.manifest.digest|test("^sha256:[0-9a-f]{64}$")) then .image else error("missing registry descriptor") end |
  (if has("config") then [.] else [.[]] end) |
  map(.config.Labels["org.opencontainers.image.revision"]) | unique |
  if length==1 and (.[0]|type=="string" and test("^[0-9a-f]{40}$")) then .[0]
  else error("published image revision is missing or ambiguous") end
' <<<"$metadata")
git merge-base --is-ancestor "$source" "$head" || { echo 'published image is outside the current source history' >&2; exit 1; }
printf '%s\n' "$source"
