#!/usr/bin/env bash
set -euo pipefail

# Minimal schema checker (no create), for existing ClickHouse tables.
# Usage:
#   CH_URL=... CH_USER=... CH_PASSWORD=... CH_DATABASE=hft_db ./check_schema.sh [exchange1,exchange2,...]

CH_URL=${CH_URL:-"https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"}
CH_USER=${CH_USER:-"default"}
CH_PASSWORD=${CH_PASSWORD:-""}
CH_DATABASE=${CH_DATABASE:-"hft_db"}

EXCHANGES_CSV=${1:-"binance,bitget,okx,bybit,hyperliquid"}

EXCHANGES=()
while IFS= read -r line; do
  line=$(echo "$line" | sed 's/^\s*//; s/\s*$//' | tr 'A-Z' 'a-z')
  [ -n "$line" ] && EXCHANGES+=("$line")
done < <(echo "$EXCHANGES_CSV" | tr ',' '\n')
# uniq
EXCHANGES=($(printf "%s\n" "${EXCHANGES[@]}" | sort -u))

declare -a TABLES=(snapshot_books)
for ex in "${EXCHANGES[@]}"; do
  case "$ex" in
    binance)
      TABLES+=(binance_orderbook binance_trades binance_l1 binance_ticker)
      ;;
    binance_futures|binance-futures)
      TABLES+=(binance_futures_orderbook binance_futures_trades binance_futures_l1 binance_futures_ticker binance_futures_snapshot_books)
      ;;
    bitget)
      TABLES+=(bitget_orderbook bitget_trades bitget_l1 bitget_ticker)
      ;;
    asterdex)
      TABLES+=(asterdex_orderbook asterdex_trades asterdex_l1 asterdex_ticker snapshot_books)
      ;;
    okx)
      TABLES+=(okx_orderbook okx_trades okx_l1 okx_ticker)
      ;;
    bybit)
      TABLES+=(bybit_orderbook bybit_trades bybit_l1 bybit_ticker)
      ;;
    hyperliquid)
      TABLES+=(hyperliquid_orderbook hyperliquid_trades hyperliquid_l1)
      ;;
  esac
done

echo "Checking tables in ${CH_DATABASE} on ${CH_URL}" >&2
curl_flags=( -sS )
if [ "${CH_INSECURE:-0}" = "1" ]; then curl_flags+=( -k ); fi
RESP=$(curl "${curl_flags[@]}" -u "$CH_USER:$CH_PASSWORD" -H 'Content-Type: text/plain' --data-binary "SHOW TABLES FROM ${CH_DATABASE}" "$CH_URL/")
missing=()
for t in "${TABLES[@]}"; do
  if ! echo "$RESP" | awk '{print $1}' | grep -qx "$t"; then
    missing+=("$t")
  fi
done

if ((${#missing[@]} > 0)); then
  echo "Missing tables (please create before running collector):" >&2
  for t in "${missing[@]}"; do echo "  - ${CH_DATABASE}.${t}"; done
  exit 2
else
  echo "All required tables exist." >&2
fi
