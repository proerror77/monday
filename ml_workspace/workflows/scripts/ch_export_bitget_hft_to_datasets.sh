#!/usr/bin/env bash
set -euo pipefail

# ClickHouse (hft.* Bitget futures) → S3 datasets/ exporter (all symbols for a day)
# Layout: s3://<bucket>/datasets/ex=<EX>/sym=<SYM>/dt=<YYYY-MM-DD>/{l1,l2,trades}.parquet
#
# Required env:
#   CLICKHOUSE_ENDPOINT  (e.g. https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443)
#   CLICKHOUSE_USER      (e.g. default)
#   CLICKHOUSE_PASS
#   S3_BUCKET            (target bucket)
# Optional env:
#   AWS_REGION=ap-northeast-1
#   EX=bitget
#   DAY=YYYY-MM-DD (default: today UTC)  [單日模式]
#   START_UTC=YYYYmmddHHMM                [區間模式：起始（UTC）]
#   END_UTC=YYYYmmddHHMM                  [區間模式：結束（UTC），預設現在]
#   SYMS=BTCUSDT,ETHUSDT (預設固定 11 檔)
#   AUTO_DISCOVER=true|false (預設 false；如為 true 則依天自動發現)
#   PARQUET_COMPRESSION=zstd
#   SKIP_L2=true|false (default false)
#
# Notes:
# - Uses object-level SSE-S3 for writes (bucket already has SSE by default).
# - tick is set to 0.0 (no tick-size catalog in DB). Can be updated later.
# - seq uses window row_number() by (symbol, date) on (exchange_ts, local_ts).

AWS_REGION=${AWS_REGION:-ap-northeast-1}
EX=${EX:-bitget}
DAY=${DAY:-$(date -u +%F)}
PARQUET_COMPRESSION=${PARQUET_COMPRESSION:-zstd}
SKIP_L2=${SKIP_L2:-false}
AUTO_DISCOVER=${AUTO_DISCOVER:-false}
DEFAULT_FIXED_SYMS=${DEFAULT_FIXED_SYMS:-"BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,XRPUSDT,SUIUSDT,MUSDT,WLFIUSDT,ENAUSDT,TAUSDT,IDOLUSDT"}

require() { local n="$1"; [[ -n "${!n:-}" ]] || { echo "Missing env: $n" >&2; exit 2; }; }
require CLICKHOUSE_ENDPOINT; require CLICKHOUSE_USER; require CLICKHOUSE_PASS; require S3_BUCKET

ch() { curl -sS --fail --user "${CLICKHOUSE_USER}:${CLICKHOUSE_PASS}" "${CLICKHOUSE_ENDPOINT}" --data-binary "$1"; }

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
  UNION ALL
  SELECT symbol FROM hft.bitget_futures_depth
    WHERE toDate(toDateTime64(exchange_ts/1000, 3)) = toDate('${day}')
)
ORDER BY symbol
FORMAT TSV
SQL
  ch "$q" | tr '\n' ',' | sed 's/,$//' || true
}

export_symbol_day() {
  local sym="$1"; local day="$2"
  local base="s3://${S3_BUCKET}/datasets/ex=${EX}/sym=${sym}/dt=${day}"
  echo "→ ${sym} ${day} → ${base}"

  # 依需要套用時間區間界線（毫秒）
  local lower_ms_cond="" upper_ms_cond=""
  if [[ -n "${START_MS:-}" && "$day" == "$START_DAY" ]]; then
    lower_ms_cond="AND exchange_ts >= ${START_MS}"
  fi
  if [[ -n "${END_MS:-}" && "$day" == "$END_DAY" ]]; then
    upper_ms_cond="AND exchange_ts < ${END_MS}"
  fi

  # L1 from BBO
  read -r -d '' SQL_L1 <<SQL || true
SELECT
  toDateTime64(exchange_ts/1000, 3) AS ts,
  best_bid AS bid1,
  best_ask AS ask1,
  best_bid_size AS b1_qty,
  best_ask_size AS a1_qty,
  (best_ask - best_bid) AS spread,
  CAST(0.0 AS Float64) AS tick,
  row_number() OVER (PARTITION BY symbol, toDate(toDateTime64(exchange_ts/1000,3)) ORDER BY exchange_ts, local_ts) AS seq
FROM hft.bitget_futures_bbo
WHERE symbol='${sym}'
  AND toDate(toDateTime64(exchange_ts/1000, 3)) = toDate('${day}')
  ${lower_ms_cond}
  ${upper_ms_cond}
ORDER BY ts
FORMAT Parquet SETTINGS output_format_parquet_compression_method='${PARQUET_COMPRESSION}'
SQL
  ch "$SQL_L1" | aws s3 cp - "${base}/l1.parquet" --region "$AWS_REGION" --sse AES256 --only-show-errors

  if [[ "$SKIP_L2" != "true" ]]; then
    # L2 from depth (flatten arrays)
    read -r -d '' SQL_L2 <<SQL || true
WITH toDateTime64(exchange_ts/1000,3) AS ts
SELECT
  ts AS ts,
  side,
  lvl AS level,
  toString(px) AS price_str,
  toString(qty) AS qty_str,
  row_number() OVER (PARTITION BY symbol, toDate(ts) ORDER BY exchange_ts, local_ts) AS seq
FROM (
  SELECT symbol, exchange_ts, local_ts,
         arrayJoin(arrayEnumerate(bids)) AS lvl,
         'bid' AS side,
         bids[lvl].1 AS px,
         bids[lvl].2 AS qty
  FROM hft.bitget_futures_depth
  WHERE symbol='${sym}' AND toDate(toDateTime64(exchange_ts/1000,3)) = toDate('${day}') ${lower_ms_cond} ${upper_ms_cond}
  UNION ALL
  SELECT symbol, exchange_ts, local_ts,
         arrayJoin(arrayEnumerate(tasks)) AS lvl,
         'ask' AS side,
         tasks[lvl].1 AS px,
         tasks[lvl].2 AS qty
  FROM hft.bitget_futures_depth
  WHERE symbol='${sym}' AND toDate(toDateTime64(exchange_ts/1000,3)) = toDate('${day}') ${lower_ms_cond} ${upper_ms_cond}
)
ORDER BY ts, side, level
FORMAT Parquet SETTINGS output_format_parquet_compression_method='${PARQUET_COMPRESSION}'
SQL
    ch "$SQL_L2" | aws s3 cp - "${base}/l2.parquet" --region "$AWS_REGION" --sse AES256 --only-show-errors
  fi

  # Trades
  read -r -d '' SQL_TR <<SQL || true
SELECT
  toDateTime64(exchange_ts/1000, 3) AS ts,
  price,
  size AS qty,
  CAST(side AS String) AS aggressor
FROM hft.bitget_futures_trades
WHERE symbol='${sym}'
  AND toDate(toDateTime64(exchange_ts/1000, 3)) = toDate('${day}')
  ${lower_ms_cond}
  ${upper_ms_cond}
ORDER BY ts
FORMAT Parquet SETTINGS output_format_parquet_compression_method='${PARQUET_COMPRESSION}'
SQL
  ch "$SQL_TR" | aws s3 cp - "${base}/trades.parquet" --region "$AWS_REGION" --sse AES256 --only-show-errors
}

