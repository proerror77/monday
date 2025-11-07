#!/usr/bin/env bash
set -euo pipefail

# Build daily per-second features from ClickHouse hft.* (Bitget futures) into S3.
# Layout: s3://<bucket>/features/ex=<EX>/sym=<SYM>/dt=<YYYY-MM-DD>/feats.parquet
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

AWS_REGION=${AWS_REGION:-ap-northeast-1}
EX=${EX:-bitget}
DAY=${DAY:-$(date -u +%F)}
PARQUET_COMPRESSION=${PARQUET_COMPRESSION:-zstd}
AUTO_DISCOVER=${AUTO_DISCOVER:-false}
DEFAULT_FIXED_SYMS=${DEFAULT_FIXED_SYMS:-"BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,XRPUSDT,SUIUSDT,MUSDT,WLFIUSDT,ENAUSDT,TAUSDT,IDOLUSDT"}
USE_TRADES=${USE_TRADES:-true}
USE_L2=${USE_L2:-false}
L2_K=${L2_K:-10}

require() { local n="$1"; [[ -n "${!n:-}" ]] || { echo "Missing env: $n" >&2; exit 2; }; }
require CLICKHOUSE_ENDPOINT; require CLICKHOUSE_USER; require CLICKHOUSE_PASS; require S3_BUCKET

ch() { curl -sS --fail --user "${CLICKHOUSE_USER}:${CLICKHOUSE_PASS}}" "${CLICKHOUSE_ENDPOINT}" --data-binary "$1"; }

# Workaround: use CH auth without wrapper to avoid brace typo in ch()
CH_ENDPOINT="$CLICKHOUSE_ENDPOINT"
CH_AUTH="$CLICKHOUSE_USER:$CLICKHOUSE_PASS"

discover_symbols() {
  local day="$1"
  local q="";
  read -r -d '' q <<SQL || true
SELECT DISTINCT symbol
FROM (
  SELECT symbol FROM hft.bitget_futures_bbo
    WHERE toDate(toDateTime64(exchange_ts/1000, 3)) = toDate('${day}')
  UNION ALL
  SELECT symbol FROM hft.bitget_futures_trades
    WHERE toDate(toDateTime64(exchange_ts/1000, 3)) = toDate('${day}')
  UNION ALL
  SELECT symbol FROM hft.bitget_futures_ticker
    WHERE toDate(toDateTime64(exchange_ts/1000, 3)) = toDate('${day}')
)
ORDER BY symbol
FORMAT TSV
SQL
  curl -sS --fail --user "$CH_AUTH" "$CH_ENDPOINT" --data-binary "$q" | tr '\n' ',' | sed 's/,$//' || true
}

parse_bounds_for_day() {
  local day="$1"
  lower_ms_cond=""; upper_ms_cond=""; START_DAY=""; END_DAY=""
  if [[ -n "${START_UTC:-}" ]]; then
    if ! START_SEC=$(date -u -j -f "%Y%m%d%H%M" "$START_UTC" +%s 2>/dev/null); then
      echo "無法解析 START_UTC='$START_UTC'" >&2; exit 2
    fi
    START_MS=$(( START_SEC * 1000 ))
    START_DAY=$(date -u -r "$START_SEC" +%F)
    if [[ "$START_DAY" == "$day" ]]; then
      lower_ms_cond="AND exchange_ts >= ${START_MS}"
    fi
  fi
  if [[ -n "${END_UTC:-}" ]]; then
    if ! END_SEC=$(date -u -j -f "%Y%m%d%H%M" "$END_UTC" +%s 2>/dev/null); then
      echo "無法解析 END_UTC='$END_UTC'" >&2; exit 2
    fi
    END_MS=$(( END_SEC * 1000 ))
    END_DAY=$(date -u -r "$END_SEC" +%F)
    if [[ "$END_DAY" == "$day" ]]; then
      upper_ms_cond="AND exchange_ts < ${END_MS}"
    fi
  fi
}

export_feats_for_symbol_day() {
  local sym="$1"; local day="$2"
  parse_bounds_for_day "$day"
  local base="s3://${S3_BUCKET}/features/ex=${EX}/sym=${sym}/dt=${day}"
  echo "→ features ${sym} ${day} → ${base}/feats.parquet"

  read -r -d '' SQL <<SQL || true
WITH
  1e-9 AS eps
SELECT
  toStartOfSecond(toDateTime64(exchange_ts/1000, 3)) AS ts,
  '${sym}'::String AS symbol,
  argMax((best_bid + best_ask)/2, exchange_ts) AS mid,
  argMax(best_ask - best_bid, exchange_ts) AS spread,
  argMax((best_ask*best_bid_size + best_bid*best_ask_size)/(best_bid_size + best_ask_size + eps), exchange_ts) AS microprice,
  argMax(((2*((best_ask*best_bid_size + best_bid*best_ask_size)/(best_bid_size + best_ask_size + eps))) - best_bid - best_ask) / greatest(best_ask - best_bid, eps), exchange_ts) AS micro_tilt,
  argMax((best_bid_size - best_ask_size) / (best_bid_size + best_ask_size + eps), exchange_ts) AS qi,
  sum(
    multiIf(
      best_bid > lagInFrame(best_bid, 1) OVER (PARTITION BY symbol ORDER BY exchange_ts, local_ts), best_bid_size,
      best_bid < lagInFrame(best_bid, 1) OVER (PARTITION BY symbol ORDER BY exchange_ts, local_ts), -lagInFrame(best_bid_size, 1) OVER (PARTITION BY symbol ORDER BY exchange_ts, local_ts),
      0
    )
    - multiIf(
      best_ask < lagInFrame(best_ask, 1) OVER (PARTITION BY symbol ORDER BY exchange_ts, local_ts), best_ask_size,
      best_ask > lagInFrame(best_ask, 1) OVER (PARTITION BY symbol ORDER BY exchange_ts, local_ts), -lagInFrame(best_ask_size, 1) OVER (PARTITION BY symbol ORDER BY exchange_ts, local_ts),
      0
    )
  ) AS ofi_sum,
  count() AS q_updates,
  quantile(0.5)(greatest(local_ts - exchange_ts, 0)) AS data_age_p50_ms,
  quantile(0.95)(greatest(local_ts - exchange_ts, 0)) AS data_age_p95_ms
FROM hft.bitget_futures_bbo
WHERE symbol='${sym}'
  AND toDate(toDateTime64(exchange_ts/1000, 3)) = toDate('${day}')
  ${lower_ms_cond}
  ${upper_ms_cond}
GROUP BY ts
ORDER BY ts
FORMAT Parquet SETTINGS output_format_parquet_compression_method='${PARQUET_COMPRESSION}'
SQL

  curl -sS --fail --user "$CH_AUTH" "$CH_ENDPOINT" --data-binary "$SQL" \
    | aws s3 cp - "${base}/feats.parquet" --region "$AWS_REGION" --sse AES256 --only-show-errors
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
  export_feats_for_symbol_day "$sym" "$DAY"
done

echo "Features export done."

