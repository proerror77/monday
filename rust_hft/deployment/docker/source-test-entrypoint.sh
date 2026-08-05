#!/usr/bin/env sh
set -eu

source_cargo_home=${CARGO_HOME:?}
export CARGO_HOME="${XDG_RUNTIME_DIR:-/tmp}/monday-source-test-cargo"
export CARGO_BUILD_JOBS=2
export CARGO_NET_OFFLINE=true
export CARGO_TARGET_DIR=/tmp/monday-source-test-target
mkdir -p "$CARGO_HOME"
cp -a "$source_cargo_home"/. "$CARGO_HOME"/

case "${1-}" in
  binance-bstocks-attestation)
    [ "$#" -eq 1 ] || { echo 'binance-bstocks-attestation accepts no extra arguments' >&2; exit 64; }
    exec cargo test --offline --locked -p hft-runtime --lib tokenized_security_requires_runtime_owned_attestation
    ;;
  bybit-spot)
    [ "$#" -eq 1 ] || { echo 'bybit-spot accepts no extra arguments' >&2; exit 64; }
    exec cargo test --offline --locked -p hft-execution-adapter-bybit --lib
    ;;
  *)
    echo 'usage: monday-source-test {binance-bstocks-attestation|bybit-spot}' >&2
    exit 64
    ;;
esac