# 區間模式：若提供 START_UTC（YYYYmmddHHMM），則從起始日時跨日匯出至 END_UTC（或現在）
if [[ -n "${START_UTC:-}" ]]; then
  # 以 UTC 解析
  if ! START_SEC=$(date -u -j -f "%Y%m%d%H%M" "$START_UTC" +%s 2>/dev/null); then
    echo "無法解析 START_UTC='$START_UTC'，期望格式 YYYYmmddHHMM (UTC)" >&2; exit 2
  fi
  START_MS=$(( START_SEC * 1000 ))
  START_DAY=$(date -u -r "$START_SEC" +%F)

  if [[ -n "${END_UTC:-}" ]]; then
    if ! END_SEC=$(date -u -j -f "%Y%m%d%H%M" "$END_UTC" +%s 2>/dev/null); then
      echo "無法解析 END_UTC='$END_UTC'，期望格式 YYYYmmddHHMM (UTC)" >&2; exit 2
    fi
  else
    END_SEC=$(date -u +%s)
  fi
  END_MS=$(( END_SEC * 1000 ))
  END_DAY=$(date -u -r "$END_SEC" +%F)

  echo "區間模式：${START_UTC} (UTC) → ${END_UTC:-NOW} (UTC)"

  # 逐日迭代
  cur_day="$START_DAY"
  while : ; do
    # 依設定取得商品清單
    if [[ -z "${SYMS:-}" ]]; then
      if [[ "$AUTO_DISCOVER" == "true" ]]; then
        echo "發現商品清單 (DAY=${cur_day}) ..."
        SYMS=$(discover_symbols "$cur_day")
      else
        SYMS="$DEFAULT_FIXED_SYMS"
      fi
    fi
    IFS=',' read -r -a syms_arr <<< "${SYMS}"
    for sym in "${syms_arr[@]}"; do
      [[ -z "$sym" ]] && continue
      export_symbol_day "$sym" "$cur_day"
    done

    # 到達結束日即停止
    if [[ "$cur_day" == "$END_DAY" ]]; then
      break
    fi
    # 下一天 (UTC)
    next_day=$(date -u -j -f "%F" "$cur_day" -v+1d +%F 2>/dev/null || date -u -d "$cur_day +1 day" +%F)
    cur_day="$next_day"
    SYMS=""  # 下一天重新發現
  done
else
  # 單日模式
  if [[ -z "${SYMS:-}" ]]; then
    if [[ "$AUTO_DISCOVER" == "true" ]]; then
      echo "發現當日商品清單 (DAY=${DAY}) ..."
      SYMS=$(discover_symbols "$DAY")
    else
      SYMS="$DEFAULT_FIXED_SYMS"
    fi
  fi
  IFS=',' read -r -a syms_arr <<< "${SYMS}"
  for sym in "${syms_arr[@]}"; do
    [[ -z "$sym" ]] && continue
    export_symbol_day "$sym" "$DAY"
  done
fi

echo "完成。可用 'aws s3 ls s3://${S3_BUCKET}/datasets/ --recursive' 檢查。"
