#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export GITHUB_RUN_ATTEMPT=2 MONDAY_RELEASE_JOB_ID=7 MONDAY_BUILD_INPUTS_FILE="$work/inputs.json"
sha=1111111111111111111111111111111111111111
# Domain-only recipes must stay independently buildable. Shared CEX/controller
# executables appear once when a producer builds more than one product.
products="$root/.github/scripts/research-release-products.sh"
bash "$products" recipes prediction-runner >"$work/prediction-recipes"
jq -s -e 'length==2 and all(.[]; .manifest=="prediction-markets/Cargo.toml") and any(.[]; .package=="ploy-research" and (.binaries|length)==3) and any(.[]; .package=="hft-prediction-research-worker" and .binaries==["monday-prediction-worker"])' "$work/prediction-recipes" >/dev/null
bash "$products" recipes cex-runner >"$work/cex-recipes"
jq -s -e 'length==3 and all(.[]; .package!="ploy-research" and .manifest!="prediction-markets/Cargo.toml")' "$work/cex-recipes" >/dev/null
[[ $(bash "$products" binaries cex-runner | wc -l) -eq 6 ]]
[[ $(bash "$products" binaries all | wc -l) -eq 10 ]]
[[ $(bash "$products" binaries cex-runner,controller | wc -l) -eq 6 ]]
[[ $(bash "$products" merge controller prediction-runner) == controller,prediction-runner ]]
for invalid in runner paired '' 'cex-runner,cex-runner' 'prediction-runner,unknown' 'all,controller'; do
  if bash "$products" normalize "$invalid" >"$work/rejection" 2>&1; then
    echo "invalid product selection accepted: $invalid" >&2; exit 1
  fi
done
# The real prebuilt Docker targets must copy precisely their catalog subset.
ruby -rjson - "$root" <<'RUBY'
root=ARGV.fetch(0)
catalog=JSON.parse(File.read(File.join(root,'.github/scripts/research-release-products.json'))).fetch('products')
{'cex-runner'=>'Dockerfile.research','prediction-runner'=>'Dockerfile.prediction-research'}.each do |product,file|
  text=File.read(File.join(root,'rust_hft/deployment/docker',file))
  prebuilt=text.split(/^FROM runtime-base AS prebuilt\s*$/,2).fetch(1).split(/^FROM /,2).first
  copied=prebuilt.scan(/^COPY --chmod=0755 research-bin\/(\S+) \/usr\/local\/bin\/(\S+)$/)
  abort "unexpected prebuilt COPY: #{product}" unless copied.all? { |source,target| source==target } && copied.map(&:first).sort==catalog.fetch(product).sort
  foreign=catalog.fetch(product=='cex-runner' ? 'prediction-runner' : 'cex-runner')
  abort "source builder imports another domain: #{product}" if foreign.any? { |binary| text.include?(binary) }
end
RUBY
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
if ruby "$root/.github/scripts/research-release-bundle.rb" unpack "$work/controller.tar" "$work/wrong-product" all >"$work/rejection" 2>&1; then exit 1; fi
if bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$work/roundtrip" "$sha" 42 "$root/rust_hft" 2 7 cex-runner >"$work/rejection" 2>&1; then exit 1; fi
printf 'unadmitted control executable\n' >"$work/roundtrip/research-bin/researchctl"
chmod 0755 "$work/roundtrip/research-bin/researchctl"
if bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$work/roundtrip" "$sha" 42 "$root/rust_hft" 2 7 controller >"$work/rejection" 2>&1; then exit 1; fi
# Round-trip every nonempty combination; wrong-domain and extra bytes remain
# rejected even when the producer contains more than one image's programs.
for selection in cex-runner prediction-runner cex-runner,controller cex-runner,prediction-runner controller,prediction-runner all; do
  directory="$work/$selection"
  mkdir -p "$directory/research-bin"
  while IFS= read -r binary; do
    printf 'mock executable: %s\n' "$binary" >"$directory/research-bin/$binary"
    chmod 0755 "$directory/research-bin/$binary"
  done < <(bash "$products" binaries "$selection")
  bash "$root/.github/scripts/research-image-release-artifact.sh" create "$directory" "$sha" 42 "$root/rust_hft" 2 7 "$selection"
  ruby "$root/.github/scripts/research-release-bundle.rb" pack "$directory.tar" "$directory" "$selection"
  ruby "$root/.github/scripts/research-release-bundle.rb" unpack "$directory.tar" "$directory-roundtrip" "$selection"
  bash "$root/.github/scripts/research-image-release-artifact.sh" verify "$directory-roundtrip" "$sha" 42 "$root/rust_hft" 2 7 "$selection"
done
printf '%s\n' deployment/aliyun/research/scripts/campaign-job-watch.sh >"$work/controller-path"
GITHUB_REF=refs/heads/main bash "$root/.github/scripts/select-rust-ci-scope.sh" --event push --changed-files "$work/controller-path" \
  --metadata "$root/.github/scripts/fixtures/rust-ci-scope/metadata.fixture" --output "$work/plan"
grep -Fqx research_product=controller "$work/plan"
grep -Fq ',ploy/research-image-smoke,' "$work/plan"
bash "$root/.github/scripts/test-research-product-image.sh"
printf 'PASS: controller-only release builds four actual executables; product, archive and unadmitted control bytes fail closed\n'
