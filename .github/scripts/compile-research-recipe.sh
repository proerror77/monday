#!/usr/bin/env bash
# Stable dependency compilation contract. Packaging and scheduling live outside it.
set -euo pipefail
recipe=${1:?recipe JSON required}
jobs=${2:?Cargo jobs required}
[[ $jobs =~ ^[1-9][0-9]*$ ]]
manifest=$(jq -er .manifest <<<"$recipe")
package=$(jq -er .package <<<"$recipe")
features=$(jq -er .features <<<"$recipe")
args=(build --manifest-path "$manifest" --target x86_64-unknown-linux-gnu --release --locked --jobs "$jobs" -p "$package")
[[ -z $features ]] || args+=(--features "$features")
while IFS= read -r binary; do args+=(--bin "$binary"); done < <(jq -er '.binaries[]' <<<"$recipe")
cargo "${args[@]}" --message-format json-render-diagnostics
