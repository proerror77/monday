#!/usr/bin/env bash
set -euo pipefail

# Local Bitget USDT-Futures ingestion smoke test
# - Runs collector locally for a short duration and tails logs to a timestamped file
# - Verifies ClickHouse tables after run

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

LOG_DIR="logs"
mkdir -p "$LOG_DIR"
STAMP=$(date +%Y%m%d-%H%M%S)
LOG_FILE="$LOG_DIR/local-bitget-futures-$STAMP.log"

# ClickHouse
CH_URL="${CH_URL:-https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443}"
if [[ -f clickhouse_credentials.txt ]]; then
  CH_USER=$(awk -F': ' '/Username/{print $2}' clickhouse_credentials.txt | head -1)
  CH_PASS=$(awk -F': ' '/Password/{print $2}' clickhouse_credentials.txt | head -1)
else
  CH_USER="${CLICKHOUSE_USER:-default}"
  CH_PASS="${CLICKHOUSE_PASSWORD:-}"
fi
export CLICKHOUSE_USER="${CLICKHOUSE_USER:-$CH_USER}"
export CLICKHOUSE_PASSWORD="${CLICKHOUSE_PASSWORD:-$CH_PASS}"

# Symbols & channels (keep small to avoid overload)
SYMBOLS="${SYMBOLS:-BTCUSDT:USDT-FUTURES,ETHUSDT:USDT-FUTURES}"
CHANNELS="${BITGET_CHANNELS:-books,books1,publicTrade}"
DURATION="${DURATION:-90}"   # seconds

echo "[info] Building collector (release, host target)..." | tee -a "$LOG_FILE"
HOST_TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
(cd apps/collector && cargo clean && cargo build --release --target "$HOST_TRIPLE") | tee -a "$LOG_FILE"

BIN=""
for cand in \
  "apps/collector/target/$HOST_TRIPLE/release/hft-collector" \
  "target/$HOST_TRIPLE/release/hft-collector" \
  "target/release/hft-collector"; do
  if [[ -x "$cand" ]]; then BIN="$cand"; break; fi
done
if [[ -z "$BIN" ]]; then
  echo "[error] collector binary not found in target paths" | tee -a "$LOG_FILE"
  exit 1
fi
echo "[info] Using binary: $BIN" | tee -a "$LOG_FILE"

echo "[info] Starting local Bitget futures run for ${DURATION}s..." | tee -a "$LOG_FILE"
echo "[info] SYMBOLS=${SYMBOLS}" | tee -a "$LOG_FILE"
echo "[info] CHANNELS=${CHANNELS}" | tee -a "$LOG_FILE"

set +e
timeout "$DURATION" stdbuf -oL -eL \
  env \
  RUST_LOG="${RUST_LOG:-info}" \
  BITGET_MARKET="USDT-FUTURES" \
  BITGET_CHANNELS="$CHANNELS" \
  SYMBOLS="$SYMBOLS" \
  "$BIN" multi \
    --exchange bitget \
    --ch-url "$CH_URL" \
    --database hft_db \
    --batch-size 300 \
    --flush-ms 2000 \
    --depth-mode limited \
    --depth-levels 20 \
    --lob-mode snapshot \
  | tee -a "$LOG_FILE"
RC=$?
set -e

echo "[info] Collector exited with code $RC" | tee -a "$LOG_FILE"

echo "[info] Checking ClickHouse ingestion (last 5 minutes)..." | tee -a "$LOG_FILE"
chq() {
  curl -s --fail -u "$CLICKHOUSE_USER:$CLICKHOUSE_PASSWORD" --data-binary "$1" "$CH_URL" || echo "<ch_error>"
}

{
  echo "--- bitget_futures_trades (5m)";
  chq "SELECT countIf(local_ts >= toUInt64(toUnixTimestamp(now())*1000 - 300000)) AS rows_5m, max(local_ts) FROM hft_db.bitget_futures_trades";
  echo;
  echo "--- bitget_futures_orderbook (5m)";
  chq "SELECT countIf(local_ts >= toUInt64(toUnixTimestamp(now())*1000 - 300000)) AS rows_5m, max(local_ts) FROM hft_db.bitget_futures_orderbook";
  echo;
  echo "--- bitget_futures_l1 (5m)";
  chq "SELECT countIf(ts >= now() - INTERVAL 5 MINUTE) AS rows_5m, toString(max(ts)) FROM hft_db.bitget_futures_l1";
  echo;
  echo "--- bitget_futures_ticker (5m)";
  chq "SELECT countIf(ts >= now() - INTERVAL 5 MINUTE) AS rows_5m, toString(max(ts)) FROM hft_db.bitget_futures_ticker";
  echo;
  echo "--- bitget_futures_trades dedup (10m)";
  chq "SELECT count() AS total, uniqExact(symbol, trade_id) AS uniq, total-uniq AS dup FROM hft_db.bitget_futures_trades WHERE local_ts >= toUInt64(toUnixTimestamp(now())*1000 - 600000)";
  echo;
} | tee -a "$LOG_FILE"

echo "[done] Log saved: $LOG_FILE"
