#!/usr/bin/env bash
# Read back each product and verify its programs start without network access.
set -euo pipefail
image=${1:?image required} source_sha=${2:?source required}
binaries=${3:?verified producer directory required} product=${4:?single product required}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
product=$(bash "$script_dir/research-release-products.sh" normalize "$product")
[[ $product != *,* && $source_sha =~ ^[0-9a-f]{40}$ ]] || exit 2
test "$(docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "$image")" = "$source_sha"
work=$(mktemp -d)
readback="$work/image"
mkdir "$readback"
container=
cleanup() {
  if [[ -s "$work/smoke.cid" ]]; then
    docker rm -f "$(cat "$work/smoke.cid")" >/dev/null 2>&1 || true
  fi
  if [[ -n $container ]]; then docker rm -f "$container" >/dev/null 2>&1 || true; fi
  rm -rf "$work"
}
trap cleanup EXIT
container=$(docker create "$image")
docker cp "$container:/usr/local/bin/." "$readback/"
expected=$(bash "$script_dir/research-release-products.sh" binaries "$product")
while IFS= read -r binary; do
  if grep -Fxq "$binary" <<<"$expected"; then
    test -f "$readback/$binary" && test ! -L "$readback/$binary" && test -x "$readback/$binary"
    cmp "$readback/$binary" "$binaries/$binary"
  else
    test ! -e "$readback/$binary" && test ! -L "$readback/$binary"
  fi
done < <(bash "$script_dir/research-release-products.sh" binaries all)
if [[ $product == controller ]]; then
  bash "$script_dir/verify-research-controller-image.sh" "$image" "$source_sha" "$binaries"
else
  bash "$script_dir/verify-research-runner-binaries.sh" "$readback" "$product"
  # Bound each offline startup and remove its container if the client times out.
  while IFS= read -r binary; do
    printf 'Research runtime smoke: product=%s binary=%s\n' "$product" "$binary"
    timeout --kill-after=5s 30s docker run --rm --network none \
      --cidfile "$work/smoke.cid" --entrypoint "/usr/local/bin/$binary" \
      "$image" --help >/dev/null
    rm -f "$work/smoke.cid"
  done <<< "$expected"
fi
