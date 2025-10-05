#!/usr/bin/env bash
set -euo pipefail

# Validate ingest correctness for one exchange (spot + futures):
#   - Runs schema check (non-blocking)
#   - Runs collector ~60s
#   - Verifies 4 tables have recency and sane values
# Usage:
#   EXCHANGE=bitget SYMBOL_SPOT=BTCUSDT SYMBOL_FUT=BTCUSDT \
#   CH_URL=... CH_USER=... CH_PASSWORD=... CH_DATABASE=hft_db \
#   ./validate_ingest.sh

EXCHANGE=${EXCHANGE:-bitget}            # bitget|binance
SYMBOL_SPOT=${SYMBOL_SPOT:-BTCUSDT}
SYMBOL_FUT=${SYMBOL_FUT:-BTCUSDT}
CH_URL=${CH_URL:-"https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"}
CH_USER=${CH_USER:-default}
CH_PASSWORD=${CH_PASSWORD:-}
CH_DATABASE=${CH_DATABASE:-hft_db}
DURATION=${DURATION:-60}

cd "$(dirname "$0")/.." >/dev/null

echo "== Step 1: Schema check (non-blocking) =="
scripts/check_schema.sh "${EXCHANGE},${EXCHANGE}_futures" || true

echo
echo "== Step 2: Baseline counts =="
tables=()
case "$EXCHANGE" in
  bitget)
    tables=(bitget_orderbook bitget_trades bitget_futures_orderbook bitget_futures_trades)
    ;;
  binance)
    tables=(binance_orderbook binance_trades binance_futures_orderbook binance_futures_trades)
    ;;
  *) echo "Unsupported EXCHANGE=$EXCHANGE" >&2; exit 1;;
esac
i=0
for t in "${tables[@]}"; do
  c=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "SELECT count() FROM ${CH_DATABASE}.${t}" "$CH_URL/") || c="ERR"
  printf "%-32s %s\n" "$t" "$c"
  eval "TAB_$i='$t'"; eval "BASE_$i='$c'"; i=$((i+1))
done

echo
echo "== Step 3: Run collector ${DURATION}s (${EXCHANGE} spot+futures) =="
export CLICKHOUSE_USER="$CH_USER"
export CLICKHOUSE_PASSWORD="$CH_PASSWORD"

features="collector-${EXCHANGE}"
exnames="$EXCHANGE"
symbols_spot="$SYMBOL_SPOT"
symbols_fut="$SYMBOL_FUT"
case "$EXCHANGE" in
  bitget)
    exnames="bitget"
    features="collector-bitget"
    symbols="${SYMBOL_SPOT}:SPOT,${SYMBOL_FUT}:USDT-FUTURES"
    ;;
  binance)
    exnames="binance,binance_futures"
    features="collector-binance,collector-binance-futures"
    symbols="${SYMBOL_SPOT},${SYMBOL_FUT}"
    ;;
esac

set +e
(
  cargo run --release --features "$features" -- \
    --exchanges "$exnames" \
    --symbols "$symbols" \
    --ch-url "$CH_URL" \
    --database "$CH_DATABASE" \
    --batch-size 800 \
    --flush-ms 1000 &
  pid=$!
  sleep "$DURATION"
  kill $pid >/dev/null 2>&1 || true
  wait $pid >/dev/null 2>&1 || true
)
set -e

