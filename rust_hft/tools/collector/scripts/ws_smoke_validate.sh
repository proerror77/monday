#!/usr/bin/env bash
set -euo pipefail

# Quick WS validation without ClickHouse writes.
# It runs a short dry-run for a given exchange and symbols,
# enables COLLECTOR_VALIDATE to print per-bucket counts.
#
# Usage examples:
#   EXCHANGE=bitget SYMBOLS="BTCUSDT:SPOT,BTCUSDT:USDT-FUTURES" DURATION=15 ./ws_smoke_validate.sh
#   EXCHANGE=binance SYMBOLS="BTCUSDT,ETHUSDT" FEATURES="collector-binance" ./ws_smoke_validate.sh

EXCHANGE=${EXCHANGE:-bitget}
SYMBOLS=${SYMBOLS:-BTCUSDT:SPOT,BTCUSDT:USDT-FUTURES}
DURATION=${DURATION:-15}
FEATURES=${FEATURES:-}

cd "$(dirname "$0")/.."

case "$EXCHANGE" in
  bitget)
    FEATURES=${FEATURES:-collector-bitget}
    ;;
  binance)
    FEATURES=${FEATURES:-collector-binance,collector-binance-futures}
    ;;
  bybit)
    FEATURES=${FEATURES:-collector-bybit}
    ;;
  okx)
    FEATURES=${FEATURES:-collector-okx}
    ;;
  hyperliquid)
    FEATURES=${FEATURES:-collector-hyperliquid}
    ;;
  asterdex)
    FEATURES=${FEATURES:-collector-asterdex}
    ;;
  *) echo "Unsupported EXCHANGE=$EXCHANGE" >&2; exit 1;;
esac

echo "== Building with features: $FEATURES =="
cargo build --release --features "$FEATURES" >/dev/null

echo "== Running dry-run for $DURATION s: $EXCHANGE $SYMBOLS =="
export COLLECTOR_VALIDATE=1
RUST_LOG=${RUST_LOG:-info}
( RUST_LOG=$RUST_LOG cargo run --release --features "$FEATURES" -- \
    --exchanges "$EXCHANGE" \
    --symbols "$SYMBOLS" \
    --batch-size 200 \
    --flush-ms 1000 \
    --dry-run & pid=$!; sleep "$DURATION"; kill $pid >/dev/null 2>&1 || true; wait $pid >/dev/null 2>&1 || true ) 2>&1 | \
  rg -n "\[Validate|連接 WebSocket|發送訂閱消息|Dry-run|錯誤|關閉" -S || true

echo "== Done =="

