#!/usr/bin/env bash
set -euo pipefail

# Watch large-trade + burst alerts written to `clob_trade_alerts`.
#
# Enable alerts via env vars on the running service, e.g.:
#   PM_TRADE_ALERT_MIN_NOTIONAL=500
#   PM_TRADE_BURST_MIN_NOTIONAL=2000
#   PM_TRADE_BURST_WINDOW_SECS=60
#   PM_TRADE_BURST_COOLDOWN_SECS=60
#
# Then tail alerts:
#   WATCH_SECS=1 ./scripts/pm_trade_alerts_watch.sh
#   TOKEN_ID=123 WINDOW_MINUTES=60 ./scripts/pm_trade_alerts_watch.sh

DB_NAME="${PLOY_DB_NAME:-ploy}"
WATCH_SECS="${WATCH_SECS:-1}"
WINDOW_MINUTES="${WINDOW_MINUTES:-60}"
TOKEN_ID="${TOKEN_ID:-}"
CONDITION_ID="${CONDITION_ID:-}"

if [[ -n "${DATABASE_URL:-}" ]]; then
  PSQL=(psql "$DATABASE_URL" -v ON_ERROR_STOP=1)
elif command -v runuser >/dev/null 2>&1; then
  PSQL=(runuser -u postgres -- psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
else
  PSQL=(psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
fi

where="created_at > now() - interval '${WINDOW_MINUTES} minutes'"
if [[ -n "$TOKEN_ID" ]]; then
  where="${where} AND token_id = '${TOKEN_ID}'"
fi
if [[ -n "$CONDITION_ID" ]]; then
  where="${where} AND condition_id = '${CONDITION_ID}'"
fi

"${PSQL[@]}" <<SQL
\\pset pager off
SELECT
  created_at,
  alert_type,
  condition_id,
  token_id,
  side,
  size,
  notional,
  trade_ts,
  window_start,
  window_end,
  transaction_hash,
  burst_bucket_unix
FROM clob_trade_alerts
WHERE ${where}
ORDER BY created_at DESC
LIMIT 50;
\\watch ${WATCH_SECS}
SQL
