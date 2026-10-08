#!/usr/bin/env bash
# Verify Cargo's real doc discovery and preserve lib doctest=false eligibility.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=loop-nextest-doctests.sh
source "$root/.github/scripts/loop-nextest-doctests.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/research-core/docs/src" "$work/research-core/disabled/src" "$work/out"
cat >"$work/research-core/Cargo.toml" <<'TOML'
[workspace]
members=["docs", "disabled"]
resolver="2"
TOML
for package in docs disabled; do
  cat >"$work/research-core/$package/Cargo.toml" <<TOML
[package]
name="$package"
version="0.1.0"
edition="2021"
TOML
done
printf '\n[lib]\ndoctest=false\n' >>"$work/research-core/disabled/Cargo.toml"
cat >"$work/research-core/docs/src/lib.rs" <<'RUST'
/// ```
/// assert_eq!(2+2,4);
/// ```
///
///     assert_eq!(3+3,6);
///
/// ```ignore
/// compile_error!("ignored");
/// ```
pub fn docs() {}
#[doc=include_str!("included.md")]
pub fn included() {}
#[test]
fn unit_must_be_skipped() { panic!("already executed by nextest"); }
RUST
# shellcheck disable=SC2016 # Literal Rust markdown fences.
printf '```\nassert_eq!(4+4,8);\n```\n' >"$work/research-core/docs/src/included.md"
# shellcheck disable=SC2016 # Literal Rust markdown fences.
printf '/// ```\n/// compile_error!("disabled docs must stay disabled");\n/// ```\npub fn disabled() {}\n' >"$work/research-core/disabled/src/lib.rs"
printf '%s\n' '{"rust-suites":{"docs":{"testcases":{"unit_must_be_skipped":{}}}}}' >"$work/inventory.json"
export CARGO_TARGET_DIR="$work/target"
(cd "$work" && cargo generate-lockfile --manifest-path research-core/Cargo.toml)
cd "$work"
loop_nextest_run_doctests "$work/out" "$work/inventory.json" -p docs -p disabled
[[ $LOOP_DOC_LISTED == 3 && $LOOP_DOC_PASSED == 3 ]]
printf 'PASS: fenced/indented/included docs, ignored and disabled docs, exact unit skip\n'
