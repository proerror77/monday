#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
helper=$root/.github/scripts/research-cache-layout.sh
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
trap 'rm -rf "$work"' EXIT
case ${1:-} in
  ''|--compatibility-key) ;;
  *) printf 'unknown cache test scope: %s\n' "$1" >&2; exit 2 ;;
esac

# These are cache identity fixtures, not native Cargo build evidence.
compat_repo="$work/compat-repo"
mkdir -p "$compat_repo/.github/scripts" "$compat_repo/rust_hft/alpha-harness/app"
cp "$helper" "$compat_repo/.github/scripts/"
cp "$root/.github/scripts/research-release-products.json" "$compat_repo/.github/scripts/"
mkdir -p "$compat_repo/.github/scripts/vendor"
cp -R "$root/.github/scripts/vendor/tomlrb" "$compat_repo/.github/scripts/vendor/"
cp "$root/rust_hft/"{Cargo.toml,workspaces.json} "$compat_repo/rust_hft/"
while IFS= read -r manifest; do
  mkdir -p "$compat_repo/rust_hft/${manifest%/Cargo.toml}"
  cp "$root/rust_hft/$manifest" "$compat_repo/rust_hft/$manifest"
  printf 'fixture lock\n' >"$compat_repo/rust_hft/${manifest%Cargo.toml}Cargo.lock"
done < <(jq -r '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
printf '[features]\ndefault=[]\n' >"$compat_repo/rust_hft/alpha-harness/app/Cargo.toml"
git -C "$compat_repo" init -q
git -C "$compat_repo" add rust_hft
compat_helper="$compat_repo/.github/scripts/research-cache-layout.sh"
fixture_inputs() {
  local profiles='{}' manifest digest
  cat "$compat_repo/rust_hft/Cargo.toml" "$compat_repo/rust_hft/workspaces.json" >"$work/profiles"
  while IFS= read -r manifest; do
    digest=$(sha256sum "$compat_repo/rust_hft/$manifest" | awk '{print $1}')
    profiles=$(jq -c --arg manifest "$manifest" --arg digest "$digest" '. + {($manifest):$digest}' <<<"$profiles")
    printf '%s\n' "$manifest" >>"$work/profiles"
    cat "$compat_repo/rust_hft/$manifest" >>"$work/profiles"
  done < <(jq -r '.workspaces[].manifest' "$compat_repo/rust_hft/workspaces.json")
  bash "$compat_helper" manifest-inputs >>"$work/profiles"
  digest=$(printf 'compiler/native fixture\n' | sha256sum | awk '{print $1}')
  jq -Sn --arg digest "$digest" --arg profiles "$(sha256sum "$work/profiles" | awk '{print $1}')" \
    --argjson owners "$profiles" \
    --argjson locks "$(bash "$root/.github/scripts/research-workspace-locks.sh" "$compat_repo/rust_hft")" \
    --argjson recipes "$(bash "$root/.github/scripts/research-release-products.sh" recipes "${1:-cex-runner,controller}" | jq -s .)" \
    '{schema:"monday.compilation-inputs.v3",target:"x86_64-unknown-linux-gnu",profile:"release",
      compiler:$digest,native:$digest,flags:$digest,profiles:$profiles,recipe:$digest,
      builder_image:("rust:fixture@sha256:"+$digest),recipes:$recipes,workspace_profiles:$owners,locks:$locks}'
}
fixture_inputs >"$work/full.json"
cp "$work/full.json" "$work/full-before.json"
bash "$compat_helper" compatibility-inputs "$work/full.json" >"$work/compat.json"
cmp "$work/full-before.json" "$work/full.json"
full_sha=$(sha256sum "$work/full.json" | awk '{print $1}')
compat_sha=$(sha256sum "$work/compat.json" | awk '{print $1}')
check_identity_change() {
  local expected=$1 actual
  [[ $(sha256sum "$work/changed.json" | awk '{print $1}') != "$full_sha" ]]
  bash "$compat_helper" compatibility-inputs "$work/changed.json" >"$work/changed-compat.json"
  actual=$(sha256sum "$work/changed-compat.json" | awk '{print $1}')
  if [[ $expected == reuse ]]; then [[ $actual == "$compat_sha" ]]; else [[ $actual != "$compat_sha" ]]; fi
}
# Local feature and lock changes remain in complete provenance, not the restore prefix.
cp "$compat_repo/rust_hft/alpha-harness/app/Cargo.toml" "$work/leaf-before"
printf '[features]\ndefault=["scientific"]\n' >"$compat_repo/rust_hft/alpha-harness/app/Cargo.toml"
fixture_inputs >"$work/changed.json"
check_identity_change reuse
cp "$work/leaf-before" "$compat_repo/rust_hft/alpha-harness/app/Cargo.toml"
printf 'new fixture lock\n' >"$compat_repo/rust_hft/research-core/Cargo.lock"
fixture_inputs >"$work/changed.json"
check_identity_change reuse
printf 'fixture lock\n' >"$compat_repo/rust_hft/research-core/Cargo.lock"

