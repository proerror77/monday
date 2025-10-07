#!/usr/bin/env bash
set -euo pipefail

# ClickHouse → S3 direct write for L2 (depth) per-hour Parquet slices using s3() table function.
#
# Output per hour:
#   s3://$S3_BUCKET/$S3_PREFIX/ex=$EX/sym=$SYM/dt=$DAY/l2_h=$HH.parquet
#
# Required env:
#   CLICKHOUSE_ENDPOINT, CLICKHOUSE_USER, CLICKHOUSE_PASS
#   S3_BUCKET
# Optional env:
#   AWS_REGION=ap-northeast-1
#   S3_PREFIX=datasets
#   EX=bitget
#   START_UTC=YYYYmmddHHMM (inclusive, UTC)  OR  DAY=YYYY-MM-DD
#   END_UTC=YYYYmmddHHMM   (exclusive, UTC)  (required if START_UTC is set)
#   SYMS=comma list (default fixed 11)
#   L2_MAX_LEVEL=20  (0 = all levels)
#   PARALLEL=2       (number of concurrent slots per symbol)
#   SLOT_MIN=10      (minutes per file)
#   OVERWRITE=false  (skip if object exists)
#   ASKS_COL=asks    (use 'asks' or 'tasks' depending on schema)

require() { local n="$1"; [[ -n "${!n:-}" ]] || { echo "Missing env: $n" >&2; exit 2; }; }

AWS_REGION=${AWS_REGION:-ap-northeast-1}
S3_PREFIX=${S3_PREFIX:-datasets}
EX=${EX:-bitget}
PARALLEL=${PARALLEL:-2}
SLOT_MIN=${SLOT_MIN:-10}
L2_MAX_LEVEL=${L2_MAX_LEVEL:-20}
OVERWRITE=${OVERWRITE:-false}
DEFAULT_FIXED_SYMS=${DEFAULT_FIXED_SYMS:-"BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,XRPUSDT,SUIUSDT,MUSDT,WLFIUSDT,ENAUSDT,TAUSDT,IDOLUSDT,BGBUSDT"}
ASKS_COL=${ASKS_COL:-asks}

require CLICKHOUSE_ENDPOINT; require CLICKHOUSE_USER; require CLICKHOUSE_PASS; require S3_BUCKET

CH_ENDPOINT="$CLICKHOUSE_ENDPOINT"
CH_AUTH="$CLICKHOUSE_USER:$CLICKHOUSE_PASS"

# Pull AWS keys from default profile (only used in SQL; not printed)
AK=$(aws configure get aws_access_key_id)
SK=$(aws configure get aws_secret_access_key)
if [[ -z "$AK" || -z "$SK" ]]; then echo "No AWS access keys configured for direct S3 write" >&2; exit 2; fi

to_sec_utc() { date -u -j -f "%Y%m%d%H%M" "$1" +%s 2>/dev/null || date -u -d "${1:0:8} ${1:8:2}:${1:10:2}" +%s; }
to_day_start_sec() { date -u -j -f "%F %T" "$1 00:00:00" +%s 2>/dev/null || date -u -d "$1 00:00:00" +%s; }

if [[ -n "${DAY:-}" ]]; then
  START_SEC=$(to_day_start_sec "$DAY")
  END_SEC=$(( START_SEC + 86400 ))
else
  require START_UTC; require END_UTC
  START_SEC=$(to_sec_utc "$START_UTC")
  END_SEC=$(to_sec_utc "$END_UTC")
  DAY=$(date -u -r "$START_SEC" +%F)
fi

if [[ -z "${SYMS:-}" ]]; then
  SYMS="$DEFAULT_FIXED_SYMS"
fi

ACCOUNT_ID=$(aws sts get-caller-identity --query Account --output text)
BUCKET="$S3_BUCKET"

echo "Direct S3 write L2: syms=[$SYMS], window=$(date -u -r "$START_SEC" +%FT%TZ)→$(date -u -r "$END_SEC" +%FT%TZ), L2_MAX_LEVEL=${L2_MAX_LEVEL}, PARALLEL=${PARALLEL}, asks_col=${ASKS_COL}"

