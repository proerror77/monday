#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
helper=$root/.github/scripts/research-cache-layout.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
case ${1:-} in
  ''|--compatibility-key) ;;
  *) printf 'unknown cache test scope: %s\n' "$1" >&2; exit 2 ;;
esac

# These are cache identity fixtures, not native Cargo build evidence.
compat_repo="$work/compat-repo"
mkdir -p "$compat_repo/.github/scripts" "$compat_repo/rust_hft/alpha-harness/app"
cp "$helper" "$compat_repo/.github/scripts/"
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
    --argjson recipes "$(bash "$root/.github/scripts/research-release-products.sh" recipes cex-runner,controller | jq -s .)" \
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
  '.builder_image=("rust:changed@sha256:"+.compiler)' '.recipes[0].features="changed-feature"'; do
  jq -S "$update" "$work/full.json" >"$work/changed.json"
  check_identity_change isolate
done
# Owner profile bytes, the legacy root manifest and the registry remain bound.
for path in research-core/Cargo.toml Cargo.toml workspaces.json; do
  cp "$compat_repo/rust_hft/$path" "$work/root-before"
  printf '\n' >>"$compat_repo/rust_hft/$path"
  fixture_inputs >"$work/changed.json"
  check_identity_change isolate
  cp "$work/root-before" "$compat_repo/rust_hft/$path"
done
for field in schema target profile compiler native flags profiles recipe builder_image recipes workspace_profiles locks; do
  jq --arg field "$field" 'del(.[$field])' "$work/full.json" >"$work/invalid.json"
  if bash "$compat_helper" compatibility-inputs "$work/invalid.json" >"$work/rejected" 2>/dev/null; then
    printf 'missing compatibility input accepted: %s\n' "$field" >&2; exit 1
  fi
  [[ ! -s $work/rejected ]]
done
for update in '.compiler=""' '.native="invalid"' '.recipes=[]' '.recipes[0].manifest="../outside/Cargo.toml"' \
  '.workspace_profiles={}' '.locks={}' '.builder_image="rust:unpinned"'; do
  jq "$update" "$work/full.json" >"$work/invalid.json"
  if bash "$compat_helper" compatibility-inputs "$work/invalid.json" >"$work/rejected" 2>/dev/null; then
    printf 'malformed compatibility input accepted: %s\n' "$update" >&2; exit 1
  fi
  [[ ! -s $work/rejected ]]
done
# Keep the full digest output and use the separate key only for dependency restore.
grep -Fq "printf 'cache_sha256=%s" "$root/.github/scripts/capture-research-build-inputs.sh"
grep -Fq 'dependency_cache_sha256=%s' "$root/.github/scripts/capture-research-build-inputs.sh"
grep -Fq 'key: research-dependencies-v1-linux-amd64-${{ steps.build-inputs.outputs.dependency_cache_sha256 }}' "$root/.github/workflows/ploy-ci.yml"
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
cp "$root/rust_hft/workspaces.json" "$work/repo/rust_hft/"
cat >"$work/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ " $* " == *" --no-deps "* ]]; then extra=false; else extra=true; fi
jq -n --arg root "$FIXTURE/repo" --argjson extra "$extra" '{packages:([{name:"local-code",manifest_path:($root+"/rust_hft/member/Cargo.toml"),targets:[{name:"local_code"}]}] + if $extra then [{name:"hidden-local",manifest_path:($root+"/vendor/hidden-local/Cargo.toml"),targets:[{name:"hidden_local"}]}] else [] end)}'
MOCK
chmod +x "$work/bin/cargo"
for id in data-pipelines prediction-markets research-core research-core--platform; do
  profile="$work/repo/rust_hft/target/$id/x86_64-unknown-linux-gnu/release"
  mkdir -p "$profile/"{build,.fingerprint,deps}
  mkdir -p "$profile/build/libduckdb-sys-native/out" "$profile/build/local-code-hash/out"
  printf 'native object\n' >"$profile/build/libduckdb-sys-native/out/native.o"
  printf 'local object\n' >"$profile/build/local-code-hash/out/local.o"
  printf 'local\n' >"$profile/deps/liblocal_code-hash.rlib"
  printf 'hidden local\n' >"$profile/deps/libhidden_local-hash.rlib"
  printf 'local fingerprint\n' >"$profile/.fingerprint/local-code-hash"
  printf 'native\n' >"$profile/deps/liblibduckdb_sys-hash.rlib"
  printf 'local executable\n' >"$profile/local-code"
done
FIXTURE="$work" PATH="$work/bin:$PATH" bash "$work/repo/.github/scripts/research-cache-layout.sh" cleanup "$work/inputs"
for id in data-pipelines prediction-markets research-core research-core--platform; do
  profile="$work/repo/rust_hft/target/$id/x86_64-unknown-linux-gnu/release"
  [[ -f $profile/build/libduckdb-sys-native/out/native.o && -f $profile/deps/liblibduckdb_sys-hash.rlib ]]
  [[ ! -e $profile/deps/libhidden_local-hash.rlib && ! -e $profile/build/local-code-hash && ! -e $profile/deps/liblocal_code-hash.rlib && ! -e $profile/.fingerprint/local-code-hash && ! -e $profile/local-code ]]
done
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