changed_digest=$(printf 'changed environment\n' | sha256sum | awk '{print $1}')
for field in compiler native flags recipe; do
  jq -S --arg field "$field" --arg digest "$changed_digest" '.[$field]=$digest' "$work/full.json" >"$work/changed.json"
  check_identity_change isolate
done
for update in '.target="aarch64-unknown-linux-gnu"' '.profile="research"' \
  '.builder_image=("rust:changed@sha256:"+.compiler)'; do
  jq -S "$update" "$work/full.json" >"$work/changed.json"
  check_identity_change isolate
done
for update in '.recipes[0].features="changed-feature"' '.recipes[0].package="changed-package"' \
  '.recipes[0].binaries=["changed-binary"]'; do
  jq -S "$update" "$work/full.json" >"$work/changed.json"
  check_identity_change reuse
done
# all -> subset changes exact coverage, never GitHub cache paths or compatibility.
bash "$compat_helper" cache-paths "$work/full.json" >"$work/cache-paths"
for product in all cex-runner controller prediction-runner; do
  fixture_inputs "$product" >"$work/changed.json"
  check_identity_change reuse
  bash "$compat_helper" cache-paths "$work/changed.json" >"$work/changed-paths"
  cmp "$work/cache-paths" "$work/changed-paths"
  cmp <(jq -Sc .recipes "$work/changed.json") <(bash "$root/.github/scripts/research-release-products.sh" recipes "$product" | jq -Scs .)
done
[[ $(grep -c '/rust_hft/target/' "$work/cache-paths") == 4 ]]
if grep -q '/cargo/bin\|/.cargo/bin' "$work/cache-paths"; then echo 'tool executables entered cache paths' >&2; exit 1; fi
printf 'fn source_only_fixture() {}\n' >"$compat_repo/rust_hft/alpha-harness/app/source.rs"
fixture_inputs >"$work/source-only.json"
cmp "$work/full.json" "$work/source-only.json"
# Dependency declarations change exact provenance without losing compatible bytes.
owner="$compat_repo/rust_hft/research-core/platform/Cargo.toml"
cp "$owner" "$work/owner-before"
ruby - "$owner" <<'RUBY'
path = ARGV.fetch(0)
text = File.read(path)
changed = text.sub('version = "=0.20.0", optional = true', 'version = "=0.20.1", optional = true')
abort 'publisher dependency fixture did not change' if changed == text
File.write(path, changed)
RUBY
fixture_inputs >"$work/changed.json"
check_identity_change reuse
cp "$work/owner-before" "$owner"
# Parse all profile tables, including quoted package and build overrides.
for table in 'profile.release.package.cache-fixture' '"profile"."release"."package"."cache-fixture"' 'profile.release.build-override'; do
  printf '\n[%s]\nopt-level=1\n' "$table" >>"$owner"
  fixture_inputs >"$work/changed.json"
  check_identity_change isolate
  cp "$work/owner-before" "$owner"
done
# Whitespace and inactive legacy workspace bytes do not change profiles.
for path in research-core/Cargo.toml Cargo.toml workspaces.json; do
  cp "$compat_repo/rust_hft/$path" "$work/root-before"
  printf '\n' >>"$compat_repo/rust_hft/$path"
  fixture_inputs >"$work/changed.json"
  check_identity_change reuse
  cp "$work/root-before" "$compat_repo/rust_hft/$path"
