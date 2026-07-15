#!/usr/bin/env bash
set -euo pipefail

# Watch tick-by-tick Polymarket trades captured in `clob_trade_ticks`.
#
# Examples:
#   WINDOW_MINUTES=5 WATCH_SECS=1 ./scripts/pm_trades_watch.sh
#   TOKEN_ID=123 WINDOW_MINUTES=30 MIN_NOTIONAL=500 ./scripts/pm_trades_watch.sh
#   CONDITION_ID=0xabc... MIN_SIZE=1000 ./scripts/pm_trades_watch.sh

DB_NAME="${PLOY_DB_NAME:-ploy}"
WATCH_SECS="${WATCH_SECS:-1}"
WINDOW_MINUTES="${WINDOW_MINUTES:-10}"
TOKEN_ID="${TOKEN_ID:-}"
CONDITION_ID="${CONDITION_ID:-}"
MIN_SIZE="${MIN_SIZE:-0}"
MIN_NOTIONAL="${MIN_NOTIONAL:-0}"

if [[ -n "${DATABASE_URL:-}" ]]; then
  PSQL=(psql "$DATABASE_URL" -v ON_ERROR_STOP=1)
elif command -v runuser >/dev/null 2>&1; then
  PSQL=(runuser -u postgres -- psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
else
  PSQL=(psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
fi

where="trade_ts > now() - interval '${WINDOW_MINUTES} minutes'"
if [[ -n "$TOKEN_ID" ]]; then
  where="${where} AND token_id = '${TOKEN_ID}'"
fi
if [[ -n "$CONDITION_ID" ]]; then
  where="${where} AND condition_id = '${CONDITION_ID}'"
fi
if [[ "$MIN_SIZE" != "0" ]]; then
  where="${where} AND size >= ${MIN_SIZE}"
fi
if [[ "$MIN_NOTIONAL" != "0" ]]; then
  where="${where} AND (size * price) >= ${MIN_NOTIONAL}"
fi

"${PSQL[@]}" <<SQL
\\pset pager off
SELECT
  trade_ts,
  condition_id,
  token_id,
  side,
  price,
  size,
  (size * price) AS notional,
  transaction_hash
FROM clob_trade_ticks
WHERE ${where}
ORDER BY trade_ts DESC
LIMIT 50;
\\watch ${WATCH_SECS}
SQL
