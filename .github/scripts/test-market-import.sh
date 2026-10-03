#!/usr/bin/env bash
# Existing CI disposable services only; generate a tiny synthetic raw triplet
# through the owning Rust protocol tests. Never copy business data into CI.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
[[ ${MONDAY_TEST_CH_ENDPOINT:?} == http://127.0.0.1:18123 ]] || exit 1
[[ ${MONDAY_TEST_DATABASE_URL:?} == postgres://fixture:*@127.0.0.1:5432/monday_foundation_test ]] || exit 1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export MONDAY_TEST_MARKET_COLUMNAR_DIR="$work/monday_market_columnar_test"
cd "$root/rust_hft"
cargo test --manifest-path data-pipelines/Cargo.toml -p hft-data --features columnar --locked standard_columnar_roundtrip -- --nocapture
cargo test --manifest-path data-pipelines/Cargo.toml -p hft-market-pipeline --features import --locked --test market_import -- --ignored --nocapture