done
cp "$compat_repo/rust_hft/prediction-markets/Cargo.toml" "$work/unselected-before"
printf '\n[profile.release.build-override]\nopt-level=1\n' >>"$compat_repo/rust_hft/prediction-markets/Cargo.toml"
fixture_inputs >"$work/changed.json"
check_identity_change isolate
cp "$work/unselected-before" "$compat_repo/rust_hft/prediction-markets/Cargo.toml"
# Profiles outside the four cached research owners remain unrelated.
cp "$compat_repo/rust_hft/runtime/Cargo.toml" "$work/runtime-before"
printf '\n[profile.release.build-override]\nopt-level=1\n' >>"$compat_repo/rust_hft/runtime/Cargo.toml"
fixture_inputs >"$work/changed.json"
check_identity_change reuse
cp "$work/runtime-before" "$compat_repo/rust_hft/runtime/Cargo.toml"
cp "$compat_repo/rust_hft/workspaces.json" "$work/registry-before"
jq '(.workspaces[]|select(.manifest=="research-core/platform/Cargo.toml")|.id)="changed-control"' "$work/registry-before" >"$compat_repo/rust_hft/workspaces.json"
fixture_inputs >"$work/changed.json"
check_identity_change isolate
cp "$work/registry-before" "$compat_repo/rust_hft/workspaces.json"
printf '\n[profile.release\n' >>"$owner"
fixture_inputs >"$work/invalid.json"
if bash "$compat_helper" compatibility-inputs "$work/invalid.json" >"$work/rejected" 2>/dev/null; then
  printf 'invalid owner TOML accepted\n' >&2; exit 1
fi
[[ ! -s $work/rejected ]]
cp "$work/owner-before" "$owner"
for profile in '[profile.unsupported]' $'[profile.unsupported]\ninherits="missing"' \
  $'[profile.first]\ninherits="second"\n[profile.second]\ninherits="first"'; do
  printf '\n%s\n' "$profile" >>"$owner"
  fixture_inputs >"$work/invalid.json"
  if bash "$compat_helper" compatibility-inputs "$work/invalid.json" >"$work/rejected" 2>/dev/null; then
    echo 'unadmitted profile accepted' >&2; exit 1
  fi
  [[ ! -s $work/rejected ]]
  cp "$work/owner-before" "$owner"
done
for field in schema target profile compiler native flags profiles recipe builder_image recipes workspace_profiles locks; do
  jq --arg field "$field" 'del(.[$field])' "$work/full.json" >"$work/invalid.json"
  if bash "$compat_helper" compatibility-inputs "$work/invalid.json" >"$work/rejected" 2>/dev/null; then
    printf 'missing compatibility input accepted: %s\n' "$field" >&2; exit 1
  fi
  [[ ! -s $work/rejected ]]
done
for update in '.compiler=""' '.native="invalid"' '.recipes=[]' '.recipes[0].manifest="../outside/Cargo.toml"' \
  '.recipes[0].manifest="runtime/Cargo.toml"' '.workspace_profiles={}' '.locks={}' '.builder_image="rust:unpinned"'; do
  jq "$update" "$work/full.json" >"$work/invalid.json"
  if bash "$compat_helper" compatibility-inputs "$work/invalid.json" >"$work/rejected" 2>/dev/null; then
    printf 'malformed compatibility input accepted: %s\n' "$update" >&2; exit 1
  fi
  [[ ! -s $work/rejected ]]
done
catalog="$compat_repo/.github/scripts/research-release-products.json"
cp "$catalog" "$work/catalog-before"
jq '.recipes[0].manifest="runtime/Cargo.toml"' "$work/catalog-before" >"$catalog"
for command in compatibility-inputs cache-paths; do
  if bash "$compat_helper" "$command" "$work/full.json" >"$work/rejected" 2>/dev/null; then
    echo 'unknown catalog cache owner accepted' >&2; exit 1
  fi
  [[ ! -s $work/rejected ]]
done
cp "$work/catalog-before" "$catalog"
mv "$compat_repo/.github/scripts/vendor/tomlrb/lib" "$compat_repo/.github/scripts/vendor/tomlrb/missing-lib"
if bash "$compat_helper" compatibility-inputs "$work/full.json" >"$work/rejected" 2>/dev/null; then
  echo 'missing Cargo TOML parser accepted' >&2; exit 1
