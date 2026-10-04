#!/usr/bin/env bash
# Exercise image readback with fixed CLI fixtures; no registry or compute writes.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/producer" "$work/image"
export IMAGE_CONTENT="$work/image" IMAGE_SHA=1111111111111111111111111111111111111111
cat >"$work/bin/docker" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  image) printf '%s\n' "$IMAGE_SHA" ;;
  create) printf 'fixture-container\n' ;;
  cp) cp -R "$IMAGE_CONTENT/." "$3/" ;;
  rm) ;;
  *) exit 91 ;;
esac
MOCK
chmod 0755 "$work/bin/docker"
export PATH="$work/bin:$PATH"
for product in cex-runner prediction-runner; do
  rm -f "$work/image/"* "$work/producer/"*
  while IFS= read -r binary; do
    printf 'verified fixture: %s\n' "$binary" >"$work/producer/$binary"
    chmod 0755 "$work/producer/$binary"
    cp -p "$work/producer/$binary" "$work/image/$binary"
  done < <(bash "$root/.github/scripts/research-release-products.sh" binaries "$product")
  verifier="$root/.github/scripts/verify-research-product-image.sh"
  bash "$verifier" fixture "$IMAGE_SHA" "$work/producer" "$product"
  if [[ $product == cex-runner ]]; then foreign=monday-prediction-evaluator; own=hft-backtest; else foreign=alpha-harness; own=monday-prediction-research; fi
  printf 'foreign executable\n' >"$work/image/$foreign"
  if bash "$verifier" fixture "$IMAGE_SHA" "$work/producer" "$product" >"$work/rejection" 2>&1; then
    echo 'foreign domain image bytes accepted' >&2; exit 1
  fi
  rm "$work/image/$foreign"
  printf 'tampered\n' >>"$work/image/$own"
  if bash "$verifier" fixture "$IMAGE_SHA" "$work/producer" "$product" >"$work/rejection" 2>&1; then
    echo 'modified image bytes accepted' >&2; exit 1
  fi
  cp -p "$work/producer/$own" "$work/image/$own"
  if bash "$verifier" fixture 2222222222222222222222222222222222222222 "$work/producer" "$product" >"$work/rejection" 2>&1; then
    echo 'wrong image source accepted' >&2; exit 1
  fi
done
printf 'PASS: independent image readback rejects foreign executables, changed bytes and wrong source\n'