echo
echo "== Step 4: Post-run counts and sanity checks =="
now_ms=$(($(date +%s)*1000))
ok=1
for idx in $(seq 0 $((${#tables[@]}-1))); do
  eval t=\$TAB_$idx
  post=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "SELECT count() FROM ${CH_DATABASE}.${t}" "$CH_URL/") || post="ERR"
  eval base=\$BASE_$idx
  printf "%-32s %s -> %s\n" "$t" "$base" "$post"
  if [[ "$base" =~ ^[0-9]+$ && "$post" =~ ^[0-9]+$ ]]; then
    delta=$((post - base))
  else
    delta=-1
  fi
  echo "delta: $delta"

  # Recency lag check (<= 5 minutes)
  case "$t" in
    *_orderbook)
      # bitget/binance futures/spot: use exchange_ts (UInt64, ms) if present, else ts DateTime64(3)
      if [[ "$EXCHANGE" == "bitget" ]]; then
        qry="SELECT max(exchange_ts) FROM ${CH_DATABASE}.${t} WHERE symbol='${SYMBOL_SPOT}' OR symbol='${SYMBOL_FUT}'"
        max_ts=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "$qry" "$CH_URL/")
        [[ "$max_ts" =~ ^[0-9]+$ ]] || max_ts=0
        lag=$((now_ms - max_ts))
      else
        qry="SELECT toUInt64(max(toUnixTimestamp64Milli(ts))) FROM ${CH_DATABASE}.${t}"
        max_ts=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "$qry" "$CH_URL/")
        [[ "$max_ts" =~ ^[0-9]+$ ]] || max_ts=0
        lag=$((now_ms - max_ts))
      fi
      echo "max_ts_lag_ms: $lag"
      if (( lag > 300000 )); then ok=0; echo "[FAIL] recency lag too large for $t"; fi
      ;;
    *_trades)
      if [[ "$EXCHANGE" == "bitget" ]]; then
        qry="SELECT max(exchange_ts) FROM ${CH_DATABASE}.${t} WHERE symbol='${SYMBOL_SPOT}' OR symbol='${SYMBOL_FUT}'"
        max_ts=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "$qry" "$CH_URL/")
        [[ "$max_ts" =~ ^[0-9]+$ ]] || max_ts=0
        lag=$((now_ms - max_ts))
      else
        qry="SELECT toUInt64(max(toUnixTimestamp64Milli(ts))) FROM ${CH_DATABASE}.${t}"
        max_ts=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "$qry" "$CH_URL/")
        [[ "$max_ts" =~ ^[0-9]+$ ]] || max_ts=0
        lag=$((now_ms - max_ts))
      fi
      echo "max_ts_lag_ms: $lag"
      if (( lag > 300000 )); then ok=0; echo "[FAIL] recency lag too large for $t"; fi
      ;;
  esac

  # Value sanity checks
  case "$t" in
    *_orderbook)
      q="SELECT sum(price<=0)+sum(qty<0) FROM ${CH_DATABASE}.${t} WHERE (symbol='${SYMBOL_SPOT}' OR symbol='${SYMBOL_FUT}') LIMIT 1"
      bad=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "$q" "$CH_URL/")
      echo "bad_count(price<=0 or qty<0): $bad"
      [[ "$bad" =~ ^[0-9]+$ ]] && (( bad==0 )) || ok=0
      # side domain
      q2="SELECT countDistinct(side) FROM ${CH_DATABASE}.${t} WHERE (symbol='${SYMBOL_SPOT}' OR symbol='${SYMBOL_FUT}') AND side NOT IN ('bid','ask')"
      bad_side=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "$q2" "$CH_URL/")
      echo "bad_side_domain: $bad_side"
      [[ "$bad_side" =~ ^[0-9]+$ ]] && (( bad_side==0 )) || ok=0
      ;;
    *_trades)
      q="SELECT sum(price<=0)+sum(qty<=0) FROM ${CH_DATABASE}.${t} WHERE (symbol='${SYMBOL_SPOT}' OR symbol='${SYMBOL_FUT}') LIMIT 1"
      bad=$(curl -sS -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "$q" "$CH_URL/")
      echo "bad_count(price<=0 or qty<=0): $bad"
      [[ "$bad" =~ ^[0-9]+$ ]] && (( bad==0 )) || ok=0
      ;;
  esac
  echo
done

if (( ok==1 )); then
  echo "== RESULT: PASS (ingest correctness checks passed) =="
  exit 0
else
  echo "== RESULT: FAIL (see messages above) =="
  exit 2
fi
