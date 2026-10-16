#!/usr/bin/env bash
# Run the patched build script in each mode. Bundled native linking remains in
# the image producer and offline smoke, with their admitted compiler and ABI.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src"
cat >"$work/Cargo.toml" <<TOML
[package]
name = "monday-native-validation"
version = "0.0.0"
edition = "2024"
[workspace]
[features]
bundled = ["libduckdb-sys/bundled"]
download = ["libduckdb-sys/download"]
[dependencies]
libduckdb-sys = { path = "$root/rust_hft/native/libduckdb-sys" }
TOML
printf 'fn main() {}\n' >"$work/src/main.rs"
# Reuse the admitted dependency versions. Do not resolve a new latest baseline.
cp "$root/rust_hft/research-core/Cargo.lock" "$work/Cargo.lock"
export CARGO_TARGET_DIR="${MONDAY_NATIVE_TEST_TARGET:-$work/target}"
cargo check --manifest-path "$work/Cargo.toml"
if DUCKDB_DOWNLOAD_LIB=1 cargo check --manifest-path "$work/Cargo.toml" >"$work/rejected" 2>&1; then
  echo 'download without its feature was accepted' >&2; exit 1
fi
grep -q 'requires the libduckdb-sys/download feature' "$work/rejected"
# Explicit feature must compile the original downloader. Keep network fetching
# disabled here; the download itself is a separately reported integration check.
cargo check --manifest-path "$work/Cargo.toml" --features download
cargo tree --manifest-path "$work/Cargo.toml" --features bundled --edges build,normal >"$work/bundled-tree"
if grep -Eq 'reqwest|rustls|zip v' "$work/bundled-tree"; then
  echo 'bundled native dependency still includes the downloader' >&2; exit 1
fi
# A fresh CI checkout uses stable mtimes, and refuses to normalize dirty source
# or an existing target. These fixtures do not establish Cargo artifact freshness.
fixture="$work/fixture"
mkdir -p "$fixture/.github/scripts" "$fixture/rust_hft/native/libduckdb-sys"
cp "$root/.github/scripts/prepare-research-native-source.sh" "$fixture/.github/scripts/"
printf 'native fixture' >"$fixture/rust_hft/native/libduckdb-sys/build.rs"
git -C "$fixture" init -q
git -C "$fixture" add .
git -C "$fixture" -c user.name=fixture -c user.email=fixture@example.invalid commit -qm fixture
GITHUB_ACTIONS=true bash "$fixture/.github/scripts/prepare-research-native-source.sh"
ruby -e 'abort "unstable mtime" unless File.mtime(ARGV[0]).to_i == 946684800' "$fixture/rust_hft/native/libduckdb-sys/build.rs"
mkdir -p "$fixture/rust_hft/target/research-core"
if GITHUB_ACTIONS=true bash "$fixture/.github/scripts/prepare-research-native-source.sh"; then exit 1; fi
rmdir "$fixture/rust_hft/target/research-core"
printf 'dirty' >>"$fixture/rust_hft/native/libduckdb-sys/build.rs"
if GITHUB_ACTIONS=true bash "$fixture/.github/scripts/prepare-research-native-source.sh"; then exit 1; fi
printf 'PASS: system and downloader compile, missing feature rejects, bundled graph excludes download dependencies, timestamp guards reject stale/dirty preparation\n'
