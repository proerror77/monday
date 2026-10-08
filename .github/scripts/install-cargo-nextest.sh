#!/usr/bin/env bash
# Install the pinned cargo-nextest binary used by the loop archive and shards.
set -euo pipefail

version=0.9.146
bindir=${CARGO_HOME:-$HOME/.cargo}/bin
mkdir -p "$bindir"
archive=$(mktemp)
trap 'rm -f "$archive"' EXIT
curl -fsSL -o "$archive" "https://get.nexte.st/${version}/linux"
printf '%s  %s\n' 682c21b777c333e96fd532e114d3a5a894e0729ab88d94c0a9f20f8419695428 "$archive" | sha256sum --check --status
tar -xzf "$archive" -C "$bindir"
installed=$(cargo nextest --version)
installed=${installed%%$'\n'*}
[[ $installed == "cargo-nextest ${version} "* ]] || {
  printf 'unexpected nextest version: %s\n' "$installed" >&2
  exit 1
}
