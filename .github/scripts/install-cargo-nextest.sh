#!/usr/bin/env bash
# Install the pinned cargo-nextest binary used by the loop archive and shards.
set -euo pipefail

version=0.9.146
bindir=${CARGO_HOME:-$HOME/.cargo}/bin
mkdir -p "$bindir"
archive=$(mktemp)
trap 'rm -f "$archive"' EXIT
curl -fsSL -o "$archive" "https://get.nexte.st/${version}/linux"
tar -xzf "$archive" -C "$bindir"
installed=$(cargo nextest --version)
installed=${installed%%$'\n'*}
[[ $installed == "cargo-nextest ${version} "* ]] || {
  printf 'unexpected nextest version: %s\n' "$installed" >&2
  exit 1
}
