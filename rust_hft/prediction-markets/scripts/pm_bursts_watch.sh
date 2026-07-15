#!/usr/bin/env bash
set -euo pipefail

# Watch rolling-window volume bursts from `clob_trade_ticks`.
#
# Examples:
#   WINDOW_SECS=60 WATCH_SECS=1 ./scripts/pm_bursts_watch.sh
#   WINDOW_SECS=300 MIN_NOTIONAL=500 ./scripts/pm_bursts_watch.sh

DB_NAME="${PLOY_DB_NAME:-ploy}"
WATCH_SECS="${WATCH_SECS:-1}"
WINDOW_SECS="${WINDOW_SECS:-60}"
MIN_NOTIONAL="${MIN_NOTIONAL:-0}"
LIMIT_ROWS="${LIMIT_ROWS:-25}"

if [[ -n "${DATABASE_URL:-}" ]]; then
  PSQL=(psql "$DATABASE_URL" -v ON_ERROR_STOP=1)
elif command -v runuser >/dev/null 2>&1; then
  PSQL=(runuser -u postgres -- psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
else
  PSQL=(psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
fi

min_clause=""
if [[ "$MIN_NOTIONAL" != "0" ]]; then
  min_clause="HAVING SUM(size * price) >= ${MIN_NOTIONAL}"
fi

"${PSQL[@]}" <<SQL
\\pset pager off
SELECT
  token_id,
  condition_id,
  COUNT(*) AS n_trades,
  SUM(size) AS sum_size,
  SUM(size * price) AS sum_notional,
  MAX(trade_ts) AS last_trade_ts
FROM clob_trade_ticks
WHERE trade_ts > now() - interval '${WINDOW_SECS} seconds'
GROUP BY token_id, condition_id
${min_clause}
ORDER BY sum_notional DESC
LIMIT ${LIMIT_ROWS};
\\watch ${WATCH_SECS}
SQL
