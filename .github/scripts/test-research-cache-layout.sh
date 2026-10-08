#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
helper=$root/.github/scripts/research-cache-layout.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
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
# Refuse to traverse a cached symlink into another target or filesystem path.
profile="$work/repo/rust_hft/target/research-core/x86_64-unknown-linux-gnu/release"
rm -rf "$profile/deps"
ln -s "$work/repo/rust_hft/target/data-pipelines/x86_64-unknown-linux-gnu/release/deps" "$profile/deps"
if FIXTURE="$work" PATH="$work/bin:$PATH" bash "$work/repo/.github/scripts/research-cache-layout.sh" cleanup "$work/inputs"; then
  echo 'symlink cleanup escaped its target' >&2; exit 1
fi
printf 'PASS: all four disjoint targets, native bytes retained, local bytes removed, path/symlink escape rejected\n'
