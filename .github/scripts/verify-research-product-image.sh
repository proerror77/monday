#!/usr/bin/env bash
# Read back immutable product contents, including foreign-domain exclusion.
set -euo pipefail
image=${1:?image required} source_sha=${2:?source required}
binaries=${3:?verified producer directory required} product=${4:?single product required}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
product=$(bash "$script_dir/research-release-products.sh" normalize "$product")
[[ $product != *,* && $source_sha =~ ^[0-9a-f]{40}$ ]] || exit 2
test "$(docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "$image")" = "$source_sha"
work=$(mktemp -d)
container=
trap '[[ -z $container ]] || docker rm -f "$container" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
container=$(docker create "$image")
docker cp "$container:/usr/local/bin/." "$work/"
expected=$(bash "$script_dir/research-release-products.sh" binaries "$product")
while IFS= read -r binary; do
  if grep -Fxq "$binary" <<<"$expected"; then
    test -f "$work/$binary" && test ! -L "$work/$binary" && test -x "$work/$binary"
    cmp "$work/$binary" "$binaries/$binary"
  else
    test ! -e "$work/$binary" && test ! -L "$work/$binary"
  fi
done < <(bash "$script_dir/research-release-products.sh" binaries all)
if [[ $product == controller ]]; then
  bash "$script_dir/verify-research-controller-image.sh" "$image" "$source_sha" "$binaries"
else
  bash "$script_dir/verify-research-runner-binaries.sh" "$work" "$product"
fi
