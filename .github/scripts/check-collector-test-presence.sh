#!/usr/bin/env bash
set -euo pipefail
name=polymarket_raw::tests::trade_pagination_offset_limit_defers_market_without_failing_cycle
listing=$(cargo test --manifest-path data-pipelines/Cargo.toml -p hft-collector --features collector-binance --locked --lib "$name" -- --exact --list)
grep -Fxq "$name: test" <<<"$listing"
ignored=$(cargo test --manifest-path data-pipelines/Cargo.toml -p hft-collector --features collector-binance --locked --lib "$name" -- --exact --ignored --list)
! grep -Fxq "$name: test" <<<"$ignored"
# The owning full collector suite executes this exact non-ignored test once.
