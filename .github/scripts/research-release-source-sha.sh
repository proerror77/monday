#!/usr/bin/env bash
# checkout and the container builder may have different UIDs. Trust this exact
# checkout for this single read; never change the user's/global Git policy.
set -euo pipefail
repo_root=$(cd "$(dirname "$0")/../.." && pwd)
source_sha=$(git -c safe.directory="$repo_root" -C "$repo_root" rev-parse HEAD)
[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid checked-out source identity' >&2; exit 1; }
printf '%s\n' "$source_sha"