# Build slot list JSON
slot_json=$(START_SEC=$START_SEC END_SEC=$END_SEC SLOT_MIN=$SLOT_MIN python - <<'PY'
import os, json, datetime
start=int(os.environ['START_SEC']); end=int(os.environ['END_SEC']); step=int(os.environ['SLOT_MIN'])*60
cur=start
arr=[]
while cur < end:
    d=datetime.datetime.utcfromtimestamp(cur).strftime('%Y-%m-%d')
    hh=datetime.datetime.utcfromtimestamp(cur).strftime('%H')
    mm=datetime.datetime.utcfromtimestamp(cur).strftime('%M')
    arr.append({"start":cur, "end":min(cur+step,end), "day":d, "hh":hh, "mm":mm})
    cur += step
print(json.dumps(arr))
PY
)

run_hour() {
  local sym="$1"; local start="$2"; local end="$3"; local day="$4"; local hh="$5"; local mm="$6"
  local key="${S3_PREFIX}/ex=${EX}/sym=${sym}/dt=${day}/l2_h=${hh}m${mm}.parquet"
  local s3_url="https://${BUCKET}.s3.ap-northeast-1.amazonaws.com/${key}"
  if [[ "$OVERWRITE" != "true" ]] && aws s3 ls "s3://${BUCKET}/${key}" >/dev/null 2>&1; then
    echo "[skip] exists: s3://${BUCKET}/${key}"
    return 0
  fi
  local K="$L2_MAX_LEVEL"
  local SLICE_BIDS SLICE_ASKS
  if [[ "$K" == "0" ]]; then
    SLICE_BIDS="bids"; SLICE_ASKS="$ASKS_COL"
  else
    SLICE_BIDS="arraySlice(bids,1,toUInt32(${K}))"; SLICE_ASKS="arraySlice(${ASKS_COL},1,toUInt32(${K}))"
  fi
  read -r -d '' SQL <<SQL || true
INSERT INTO FUNCTION s3('${s3_url}', '${AK}', '${SK}', 'Parquet')
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
         arrayJoin(arrayEnumerate(${SLICE_BIDS})) AS lvl,
         'bid' AS side,
         ${SLICE_BIDS}[lvl].1 AS px,
         ${SLICE_BIDS}[lvl].2 AS qty
  FROM hft.bitget_futures_depth
  WHERE symbol='${sym}' AND exchange_ts >= toUnixTimestamp64Milli(ts_from) AND exchange_ts < toUnixTimestamp64Milli(ts_to)
  UNION ALL
  SELECT symbol, exchange_ts, local_ts,
         arrayJoin(arrayEnumerate(${SLICE_ASKS})) AS lvl,
         'ask' AS side,
         ${SLICE_ASKS}[lvl].1 AS px,
         ${SLICE_ASKS}[lvl].2 AS qty
  FROM hft.bitget_futures_depth
  WHERE symbol='${sym}' AND exchange_ts >= toUnixTimestamp64Milli(ts_from) AND exchange_ts < toUnixTimestamp64Milli(ts_to)
)
ORDER BY ts, side, level
SETTINGS 
  output_format_parquet_compression_method='zstd',
  output_format_compression_level=1,
  max_insert_threads=16,
  s3_create_new_file_on_insert=1,
  s3_min_upload_part_size=67108864,
  s3_max_upload_part_size=134217728,
  s3_max_single_part_upload_size=33554432
SQL
  echo "[run] ${sym} ${day} ${hh}:${mm} → s3://${BUCKET}/${key}"
  curl -sS --fail --user "$CH_AUTH" "$CH_ENDPOINT" --data-binary "$SQL" >/dev/null
}

# Parallel execution using background jobs (Bash 3 compatible)
jobc=0
for sym in $(echo "$SYMS" | tr ',' ' '); do
  while IFS='|' read -r start end day hh mm; do
    run_hour "$sym" "$start" "$end" "$day" "$hh" "$mm" &
    jobc=$((jobc+1))
    if (( jobc >= PARALLEL )); then
      oldest=$(jobs -p | head -n 1 || true)
      if [[ -n "$oldest" ]]; then wait "$oldest" || true; fi
      jobc=$((jobc-1))
    fi
  done < <(SLOTS_JSON="$slot_json" python - <<'PY'
import os, json
for x in json.loads(os.environ['SLOTS_JSON']):
    print(f"{x['start']}|{x['end']}|{x['day']}|{x['hh']}|{x['mm']}")
PY
)
done

wait || true
echo "Done."
