#!/usr/bin/env bash
set -euo pipefail

# Export L2 (orderbook depth) per-hour Parquet slices from ClickHouse to S3.
#
# Output per hour:
#   s3://<bucket>/datasets/ex=<EX>/sym=<SYM>/dt=<YYYY-MM-DD>/l2_h=<HH>.parquet
#
# Required env:
#   CLICKHOUSE_ENDPOINT, CLICKHOUSE_USER, CLICKHOUSE_PASS
#   S3_BUCKET
# Optional env:
#   AWS_REGION=ap-northeast-1
#   EX=bitget
#   START_UTC=YYYYmmddHHMM (inclusive, UTC)
#   END_UTC=YYYYmmddHHMM   (exclusive, UTC)
#   DAY=YYYY-MM-DD (if set, exports the full day)
#   SYMS=comma list (default fixed 11)
#   L2_MAX_LEVEL=20   (0 means all levels)
#   PARALLEL=2        (concurrent uploads)
#   OVERWRITE=false   (skip if object exists)
#   RETRIES=3         (retry count per hour)

require() { local n="$1"; [[ -n "${!n:-}" ]] || { echo "Missing env: $n" >&2; exit 2; }; }

AWS_REGION=${AWS_REGION:-ap-northeast-1}
EX=${EX:-bitget}
PARALLEL=${PARALLEL:-2}
RETRIES=${RETRIES:-3}
OVERWRITE=${OVERWRITE:-false}
L2_MAX_LEVEL=${L2_MAX_LEVEL:-20}
DEFAULT_FIXED_SYMS=${DEFAULT_FIXED_SYMS:-"BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,XRPUSDT,SUIUSDT,MUSDT,WLFIUSDT,ENAUSDT,TAUSDT,IDOLUSDT"}

require CLICKHOUSE_ENDPOINT; require CLICKHOUSE_USER; require CLICKHOUSE_PASS; require S3_BUCKET

CH_ENDPOINT="$CLICKHOUSE_ENDPOINT"
CH_AUTH="$CLICKHOUSE_USER:$CLICKHOUSE_PASS"

# Resolve time window
to_sec_utc() { date -u -j -f "%Y%m%d%H%M" "$1" +%s 2>/dev/null || date -u -d "${1:0:8} ${1:8:2}:${1:10:2}" +%s; }
to_day_start_sec() { date -u -j -f "%F %T" "$1 00:00:00" +%s 2>/dev/null || date -u -d "$1 00:00:00" +%s; }

if [[ -n "${DAY:-}" ]]; then
  START_SEC=$(to_day_start_sec "$DAY")
  END_SEC=$(( START_SEC + 86400 ))
else
  require START_UTC; require END_UTC
  START_SEC=$(to_sec_utc "$START_UTC")
  END_SEC=$(to_sec_utc "$END_UTC")
fi

if [[ -z "${SYMS:-}" ]]; then
  SYMS="$DEFAULT_FIXED_SYMS"
fi

# Build hour buckets (aligned)
hours_json=$(START_SEC=$START_SEC END_SEC=$END_SEC python - <<'PY'
import sys, json, os
from datetime import datetime, timezone
start=int(os.environ['START_SEC']); end=int(os.environ['END_SEC'])
start = start - (start % 3600)
arr=[]
cur=start
while cur < end:
    day=datetime.fromtimestamp(cur, tz=timezone.utc).strftime('%Y-%m-%d')
    hh=datetime.fromtimestamp(cur, tz=timezone.utc).strftime('%H')
    arr.append({"start":cur, "end":min(cur+3600,end), "day":day, "hh":hh})
    cur += 3600
print(json.dumps(arr))
PY
)

job_count=0
max_jobs="$PARALLEL"

