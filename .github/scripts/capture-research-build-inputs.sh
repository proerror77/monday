#!/usr/bin/env bash
# Cache identity excludes Run/Attempt state. Final executable bytes are always
# built, hashed and smoke tested; this key is never execution admission.
set -euo pipefail
output=${1:?output JSON required}
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
# Native hosted Ubuntu bytes cannot be executed by the existing bookworm image.
# This admission runs before the expensive build and keys the actual native ABI.
test "$(sed -n 's/^ID=//p' /etc/os-release)" = debian
test "$(sed -n 's/^VERSION_CODENAME=//p' /etc/os-release)" = bookworm
# Read the literal pinned container of this authenticated producer workflow.
workflow_path=${GITHUB_WORKFLOW_REF#*/.github/}
workflow_path=.github/${workflow_path%%@*}
builder=$(ruby -ryaml -e 'w=YAML.safe_load(File.read(ARGV[0])); j=w.fetch("jobs").values.select{|j| ["Research image binaries","Research release binaries"].include?(j["name"])}; abort "ambiguous producer container" unless j.length==1; puts j[0].fetch("container").fetch("image")' "$root/$workflow_path")
[[ $builder =~ @sha256:[0-9a-f]{64}$ ]] || exit 2
rustc -Vv >"$work/compiler"
cargo -V >>"$work/compiler"
sha256sum "$(rustup which rustc)" "$(rustup which cargo)" >>"$work/compiler"
sysroot=$(rustc --print sysroot)
find "$sysroot/lib/rustlib/x86_64-unknown-linux-gnu/lib" -type f -print0 | sort -z | xargs -0 sha256sum >>"$work/compiler"
dpkg-query -W -f='${Package}\t${Version}\t${Architecture}\n' | LC_ALL=C sort >"$work/native"
cat /etc/os-release >>"$work/native"
ldd --version >>"$work/native"
for variable in CC CXX AR LD CFLAGS CXXFLAGS LDFLAGS RUSTC_BOOTSTRAP CARGO_BUILD_TARGET \
  LIBRARY_PATH LD_LIBRARY_PATH CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH \
  PKG_CONFIG PKG_CONFIG_PATH PKG_CONFIG_LIBDIR PKG_CONFIG_SYSROOT_DIR \
  OPENSSL_DIR OPENSSL_LIB_DIR OPENSSL_INCLUDE_DIR OPENSSL_STATIC \
  BINDGEN_EXTRA_CLANG_ARGS PROTOC PROTOC_INCLUDE CMAKE_TOOLCHAIN_FILE; do
  printf '%s=%s\n' "$variable" "${!variable-}" >>"$work/native"
done
for command in clang cmake mold pkg-config protoc zstd zstdmt unzstd; do "$command" --version >>"$work/native"; done
# Bind relevant Cargo overrides without capturing registry or service secrets.
for variable in RUSTC RUSTDOC RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL; do
  printf '%s=%s\n' "$variable" "${!variable-}" >>"$work/flags"
done
while IFS= read -r variable; do
  printf '%s=%s\n' "$variable" "${!variable-}" >>"$work/flags"
done < <(env | awk -F= '$1 ~ /^CARGO_(PROFILE_(RELEASE|RESEARCH)_|TARGET_.*_(LINKER|RUSTFLAGS)$|BUILD_)/ {print $1}' | LC_ALL=C sort)
for variable in RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER; do
  if [[ -n ${!variable-} ]]; then sha256sum "$(command -v "${!variable}")" >>"$work/compiler"; fi
done
printf '%s\n' "${RUSTFLAGS:-}" "${CARGO_ENCODED_RUSTFLAGS:-}" "${RUSTC_WRAPPER:-}" "${CARGO_PROFILE_RELEASE_DEBUG:-}" >>"$work/flags"
for config in "$root/.cargo/config.toml" "$root/rust_hft/.cargo/config.toml" "$root/rust_hft/prediction-markets/.cargo/config.toml"; do
  if [[ -f $config ]]; then cat "$config" >>"$work/flags"; fi
done
cat "$root/rust_hft/Cargo.toml" "$root/rust_hft/workspaces.json" >"$work/profiles"
while IFS= read -r manifest; do
  printf '%s\n' "$manifest" >>"$work/profiles"
  cat "$root/rust_hft/$manifest" >>"$work/profiles"
done < <(jq -r '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
# Native/dependency feature resolution can change via an excluded local
# package manifest without any lockfile change. Bind those manifests too.
bash "$root/.github/scripts/research-cache-layout.sh" manifest-inputs >>"$work/profiles"
workspace_profiles='{}'
while IFS= read -r manifest; do
  digest=$(sha256sum "$root/rust_hft/$manifest" | awk '{print $1}')
  workspace_profiles=$(jq -c --arg manifest "$manifest" --arg digest "$digest" '. + {($manifest):$digest}' <<<"$workspace_profiles")
done < <(jq -r '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
recipes=$(bash "$root/.github/scripts/research-release-products.sh" recipes "${2:-all}" | jq -s .)
locks=$("$root/.github/scripts/research-workspace-locks.sh" "$root/rust_hft")
cat "$root/.github/scripts/build-research-release.sh" \
  "$root/.github/scripts/build-research-recipes.sh" \
  "$root/.github/scripts/research-cache-layout.sh" \
  "$root/.github/scripts/cargo-cache-profile-inputs.py" \
  "$root/.github/scripts/research-release-products.sh" \
  "$root/.github/scripts/research-release-products.json" \
  "$root/.github/scripts/research-release-source-sha.sh" \
  "$root/.github/scripts/research-workspace-locks.sh" \
  "$root/.github/scripts/verify-research-runtime-abi.sh" \
  "$root/.github/scripts/verify-research-runner-binaries.sh" >"$work/recipe"
jq -S -n --arg compiler "$(sha256sum "$work/compiler" | awk '{print $1}')" \
  --arg native "$(sha256sum "$work/native" | awk '{print $1}')" \
  --arg flags "$(sha256sum "$work/flags" | awk '{print $1}')" \
  --arg profiles "$(sha256sum "$work/profiles" | awk '{print $1}')" \
  --arg recipe "$(sha256sum "$work/recipe" | awk '{print $1}')" \
  --arg builder_image "$builder" \
  --argjson recipes "$recipes" \
  --argjson workspace_profiles "$workspace_profiles" \
  --argjson locks "$locks" \
  '{schema:"monday.compilation-inputs.v3",target:"x86_64-unknown-linux-gnu",profile:"release",compiler:$compiler,native:$native,flags:$flags,profiles:$profiles,recipe:$recipe,locks:$locks,builder_image:$builder_image,recipes:$recipes,workspace_profiles:$workspace_profiles}'  >"$output"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then printf 'cache_sha256=%s\n' "$(sha256sum "$output" | awk '{print $1}')" >>"$GITHUB_OUTPUT"; fi
# This separate restore identity does not replace the complete v3 provenance.
bash "$root/.github/scripts/research-cache-layout.sh" compatibility-inputs "$output" >"$work/dependency-cache-inputs.json"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
  printf 'dependency_cache_sha256=%s\n' "$(sha256sum "$work/dependency-cache-inputs.json" | awk '{print $1}')" >>"$GITHUB_OUTPUT"
fi