fi
[[ ! -s $work/rejected ]]
mv "$compat_repo/.github/scripts/vendor/tomlrb/missing-lib" "$compat_repo/.github/scripts/vendor/tomlrb/lib"
# Keep the full digest output and use the separate key only for dependency restore.
grep -Fq "printf 'cache_sha256=%s" "$root/.github/scripts/capture-research-build-inputs.sh"
grep -Fq 'dependency_cache_sha256=%s' "$root/.github/scripts/capture-research-build-inputs.sh"
ruby -ryaml - "$root" <<'RUBY'
root = ARGV.fetch(0)
restores = %w[ploy-ci acr-publish].map do |workflow|
  jobs = YAML.safe_load(File.read("#{root}/.github/workflows/#{workflow}.yml")).fetch('jobs')
  steps = jobs.values.find { |job| ['Research image binaries', 'Research release binaries'].include?(job['name']) }.fetch('steps')
  restore = steps.find { |step| step['id'] == 'research-cache' }
  save = steps.find { |step| step['name'] == 'Save trusted research dependency cache' }
  cleanup = steps.find { |step| step['name'] == 'Retain only dependency compilation bytes for trusted cache saves' }
  abort 'restore action can save uncleaned bytes' unless restore.fetch('uses').start_with?('actions/cache/restore@')
  abort 'cache save precedes dependency cleanup' unless steps.index(cleanup) < steps.index(save)
  abort 'save is not success/main bound' unless save.fetch('if').include?("github.ref == 'refs/heads/main' && success()")
  abort 'save can replace an exact cache' unless save.fetch('if').include?("cache-hit != 'true'")
  abort 'restore/save paths drifted' unless restore.dig('with', 'path') == save.dig('with', 'path')
  abort 'save key differs from attempted exact key' unless save.dig('with', 'key') == '${{ steps.research-cache.outputs.cache-primary-key }}'
  key = restore.dig('with', 'key')
  prefix = restore.dig('with', 'restore-keys')
  abort 'exact cache key lacks complete v3 provenance' unless key == prefix + '${{ steps.build-inputs.outputs.cache_sha256 }}'
  abort 'compatibility prefix is absent' unless prefix.include?('steps.build-inputs.outputs.dependency_cache_sha256')
  install = steps.find { |step| step['name'] == 'Install build dependencies' }.fetch('run')
  abort 'native container lacks Ruby or zstd tools' unless install.include?('ruby binutils zstd')
  restore.fetch('with')
end
abort 'producer cache namespaces drifted' unless restores.uniq.length == 1
RUBY
printf 'PASS: dependency compatibility identity, provenance preservation and fail-closed input contracts\n'
[[ ${1:-} != --compatibility-key ]] || exit 0

