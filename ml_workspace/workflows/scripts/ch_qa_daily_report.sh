#!/usr/bin/env bash
set -euo pipefail

# Produce daily QA metrics for hft.* Bitget futures and upload to S3.
# Outputs under s3://<bucket>/qa/dt=<DAY>/
#
# Required env:
#   CLICKHOUSE_ENDPOINT, CLICKHOUSE_USER, CLICKHOUSE_PASS
#   S3_BUCKET
# Optional:
#   AWS_REGION=ap-northeast-1
#   DAY=YYYY-MM-DD (default: today UTC)

AWS_REGION=${AWS_REGION:-ap-northeast-1}
DAY=${DAY:-$(date -u +%F)}

require() { local n="$1"; [[ -n "${!n:-}" ]] || { echo "Missing env: $n" >&2; exit 2; }; }
require CLICKHOUSE_ENDPOINT; require CLICKHOUSE_USER; require CLICKHOUSE_PASS; require S3_BUCKET

ch() { curl -sS --fail --user "${CLICKHOUSE_USER}:${CLICKHOUSE_PASS}" "${CLICKHOUSE_ENDPOINT}" --data-binary "$1"; }

prefix="s3://${S3_BUCKET}/qa/dt=${DAY}"
echo "Writing QA to ${prefix}"

# 1) BBO per-symbol row counts (safe and cheap)
read -r -d '' Q_BBO_ROWS <<SQL || true
SELECT symbol, count() AS rows
FROM hft.bitget_futures_bbo
WHERE toDate(toDateTime64(exchange_ts/1000,3)) = toDate('${DAY}')
GROUP BY symbol
ORDER BY rows DESC
FORMAT CSVWithNames
SQL
ch "$Q_BBO_ROWS" | aws s3 cp - "${prefix}/bbo_rows.csv" --sse AES256 --only-show-errors

# 2) Per-table symbol presence
read -r -d '' Q_PRES <<'SQL' || true
WITH toDate('{DAY}') AS day
SELECT s.symbol,
  s.symbol IN (SELECT DISTINCT symbol FROM hft.bitget_futures_bbo   WHERE toDate(toDateTime64(exchange_ts/1000,3))=day)   AS has_bbo,
  s.symbol IN (SELECT DISTINCT symbol FROM hft.bitget_futures_depth WHERE toDate(toDateTime64(exchange_ts/1000,3))=day)   AS has_depth,
  s.symbol IN (SELECT DISTINCT symbol FROM hft.bitget_futures_trades WHERE toDate(toDateTime64(exchange_ts/1000,3))=day)  AS has_trades,
  s.symbol IN (SELECT DISTINCT symbol FROM hft.bitget_futures_ticker WHERE toDate(toDateTime64(exchange_ts/1000,3))=day)  AS has_ticker
FROM (
  SELECT DISTINCT symbol FROM hft.bitget_futures_bbo   WHERE toDate(toDateTime64(exchange_ts/1000,3))=day
  UNION DISTINCT
  SELECT DISTINCT symbol FROM hft.bitget_futures_depth WHERE toDate(toDateTime64(exchange_ts/1000,3))=day
  UNION DISTINCT
  SELECT DISTINCT symbol FROM hft.bitget_futures_trades WHERE toDate(toDateTime64(exchange_ts/1000,3))=day
  UNION DISTINCT
  SELECT DISTINCT symbol FROM hft.bitget_futures_ticker WHERE toDate(toDateTime64(exchange_ts/1000,3))=day
) s
ORDER BY s.symbol
FORMAT CSVWithNames
SQL
Q2=${Q_PRES//\{DAY\}/$DAY}
ch "$Q2" | aws s3 cp - "${prefix}/table_presence.csv" --sse AES256 --only-show-errors

# 3) Raw WS events by channel
read -r -d '' Q_EVT <<SQL || true
SELECT channel, count() AS cnt
FROM hft.raw_ws_events
WHERE toDate(toDateTime64(timestamp/1000000,6))=toDate('${DAY}')
GROUP BY channel
ORDER BY cnt DESC
FORMAT CSVWithNames
SQL
ch "$Q_EVT" | aws s3 cp - "${prefix}/ws_events.csv" --sse AES256 --only-show-errors

echo "QA export done."