submit_task() {
  local sym="$1"; local start="$2"; local end="$3"; local day="$4"; local hh="$5"
  local prefix="s3://${S3_BUCKET}/datasets/ex=${EX}/sym=${sym}/dt=${day}"
  local key="${prefix}/l2_h=${hh}.parquet"

  if [[ "$OVERWRITE" != "true" ]]; then
    if aws s3 ls "$key" >/dev/null 2>&1; then
      echo "[skip] exists: $key"
      return
    fi
  fi

  (
    echo "[run] $sym $day $hh → $key"
    K="$L2_MAX_LEVEL"
    lvl_slice_bids="bids"; lvl_slice_asks="tasks"
    if [[ "$K" != "0" ]]; then
      lvl_slice_bids="arraySlice(bids,1,toUInt32(${K}))"
      lvl_slice_asks="arraySlice(tasks,1,toUInt32(${K}))"
    fi
    read -r -d '' SQL <<SQL || true
WITH toDateTime64(${start},3) AS ts_from, toDateTime64(${end},3) AS ts_to
SELECT
  toDateTime64(exchange_ts/1000,3) AS ts,
  side,
  lvl AS level,
  toString(px) AS price_str,
  toString(qty) AS qty_str,
  row_number() OVER (PARTITION BY symbol, toDate(ts) ORDER BY exchange_ts, local_ts) AS seq
FROM (
  SELECT symbol, exchange_ts, local_ts,
         arrayJoin(arrayEnumerate(${lvl_slice_bids})) AS lvl,
         'bid' AS side,
         ${lvl_slice_bids}[lvl].1 AS px,
         ${lvl_slice_bids}[lvl].2 AS qty
  FROM hft.bitget_futures_depth
  WHERE symbol='${sym}' AND exchange_ts >= toUnixTimestamp64Milli(ts_from) AND exchange_ts < toUnixTimestamp64Milli(ts_to)
  UNION ALL
  SELECT symbol, exchange_ts, local_ts,
         arrayJoin(arrayEnumerate(${lvl_slice_asks})) AS lvl,
         'ask' AS side,
         ${lvl_slice_asks}[lvl].1 AS px,
         ${lvl_slice_asks}[lvl].2 AS qty
  FROM hft.bitget_futures_depth
  WHERE symbol='${sym}' AND exchange_ts >= toUnixTimestamp64Milli(ts_from) AND exchange_ts < toUnixTimestamp64Milli(ts_to)
)
ORDER BY ts, side, level
FORMAT Parquet SETTINGS output_format_parquet_compression_method='zstd'
SQL

    tmpf=$(mktemp)
    trap 'rm -f "$tmpf"' RETURN INT TERM HUP
    ok=0; n=0
    while (( n < ${RETRIES} )); do
      if curl -sS --fail --user "$CH_AUTH" "$CH_ENDPOINT" --data-binary "$SQL" -o "$tmpf"; then
        if [[ -s "$tmpf" ]]; then
          if aws s3 cp "$tmpf" "$key" --region "$AWS_REGION" --sse AES256 --only-show-errors; then
            ok=1; break
          fi
        fi
      fi
      n=$((n+1)); sleep $((2*n))
    done
    if (( ! ok )); then
      echo "[fail] $sym $day $hh"
    fi
  ) &

  job_count=$((job_count+1))
  if (( job_count >= max_jobs )); then
    # Bash 3 compatibility: wait for the oldest job
    oldest=$(jobs -p | head -n 1 || true)
    if [[ -n "$oldest" ]]; then wait "$oldest" || true; fi
    job_count=$((job_count-1))
  fi
}

echo "Exporting L2 hourly slices: syms=[$SYMS], window=$(date -u -r "$START_SEC" +%FT%TZ)→$(date -u -r "$END_SEC" +%FT%TZ), L2_MAX_LEVEL=${L2_MAX_LEVEL}, PARALLEL=${PARALLEL}"

for sym in $(echo "$SYMS" | tr ',' ' '); do
  while IFS='|' read -r start end day hh; do
    submit_task "$sym" "$start" "$end" "$day" "$hh"
  done < <(HOURS_JSON="$hours_json" python - <<'PY'
import os, json
arr=json.loads(os.environ['HOURS_JSON'])
for x in arr:
    print(f"{x['start']}|{x['end']}|{x['day']}|{x['hh']}")
PY
)
done

wait || true
echo "Done."