bash "$root/.github/scripts/research-release-products.sh" recipes all | jq -s '{recipes:.}' >"$work/inputs"
bash "$helper" workspaces "$work/inputs" >"$work/layout"
ruby -rpathname - "$root" "$work/layout" <<'RUBY'
root, file = ARGV
pairs = File.readlines(file).grep(/ -> /).map { |line| line.strip.split(' -> ') }
abort 'expected four recipe workspaces' unless pairs.length == 4
paths = pairs.map { |source, target| Pathname.new("#{root}/#{source}/#{target}").cleanpath.to_s }
abort 'shared/nested target could be cleaned twice' unless paths.uniq == paths && paths.none? { |p| paths.any? { |q| p != q && p.start_with?("#{q}/") } }
RUBY
if bash "$helper" target-dir ../outside/Cargo.toml; then echo 'escaping target accepted' >&2; exit 1; fi
mkdir -p "$work/repo/.github/scripts" "$work/repo/rust_hft" "$work/bin"
cp "$helper" "$work/repo/.github/scripts/"
cp "$root/.github/scripts/research-release-products.json" "$work/repo/.github/scripts/"
cp "$root/rust_hft/workspaces.json" "$work/repo/rust_hft/"
cat >"$work/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ " $* " == *" --no-deps "* ]]; then extra=false; else extra=true; fi
printf '%s\n' "$*" >>"$FIXTURE/metadata-calls"
jq -n --arg root "$FIXTURE/repo" --argjson extra "$extra" '{packages:([
  {name:"local-code",manifest_path:($root+"/rust_hft/member/Cargo.toml"),targets:[{name:"local_code"}]},
  {name:"hft-data",manifest_path:($root+"/rust_hft/data-pipelines/core/Cargo.toml"),targets:[{name:"data"}]}
] + if $extra then [
  {name:"hidden-local",manifest_path:($root+"/vendor/hidden-local/Cargo.toml"),targets:[{name:"hidden_local"}]},
  {name:"outside-local",source:null,manifest_path:"/external/local/Cargo.toml",targets:[{name:"outside_local"}]},
  {name:"data-encoding",source:"registry+https://example.invalid",manifest_path:"/external/registry/data-encoding/Cargo.toml",targets:[{name:"data_encoding"}]},
  {name:"libduckdb-sys",source:"registry+https://example.invalid",manifest_path:"/external/registry/libduckdb-sys/Cargo.toml",targets:[{name:"libduckdb_sys"}]},
  {name:"local-code",source:"registry+https://example.invalid",manifest_path:"/external/registry/local-code/Cargo.toml",targets:[{name:"local_code"}]}
] else [] end)}'
MOCK
chmod +x "$work/bin/cargo"
for id in data-pipelines prediction-markets research-core research-core--platform; do
  profile="$work/repo/rust_hft/target/$id/x86_64-unknown-linux-gnu/release"
  mkdir -p "$profile/"{build,.fingerprint,deps,incremental}
  mkdir -p "$work/repo/rust_hft/target/$id/tmp" "$work/repo/rust_hft/target/$id/x86_64-unknown-linux-gnu/tmp"
  printf 'local temporary object\n' >"$work/repo/rust_hft/target/$id/tmp/local.o"
  printf 'unknown bytes\n' >"$work/repo/rust_hft/target/$id/x86_64-unknown-linux-gnu/tmp/local.o"
  mkdir -p "$profile/build/libduckdb-sys-native/out" "$profile/build/local-code-hash/out"
  printf 'native object\n' >"$profile/build/libduckdb-sys-native/out/native.o"
  printf 'external build-script executable\n' >"$profile/build/libduckdb-sys-native/build-script-build"
  printf 'external native codegen executable\n' >"$profile/build/libduckdb-sys-native/out/native-codegen"
  chmod +x "$profile/build/libduckdb-sys-native/build-script-build" "$profile/build/libduckdb-sys-native/out/native-codegen"
  printf 'local object\n' >"$profile/build/local-code-hash/out/local.o"
  printf 'local\n' >"$profile/deps/liblocal_code-hash.rlib"
  printf 'hidden local\n' >"$profile/deps/libhidden_local-hash.rlib"
  printf 'outside local\n' >"$profile/deps/liboutside_local-hash.rlib"
  printf 'deleted local\n' >"$profile/deps/libdeleted_local-hash.rlib"
  mkdir -p "$profile/build/deleted-local-hash/out" "$profile/.fingerprint/deleted-local-hash" "$profile/examples"
  printf 'unknown object\n' >"$profile/build/deleted-local-hash/out/native.o"
  printf 'unknown fingerprint\n' >"$profile/.fingerprint/deleted-local-hash/lib-deleted_local"
  printf 'local example executable\n' >"$profile/examples/local"
  printf 'external dependency test executable\n' >"$profile/deps/data_encoding-hash"
  chmod +x "$profile/deps/data_encoding-hash"
  printf 'incremental local\n' >"$profile/incremental/local.o"
  printf 'local fingerprint\n' >"$profile/.fingerprint/local-code-hash"
  printf 'native\n' >"$profile/deps/liblibduckdb_sys-hash.rlib"
  printf 'local executable\n' >"$profile/local-code"
  # Local data and hft-data artifacts must be removed without deleting data-encoding.
  mkdir -p "$profile/build/hft-data-hash/out" "$profile/.fingerprint/hft-data-hash" \
    "$profile/.fingerprint/data-encoding-hash" "$profile/build/data-encoding-hash/out"
  printf 'local data object\n' >"$profile/build/hft-data-hash/out/local.o"
  printf 'local data fingerprint\n' >"$profile/.fingerprint/hft-data-hash/lib-data"
  for artifact in data-hash.d libdata-hash.rlib libdata-hash.rmeta libdata-hash.so hft-data-hash; do
    printf 'local data\n' >"$profile/deps/$artifact"
  done
  printf 'external dependency fingerprint\n' >"$profile/.fingerprint/data-encoding-hash/lib-data_encoding"
  printf 'external dependency bytes\n' >"$profile/deps/libdata_encoding-hash.rlib"
  printf 'external build output' >"$profile/build/data-encoding-hash/out/dependency.o"
