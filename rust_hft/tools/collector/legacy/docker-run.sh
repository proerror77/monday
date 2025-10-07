#!/usr/bin/env bash
set -euo pipefail

IMAGE_NAME=${IMAGE_NAME:-hft-collector:latest}

# ClickHouse connection
CLICKHOUSE_URL=${CLICKHOUSE_URL:-https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443}
CLICKHOUSE_DB=${CLICKHOUSE_DB:-hft_db}
CLICKHOUSE_USER=${CLICKHOUSE_USER:-default}
CLICKHOUSE_PASSWORD=${CLICKHOUSE_PASSWORD:-}

# Collector args
EXCHANGE=${EXCHANGE:-binance}
TOP_LIMIT=${TOP_LIMIT:-20}
DEPTH_MODE=${DEPTH_MODE:-both}
DEPTH_LEVELS=${DEPTH_LEVELS:-20}
FLUSH_MS=${FLUSH_MS:-2000}
LOB_MODE=${LOB_MODE:-both}
STORE_RAW=${STORE_RAW:-0}

args=(
  multi
  --exchange "$EXCHANGE"
  --top-limit "$TOP_LIMIT"
  --depth-mode "$DEPTH_MODE"
  --depth-levels "$DEPTH_LEVELS"
  --flush-ms "$FLUSH_MS"
  --lob-mode "$LOB_MODE"
  --ch-url "$CLICKHOUSE_URL"
  --database "$CLICKHOUSE_DB"
)

if [[ "$STORE_RAW" == "1" || "$STORE_RAW" == "true" ]]; then
  args+=( --store-raw )
fi

exec docker run --rm --name hft-collector \
  -e CLICKHOUSE_USER="$CLICKHOUSE_USER" \
  -e CLICKHOUSE_PASSWORD="$CLICKHOUSE_PASSWORD" \
  "$IMAGE_NAME" "${args[@]}"

