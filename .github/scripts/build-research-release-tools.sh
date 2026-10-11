#!/usr/bin/env bash
# Independent, credential-free release tool producer. This is not an image product.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
: "${RUNNER_TEMP:?}" "${GITHUB_REPOSITORY:?}" "${GITHUB_RUN_ID:?}" "${GITHUB_RUN_ATTEMPT:?}"
test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
test "$(sed -n 's/^ID=//p' /etc/os-release)" = debian
test "$(sed -n 's/^VERSION_CODENAME=//p' /etc/os-release)" = bookworm
source_sha=$(bash .github/scripts/research-release-source-sha.sh)
git -c safe.directory="$root" diff --quiet HEAD -- .
workflow=${GITHUB_WORKFLOW_REF#*/.github/}
workflow=.github/${workflow%%@*}
[[ $workflow == .github/workflows/ploy-ci.yml || $workflow == .github/workflows/acr-publish.yml ]]
job=$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" | jq -ser \
  --argjson run "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" \
  '[.[].jobs[]? | select(.name=="Research release tools" and .run_id==$run and .run_attempt==$attempt)] | if length==1 then .[0].id else error("ambiguous tools producer") end')
builder=$(ruby -ryaml -e 'jobs=YAML.safe_load(File.read(ARGV[0])).fetch("jobs").values.select{|j|j["name"]=="Research release tools"}; abort unless jobs.length==1; puts jobs[0].fetch("container").fetch("image")' "$workflow")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
rustc -Vv >"$work/rustc"
cargo -V >"$work/cargo"
sysroot=$(rustc --print sysroot)
find "$sysroot/lib/rustlib/x86_64-unknown-linux-gnu/lib" -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum >"$work/sysroot"
dpkg-query -W -f='${Package}\t${Version}\t${Architecture}\n' | LC_ALL=C sort >"$work/native"
cat /etc/os-release >>"$work/native"
ldd --version >>"$work/native"
for name in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL \
  CC CXX AR LD CFLAGS CXXFLAGS LDFLAGS CARGO_BUILD_TARGET; do
  printf '%s=%s\n' "$name" "${!name-}" >>"$work/flags"
done
while IFS= read -r name; do printf '%s=%s\n' "$name" "${!name-}" >>"$work/flags"; done \
  < <(env | awk -F= '$1 ~ /^CARGO_(PROFILE_RELEASE_|TARGET_.*_(LINKER|RUSTFLAGS)$|BUILD_)/ {print $1}' | LC_ALL=C sort)
hash() { sha256sum "$1" | awk '{print $1}'; }
jq -n --arg builder "$builder" --rawfile rustc "$work/rustc" --rawfile cargo "$work/cargo" \
  --arg rustc_sha "$(hash "$(rustup which rustc)")" --arg cargo_sha "$(hash "$(rustup which cargo)")" \
  --arg sysroot_sha "$(hash "$work/sysroot")" --arg native "$(hash "$work/native")" --arg flags "$(hash "$work/flags")" \
  '{builder_image:$builder,compiler:{rustc_version:$rustc,cargo_version:$cargo,rustc_sha256:$rustc_sha,cargo_sha256:$cargo_sha,sysroot_sha256:$sysroot_sha},native_sha256:$native,flags_sha256:$flags}' >"$work/compiler.json"
export MONDAY_SOURCE_REVISION=$source_sha
export CARGO_TARGET_DIR="$root/rust_hft/research-core/platform/target/release-tools"
recipe=.github/scripts/research-release-tools.json
args=(--manifest-path "$root/$(jq -er .manifest "$recipe")" --locked --no-default-features --features "$(jq -er .features "$recipe")")
bins=()
while IFS= read -r binary; do bins+=(--bin "$binary"); done < <(jq -r '.binaries[]' "$recipe")
target=$(jq -er .target "$recipe")
(cd rust_hft
 cargo test "${args[@]}" --lib release_
 cargo test "${args[@]}" "${bins[@]}"
 cargo build "${args[@]}" --release --target "$target" "${bins[@]}")
git -c safe.directory="$root" diff --quiet HEAD -- .
release="$RUNNER_TEMP/research-release-tools"
test ! -e "$release"
mkdir -p "$release/release-tools"
while IFS= read -r binary; do
  source_binary="$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/$binary"
  readelf -h "$source_binary" | grep -Eq 'Class:.*ELF64'
  readelf -h "$source_binary" | grep -Eq 'Machine:.*Advanced Micro Devices X86-64'
  install -m 0755 "$source_binary" "$release/release-tools/$binary"
done < <(jq -r '.binaries[]' "$recipe")
test "$("$release/release-tools/research-release-publisher" --version)" = "research-release-publisher $source_sha"
ruby .github/scripts/research-release-tools-artifact.rb create "$release" "$source_sha" "$GITHUB_RUN_ID" "$GITHUB_RUN_ATTEMPT" "$job" "$workflow" "$work/compiler.json"
ruby .github/scripts/research-release-tools-artifact.rb verify "$release" "$source_sha" "$GITHUB_RUN_ID" "$GITHUB_RUN_ATTEMPT" "$job" "$workflow"
ruby .github/scripts/research-release-bundle.rb pack-tools "$RUNNER_TEMP/research-release-tools.tar" "$release"
