#!/usr/bin/env bash
set -euo pipefail

# Build daily per-second labels from ClickHouse hft.* (Bitget futures) into S3.
# Layout: s3://<bucket>/labels/ex=<EX>/sym=<SYM>/dt=<YYYY-MM-DD>/labels.parquet
#
# Labels: y_edge_H{1,3,5} (raw mid diff in price units)
#
# Required env:
#   CLICKHOUSE_ENDPOINT, CLICKHOUSE_USER, CLICKHOUSE_PASS
#   S3_BUCKET
# Optional env:
#   AWS_REGION=ap-northeast-1
#   EX=bitget
#   DAY=YYYY-MM-DD (default: today UTC)
#   START_UTC / END_UTC (limit range; UTC, YYYYmmddHHMM)
#   SYMS (default fixed 11)
#   AUTO_DISCOVER=true|false (default false)
#   PARQUET_COMPRESSION=zstd
#   HORIZONS="1,3,5"

AWS_REGION=${AWS_REGION:-ap-northeast-1}
EX=${EX:-bitget}
DAY=${DAY:-$(date -u +%F)}
PARQUET_COMPRESSION=${PARQUET_COMPRESSION:-zstd}
HORIZONS=${HORIZONS:-"1,3,5"}
AUTO_DISCOVER=${AUTO_DISCOVER:-false}
DEFAULT_FIXED_SYMS=${DEFAULT_FIXED_SYMS:-"BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,XRPUSDT,SUIUSDT,MUSDT,WLFIUSDT,ENAUSDT,TAUSDT,IDOLUSDT"}

require() { local n="$1"; [[ -n "${!n:-}" ]] || { echo "Missing env: $n" >&2; exit 2; }; }
require CLICKHOUSE_ENDPOINT; require CLICKHOUSE_USER; require CLICKHOUSE_PASS; require S3_BUCKET

CH_ENDPOINT="$CLICKHOUSE_ENDPOINT"
CH_AUTH="$CLICKHOUSE_USER:$CLICKHOUSE_PASS"

discover_symbols() {
  local day="$1"
  local q="";
  read -r -d '' q <<SQL || true
SELECT DISTINCT symbol
FROM hft.bitget_futures_bbo
WHERE toDate(toDateTime64(exchange_ts/1000,3))=toDate('${day}')
ORDER BY symbol
FORMAT TSV
SQL
  curl -sS --fail --user "$CH_AUTH" "$CH_ENDPOINT" --data-binary "$q" | tr '\n' ',' | sed 's/,$//' || true
}

compute_bounds() {
  # Compute day start/end seconds and narrow by START_UTC/END_UTC if provided
  DAY_START_SEC=$(date -u -j -f "%F %T" "${DAY} 00:00:00" +%s 2>/dev/null || date -u -d "${DAY} 00:00:00" +%s)
  DAY_END_SEC=$((DAY_START_SEC + 86400))
  RANGE_START_SEC=$DAY_START_SEC
  RANGE_END_SEC=$DAY_END_SEC
  if [[ -n "${START_UTC:-}" ]]; then
    if START_SEC=$(date -u -j -f "%Y%m%d%H%M" "$START_UTC" +%s 2>/dev/null); then
      if (( START_SEC > RANGE_START_SEC )); then RANGE_START_SEC=$START_SEC; fi
    fi
  fi
  if [[ -n "${END_UTC:-}" ]]; then
    if END_SEC=$(date -u -j -f "%Y%m%d%H%M" "$END_UTC" +%s 2>/dev/null); then
      if (( END_SEC < RANGE_END_SEC )); then RANGE_END_SEC=$END_SEC; fi
    fi
  fi
}

build_labels_for_symbol_day() {
  local sym="$1"; local day="$2"
  compute_bounds
  local base="s3://${S3_BUCKET}/labels/ex=${EX}/sym=${sym}/dt=${day}"
  echo "→ labels ${sym} ${day} → ${base}/labels.parquet"

  # Build dynamic y_edge columns
  local select_h=""; local i=1
  IFS=',' read -r -a hs <<< "$HORIZONS"
  for h in "${hs[@]}"; do
    [[ -z "$h" ]] && continue
    select_h+=" , (leadInFrame(mid, ${h}) OVER (PARTITION BY symbol ORDER BY ts) - mid) AS y_edge_H${h}"
    i=$((i+1))
  done

  read -r -d '' SQL <<SQL || true
WITH
  toDateTime(${RANGE_START_SEC}) AS start_ts,
  toDateTime(${RANGE_END_SEC}) AS end_ts
SELECT ts, symbol, mid${select_h}
FROM (
  SELECT
    toDateTime(intDiv(toUnixTimestamp64Milli(toDateTime64(exchange_ts/1000,3)), 1000)) AS ts,
    '${sym}'::String AS symbol,
    argMax((best_bid + best_ask)/2, exchange_ts) AS mid
  FROM hft.bitget_futures_bbo
  WHERE symbol='${sym}'
    AND toDate(toDateTime64(exchange_ts/1000,3)) = toDate('${day}')
    AND (exchange_ts >= ${RANGE_START_SEC}000) AND (exchange_ts < ${RANGE_END_SEC}000)
  GROUP BY ts
  UNION ALL
  -- generate grid seconds
  SELECT toDateTime(number + toUnixTimestamp(start_ts)) AS ts, '${sym}' AS symbol, CAST(NULL AS Nullable(Float64)) AS mid
  FROM numbers(toUInt64(toUnixTimestamp(end_ts) - toUnixTimestamp(start_ts)))
)
GROUP BY ts, symbol
ORDER BY symbol, ts WITH FILL STEP toIntervalSecond(1)
FORMAT Parquet SETTINGS output_format_parquet_compression_method='${PARQUET_COMPRESSION}'
SQL

  curl -sS --fail --user "$CH_AUTH" "$CH_ENDPOINT" --data-binary "$SQL" \
    | aws s3 cp - "${base}/labels.parquet" --region "$AWS_REGION" --sse AES256 --only-show-errors
}

# main
if [[ -z "${SYMS:-}" ]]; then
  if [[ "$AUTO_DISCOVER" == "true" ]]; then
    echo "發現當日商品清單 (DAY=${DAY}) ..."
    SYMS=$(discover_symbols "$DAY")
  else
    SYMS="$DEFAULT_FIXED_SYMS"
  fi
fi

IFS=',' read -r -a syms_arr <<< "$SYMS"
for sym in "${syms_arr[@]}"; do
  [[ -z "$sym" ]] && continue
  build_labels_for_symbol_day "$sym" "$DAY"
done

echo "Labels export done."

