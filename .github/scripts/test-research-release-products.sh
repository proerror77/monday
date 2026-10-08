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
jq -s -e 'length==3 and all(.[]; .manifest=="prediction-markets/Cargo.toml") and any(.[]; .package=="ploy-research" and (.binaries|length)==3) and any(.[]; .package=="hft-prediction-research-worker" and .binaries==["monday-prediction-worker"]) and any(.[]; .package=="hft-prediction-research-operator" and .binaries==["monday-prediction-operator"])' "$work/prediction-recipes" >/dev/null
bash "$products" recipes cex-runner >"$work/cex-recipes"
jq -s -e 'length==4 and any(.[]; .package=="hft-cex-research-worker" and .binaries==["monday-cex-worker"]) and all(.[]; .package!="ploy-research" and .manifest!="prediction-markets/Cargo.toml")' "$work/cex-recipes" >/dev/null
[[ $(bash "$products" binaries cex-runner | wc -l) -eq 7 ]]
[[ $(bash "$products" binaries all | wc -l) -eq 13 ]]
[[ $(bash "$products" binaries cex-runner,controller | wc -l) -eq 8 ]]
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
[[ $(wc -l <"$work/binaries") -eq 5 ]]
while IFS= read -r binary; do
  printf 'mock executable: %s\n' "$binary" >"$work/controller/research-bin/$binary"
  chmod 0755 "$work/controller/research-bin/$binary"
done <"$work/binaries"
bash "$root/.github/scripts/research-release-products.sh" recipes controller >"$work/recipes"
jq -s -e 'length==3 and any(.[]; .package=="hft-research-platform" and .manifest=="research-core/platform/Cargo.toml" and .features=="publisher" and .binaries==["research-release-publisher"]) and any(.[]; .package=="alpha-harness") and any(.[]; .package=="hft-collector") and
  all(.[]; .package!="hft-backtest" and .package!="ploy-research")' "$work/recipes" >/dev/null
locks=$(bash "$root/.github/scripts/research-workspace-locks.sh" "$root/rust_hft")
workspace_profiles=$(ruby -rjson -rdigest -e 'root=ARGV[0]; puts JSON.generate(JSON.parse(File.read("#{root}/workspaces.json")).fetch("workspaces").to_h{|owner| manifest=owner.fetch("manifest"); [manifest,Digest::SHA256.file("#{root}/#{manifest}").hexdigest]})' "$root/rust_hft")
jq -n --arg h "$(printf a%.0s {1..64})" --argjson locks "$locks" --argjson workspace_profiles "$workspace_profiles" --argjson recipes "$(jq -s . "$work/recipes")" \
  '{schema:"monday.compilation-inputs.v3",target:"x86_64-unknown-linux-gnu",profile:"release",compiler:$h,native:$h,flags:$h,profiles:$h,recipe:$h,locks:$locks,builder_image:("builder@sha256:"+$h),recipes:$recipes,workspace_profiles:$workspace_profiles}' >"$MONDAY_BUILD_INPUTS_FILE"
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
  jq --argjson recipes "$(bash "$products" recipes "$selection" | jq -s .)" '.recipes=$recipes' "$MONDAY_BUILD_INPUTS_FILE" >"$work/updated-inputs.json"
  cp "$work/updated-inputs.json" "$MONDAY_BUILD_INPUTS_FILE"
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
# Exercise the real producer with a conflicting ambient target and stale host
# binaries. Only the target recorded by compiler inputs may reach the archive.
fixture="$work/producer"
mkdir -p "$fixture/.github/scripts" "$fixture/rust_hft" "$fixture/bin" "$fixture/output"
for script in research-cache-layout.sh build-research-release.sh build-research-recipes.sh research-release-source-sha.sh research-release-products.sh research-release-products.json research-workspace-locks.sh research-image-release-artifact.sh verify-research-runner-binaries.sh research-release-bundle.rb; do
  cp "$root/.github/scripts/$script" "$fixture/.github/scripts/$script"
