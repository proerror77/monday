#!/bin/sh
set -e

# Environment defaults
CH_URL=${CH_URL:-https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443}
CH_DB=${CH_DB:-hft_db}
EXCHANGE=${EXCHANGE:-binance}
TOP_LIMIT=${TOP_LIMIT:-20}
BATCH_SIZE=${BATCH_SIZE:-1000}
FLUSH_MS=${FLUSH_MS:-2000}

export CLICKHOUSE_URL="$CH_URL"
export CLICKHOUSE_DB="$CH_DB"

echo "[entrypoint] exchange=$EXCHANGE ch_url=$CH_URL db=$CH_DB top=$TOP_LIMIT batch=$BATCH_SIZE flush_ms=$FLUSH_MS"

exec /usr/local/bin/hft-collector \
  multi \
  --exchange "$EXCHANGE" \
  --ch-url "$CH_URL" \
  --database "$CH_DB" \
  --top-limit "$TOP_LIMIT" \
  --batch-size "$BATCH_SIZE" \
  --flush-ms "$FLUSH_MS"