done
bash "$root/.github/scripts/research-release-products.sh" recipes prediction-runner | jq -s '{recipes:.}' >"$work/subset-inputs"
FIXTURE="$work" PATH="$work/bin:$PATH" bash "$work/repo/.github/scripts/research-cache-layout.sh" cleanup "$work/subset-inputs"
[[ $(grep -vc -- '--no-deps' "$work/metadata-calls") == 8 ]]
grep -q -- '--features ploy-research/db' "$work/metadata-calls"
grep -q -- '--features hft-research-platform/publisher' "$work/metadata-calls"
if grep -q -- '--all-features' "$work/metadata-calls"; then echo 'cleanup used a feature union' >&2; exit 1; fi
for id in data-pipelines prediction-markets research-core research-core--platform; do
  profile="$work/repo/rust_hft/target/$id/x86_64-unknown-linux-gnu/release"
  [[ ! -e $work/repo/rust_hft/target/$id/tmp && ! -e $work/repo/rust_hft/target/$id/x86_64-unknown-linux-gnu/tmp ]]
  [[ -f $profile/build/libduckdb-sys-native/out/native.o && -f $profile/deps/liblibduckdb_sys-hash.rlib ]]
  [[ -x $profile/build/libduckdb-sys-native/build-script-build ]]
  [[ -x $profile/build/libduckdb-sys-native/out/native-codegen ]]
  [[ ! -e $profile/deps/libdeleted_local-hash.rlib && ! -e $profile/build/deleted-local-hash && ! -e $profile/.fingerprint/deleted-local-hash ]]
  [[ ! -e $profile/examples && ! -e $profile/deps/data_encoding-hash ]]
  [[ ! -e $profile/deps/libhidden_local-hash.rlib && ! -e $profile/build/local-code-hash && ! -e $profile/deps/liblocal_code-hash.rlib && ! -e $profile/.fingerprint/local-code-hash && ! -e $profile/local-code ]]
  [[ ! -e $profile/deps/liboutside_local-hash.rlib && ! -e $profile/incremental ]]
  [[ ! -e $profile/build/hft-data-hash && ! -e $profile/.fingerprint/hft-data-hash ]]
  for artifact in data-hash.d libdata-hash.rlib libdata-hash.rmeta libdata-hash.so hft-data-hash; do
    [[ ! -e $profile/deps/$artifact ]]
  done
  [[ $(cat "$profile/.fingerprint/data-encoding-hash/lib-data_encoding") == 'external dependency fingerprint' ]]
  [[ $(cat "$profile/deps/libdata_encoding-hash.rlib") == 'external dependency bytes' ]]
  [[ $(cat "$profile/build/data-encoding-hash/out/dependency.o") == 'external build output' ]]
done
printf 'PASS: subset cleanup covers all four cached targets, removes local/unknown bytes and executables, retains external build scripts/native objects/codegen tools\n'
# A local shared/vendor manifest outside the recipe roots changes cache identity
# even when every owning Cargo.lock and recipe remains unchanged.
mkdir -p "$work/repo/rust_hft/shared/cex-input" "$work/repo/vendor/local"
printf 'default=[]\n' >"$work/repo/rust_hft/shared/cex-input/Cargo.toml"
printf 'default=[]\n' >"$work/repo/vendor/local/Cargo.toml"
git -C "$work/repo" init -q
git -C "$work/repo" add rust_hft/shared/cex-input/Cargo.toml vendor/local/Cargo.toml
before=$(bash "$work/repo/.github/scripts/research-cache-layout.sh" manifest-inputs)
printf 'default=["native-feature"]\n' >"$work/repo/rust_hft/shared/cex-input/Cargo.toml"
after=$(bash "$work/repo/.github/scripts/research-cache-layout.sh" manifest-inputs)
[[ $before != "$after" ]]
jq -e 'has("rust_hft/shared/cex-input/Cargo.toml") and has("vendor/local/Cargo.toml")' <<<"$after" >/dev/null
# Match the container checkout ownership boundary without changing host owners.
foreign=$(GIT_TEST_ASSUME_DIFFERENT_OWNER=1 bash "$work/repo/.github/scripts/research-cache-layout.sh" manifest-inputs)
[[ $foreign == "$after" ]]
cat >"$work/bin/git" <<'MOCK'
#!/usr/bin/env bash
exit 73
MOCK
chmod +x "$work/bin/git"
if PATH="$work/bin:$PATH" bash "$work/repo/.github/scripts/research-cache-layout.sh" manifest-inputs >"$work/rejected-inputs"; then
  echo 'Git failure was silently accepted' >&2; exit 1
fi
[[ ! -s $work/rejected-inputs ]]
# Refuse to traverse a cached symlink into another target or filesystem path.
profile="$work/repo/rust_hft/target/research-core/x86_64-unknown-linux-gnu/release"
rm -rf "$profile/deps"
ln -s "$work/repo/rust_hft/target/data-pipelines/x86_64-unknown-linux-gnu/release/deps" "$profile/deps"
if FIXTURE="$work" PATH="$work/bin:$PATH" bash "$work/repo/.github/scripts/research-cache-layout.sh" cleanup "$work/inputs"; then
  echo 'symlink cleanup escaped its target' >&2; exit 1
fi
printf 'PASS: all four disjoint targets, native bytes retained, local bytes removed, path/symlink escape rejected\n'
