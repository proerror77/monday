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
rustc -Vv >"$work/compiler"
cargo -V >>"$work/compiler"
sha256sum "$(rustup which rustc)" "$(rustup which cargo)" >>"$work/compiler"
sysroot=$(rustc --print sysroot)
find "$sysroot/lib/rustlib/x86_64-unknown-linux-gnu/lib" -type f -print0 | sort -z | xargs -0 sha256sum >>"$work/compiler"
dpkg-query -W -f='${Package}\t${Version}\t${Architecture}\n' | LC_ALL=C sort >"$work/native"
for command in clang cmake mold pkg-config protoc; do "$command" --version >>"$work/native"; done
printf '%s\n' "${RUSTFLAGS:-}" "${CARGO_ENCODED_RUSTFLAGS:-}" "${RUSTC_WRAPPER:-}" "${CARGO_PROFILE_RELEASE_DEBUG:-}" >"$work/flags"
for config in "$root/.cargo/config.toml" "$root/rust_hft/.cargo/config.toml" "$root/rust_hft/prediction-markets/.cargo/config.toml"; do
  if [[ -f $config ]]; then cat "$config" >>"$work/flags"; fi
done
cat "$root/rust_hft/Cargo.toml" "$root/rust_hft/prediction-markets/Cargo.toml" >"$work/profiles"
jq -S -n --arg compiler "$(sha256sum "$work/compiler" | awk '{print $1}')" \
  --arg native "$(sha256sum "$work/native" | awk '{print $1}')" \
  --arg flags "$(sha256sum "$work/flags" | awk '{print $1}')" \
  --arg profiles "$(sha256sum "$work/profiles" | awk '{print $1}')" \
  --arg recipe "$(sha256sum "$root/.github/scripts/build-research-release.sh" | awk '{print $1}')" \
  --arg root_lock "$(sha256sum "$root/rust_hft/Cargo.lock" | awk '{print $1}')" \
  --arg prediction_lock "$(sha256sum "$root/rust_hft/prediction-markets/Cargo.lock" | awk '{print $1}')" \
  '{schema:"monday.compilation-inputs.v1",target:"x86_64-unknown-linux-gnu",profile:"release",compiler:$compiler,native:$native,flags:$flags,profiles:$profiles,recipe:$recipe,locks:{root:$root_lock,prediction:$prediction_lock}}' >"$output"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then printf 'cache_sha256=%s\n' "$(sha256sum "$output" | awk '{print $1}')" >>"$GITHUB_OUTPUT"; fi