done
cp "$root/rust_hft/workspaces.json" "$fixture/rust_hft/workspaces.json"
while IFS= read -r manifest; do
  directory=${manifest%/Cargo.toml}; mkdir -p "$fixture/rust_hft/$directory"
  cp "$root/rust_hft/$manifest" "$fixture/rust_hft/$manifest"
  cp "$root/rust_hft/$directory/Cargo.lock" "$fixture/rust_hft/$directory/Cargo.lock"
done < <(jq -r '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
printf '#!/usr/bin/env bash\nexit 0\n' >"$fixture/.github/scripts/verify-research-runtime-abi.sh"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "1111111111111111111111111111111111111111"\n' >"$fixture/bin/git"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" 7\n' >"$fixture/bin/gh"
cat >"$fixture/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ ${FAIL_RECIPE:-0} != 1 ]] || exit 37
target='' binaries=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) target=$2; shift ;;
    --bin) binaries+=("$2"); shift ;;
  esac
  shift
done
test "$target" = "$(jq -r .target "$MONDAY_BUILD_INPUTS_FILE")"
mkdir -p "$CARGO_TARGET_DIR/$target/release"
for binary in "${binaries[@]}"; do
  printf 'fresh target executable: %s\n' "$binary" >"$CARGO_TARGET_DIR/$target/release/$binary"
  printf '%s\n' '{"reason":"compiler-artifact","fresh":true}'
done
printf '%s\n' '{"reason":"build-finished","success":true}'
MOCK
chmod +x "$fixture/bin/"* "$fixture/.github/scripts/verify-research-runtime-abi.sh"
mkdir -p "$fixture/rust_hft/target/release"
while IFS= read -r binary; do
  printf 'stale host executable\n' >"$fixture/rust_hft/target/release/$binary"
done < <(bash "$products" binaries controller)
jq --argjson recipes "$(bash "$products" recipes controller | jq -s .)" '.recipes=$recipes' "$MONDAY_BUILD_INPUTS_FILE" >"$fixture/inputs.json"
MONDAY_RESEARCH_CACHE_PROBE=1 PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/output" GITHUB_REPOSITORY=fixture/monday GITHUB_RUN_ID=42 CARGO_BUILD_TARGET=aarch64-unknown-linux-gnu MONDAY_BUILD_INPUTS_FILE="$fixture/inputs.json" bash "$fixture/.github/scripts/build-research-release.sh" controller
while IFS= read -r binary; do
  test "$(cat "$fixture/output/research-release/research-bin/$binary")" = "fresh target executable: $binary"
done < <(bash "$products" binaries controller)
printf 'PASS: controller-only release builds five admitted executables including the ACK importer; product, archive and unadmitted control bytes fail closed\n'

# The opt-in probe must run all three controller recipes twice with one input identity.
jq -es 'length==6 and ([.[].phase]|sort)==["after-cache-lookup","after-cache-lookup","after-cache-lookup","warm-local","warm-local","warm-local"]
  and ([.[].compilation_inputs_sha256]|unique|length)==1
  and all(.[]; .source_sha=="1111111111111111111111111111111111111111" and .compiler_artifacts>0)' \
  "$fixture/output/research-recipe-probe/timings.jsonl" >/dev/null
# A compiler failure must block the producer before artifact creation.
mkdir -p "$fixture/failed-output"
if FAIL_RECIPE=1 PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/failed-output" GITHUB_REPOSITORY=fixture/monday \
  GITHUB_RUN_ID=42 MONDAY_BUILD_INPUTS_FILE="$fixture/inputs.json" bash "$fixture/.github/scripts/build-research-release.sh" controller; then
  echo 'compiler failure accepted by the producer' >&2; exit 1
fi
[[ ! -e $fixture/failed-output/research-image-release.tar ]]
jq '.recipes=[]' "$fixture/inputs.json" >"$fixture/wrong-inputs.json"
if PATH="$fixture/bin:$PATH" RUNNER_TEMP="$fixture/failed-output" MONDAY_SOURCE_REVISION="$sha" \
  MONDAY_BUILD_INPUTS_FILE="$fixture/wrong-inputs.json" bash "$fixture/.github/scripts/build-research-recipes.sh" controller after-cache-lookup; then
  echo 'wrong admitted recipes accepted by the probe' >&2; exit 1
fi
printf 'PASS: recipe probe preserves exact inputs, compiler failures and all selected recipes\n'
