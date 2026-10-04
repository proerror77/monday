#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export GITHUB_RUN_ATTEMPT=2 MONDAY_RELEASE_JOB_ID=7 MONDAY_BUILD_INPUTS_FILE="$work/inputs.json"
sha=1111111111111111111111111111111111111111
mkdir -p "$work/controller/research-bin"
bash "$root/.github/scripts/research-release-products.sh" binaries controller >"$work/binaries"
[[ $(wc -l <"$work/binaries") -eq 4 ]]
while IFS= read -r binary; do
  printf 'mock executable: %s\n' "$binary" >"$work/controller/research-bin/$binary"
  chmod 0755 "$work/controller/research-bin/$binary"
done <"$work/binaries"
bash "$root/.github/scripts/research-release-products.sh" recipes controller >"$work/recipes"
jq -s -e 'length==2 and any(.[]; .package=="alpha-harness") and any(.[]; .package=="hft-collector") and
  all(.[]; .package!="hft-backtest" and .package!="ploy-research" and .package!="hft-research-platform")' "$work/recipes" >/dev/null
locks=$(bash "$root/.github/scripts/research-workspace-locks.sh" "$root/rust_hft")
jq -n --arg h "$(printf a%.0s {1..64})" --argjson locks "$locks" \
  '{schema:"monday.compilation-inputs.v2",target:"x86_64-unknown-linux-gnu",profile:"release",compiler:$h,native:$h,flags:$h,profiles:$h,recipe:$h,locks:$locks}' >"$MONDAY_BUILD_INPUTS_FILE"
bash "$root/.github/scripts/research-image-release-artifact.sh" create "$work/controller" "$sha" 42 "$root/rust_hft" 2 7 controller
ruby "$root/.github/scripts/research-release-bundle.rb" pack "$work/controller.tar" "$work/controller" controller
ruby "$root/.github/scripts/research-release-bundle.rb" unpack "$work/controller.tar" "$work/roundtrip" controller
bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$work/roundtrip" "$sha" 42 "$root/rust_hft" 2 7 controller
if ruby "$root/.github/scripts/research-release-bundle.rb" unpack "$work/controller.tar" "$work/wrong-product" paired >"$work/rejection" 2>&1; then exit 1; fi
if bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$work/roundtrip" "$sha" 42 "$root/rust_hft" 2 7 runner >"$work/rejection" 2>&1; then exit 1; fi
printf 'unadmitted control executable\n' >"$work/roundtrip/research-bin/researchctl"
chmod 0755 "$work/roundtrip/research-bin/researchctl"
if bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$work/roundtrip" "$sha" 42 "$root/rust_hft" 2 7 controller >"$work/rejection" 2>&1; then exit 1; fi
printf '%s\n' deployment/aliyun/research/scripts/campaign-job-watch.sh >"$work/controller-path"
GITHUB_REF=refs/heads/main bash "$root/.github/scripts/select-rust-ci-scope.sh" --event push --changed-files "$work/controller-path" \
  --metadata "$root/.github/scripts/fixtures/rust-ci-scope/metadata.fixture" --output "$work/plan"
grep -Fqx research_product=controller "$work/plan"
grep -Fq ',ploy/research-image-smoke,' "$work/plan"
printf 'PASS: controller-only release builds four actual executables; product, archive and unadmitted control bytes fail closed\n'
