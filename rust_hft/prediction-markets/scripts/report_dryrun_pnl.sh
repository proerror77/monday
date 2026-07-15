#!/usr/bin/env bash
set -euo pipefail

HOST="${PLOY_RESEARCH_HOST:-tango-1-1}"
SINCE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --since) SINCE="$2"; shift 2 ;;
    --host)  HOST="$2"; shift 2 ;;
    *) echo "Usage: $0 [--since YYYY-MM-DD] [--host HOST]"; exit 1 ;;
  esac
done

if [[ -n "$SINCE" ]]; then
  DATE_FILTER="AND first_fill_at >= '${SINCE}T00:00:00+08'"
  DAILY_FILTER="AND trading_day_cst >= '${SINCE}'"
else
  DATE_FILTER=""
  DAILY_FILTER=""
fi

run_sql() {
  # shellcheck disable=SC2029
  ssh "$HOST" "PGPASSWORD=postgres psql -U postgres -d ploy --no-align --tuples-only -c \"$1\"" 2>/dev/null
}

run_sql_table() {
  # shellcheck disable=SC2029
  ssh "$HOST" "PGPASSWORD=postgres psql -U postgres -d ploy -c \"$1\"" 2>&1 | grep -v "^perl:\|LANGUAGE\|LC_ALL\|LC_CTYPE\|LANG\|are supported"
}

echo "==================================================="
echo "  Dry-Run Strategy Report"
[[ -n "$SINCE" ]] && echo "  Since: $SINCE"
echo "  Host: $HOST"
echo "==================================================="
echo ""

echo "-- Summary ----------------------------------------"
run_sql_table "
SELECT
  COUNT(*) as total_trades,
  SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END) as wins,
  SUM(CASE WHEN is_closed AND net_pnl <= 0 THEN 1 ELSE 0 END) as losses,
  ROUND(
    CASE WHEN SUM(CASE WHEN is_closed THEN 1 ELSE 0 END) > 0
    THEN SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END)::numeric
         / SUM(CASE WHEN is_closed THEN 1 ELSE 0 END)::numeric * 100
    ELSE 0 END, 1
  ) as win_rate_pct,
  ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2) as realized_pnl,
  ROUND(SUM(CASE WHEN is_closed THEN total_fee ELSE 0 END)::numeric, 2) as total_fees,
  SUM(CASE WHEN NOT is_closed THEN 1 ELSE 0 END) as open_positions,
  ROUND(SUM(CASE WHEN NOT is_closed THEN buy_notional ELSE 0 END)::numeric, 2) as open_exposure
FROM strategy_runtime_event_track_record
WHERE runtime_mode = 'dry_run' $DATE_FILTER;
"

echo ""
echo "-- By Window --------------------------------------"
run_sql_table "
WITH events AS (
  SELECT
    t.*,
    CASE
      WHEN m.end_time IS NOT NULL AND m.start_time IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (m.end_time - m.start_time)))::int
      WHEN m.market_slug ILIKE '%15m%' OR m.market_slug ILIKE '%15-minute%' THEN 900
      WHEN m.market_slug ILIKE '%5m%' OR m.market_slug ILIKE '%5-minute%' THEN 300
      ELSE NULL
    END AS window_secs,
    CASE
      WHEN m.end_time IS NOT NULL AND t.opened_at IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (m.end_time - t.opened_at)))::int
      ELSE NULL
    END AS entry_ttr_secs
  FROM strategy_runtime_event_track_record t
  LEFT JOIN pm_market_metadata m ON m.market_slug = t.event_id
  WHERE t.runtime_mode = 'dry_run' $DATE_FILTER
)
SELECT
  CASE
    WHEN window_secs = 300 THEN '5m'
    WHEN window_secs = 900 THEN '15m'
    WHEN window_secs IS NULL THEN 'unknown'
    ELSE window_secs::text || 's'
  END as window,
  COUNT(*) as trades,
  SUM(CASE WHEN is_closed THEN 1 ELSE 0 END) as closed,
  SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END) as wins,
  SUM(CASE WHEN is_closed AND net_pnl <= 0 THEN 1 ELSE 0 END) as losses,
  ROUND(
    CASE WHEN SUM(CASE WHEN is_closed THEN 1 ELSE 0 END) > 0
    THEN SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END)::numeric
         / SUM(CASE WHEN is_closed THEN 1 ELSE 0 END)::numeric * 100
    ELSE 0 END, 1
  ) as win_rate_pct,
  ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2) as realized_pnl,
  MIN(entry_ttr_secs) as min_entry_ttr,
  MAX(entry_ttr_secs) as max_entry_ttr
FROM events
GROUP BY window_secs
ORDER BY window_secs;
"

echo ""
echo "-- Daily ------------------------------------------"
run_sql_table "
SELECT
  trading_day_cst as day,
  trade_count as trades,
  closed_trade_count as closed,
  winning_trade_count_all as wins,
  losing_trade_count_all as losses,
  confirmed_trade_count as confirmed,
  ROUND(net_pnl::numeric, 2) as net_pnl,
  ROUND(confirmed_net_pnl::numeric, 2) as confirmed_pnl,
  ROUND(total_fee::numeric, 2) as fees,
  residual_open_quantity as open_qty
FROM strategy_runtime_daily_track_record
WHERE runtime_mode = 'dry_run' $DAILY_FILTER
ORDER BY trading_day_cst DESC;
"

echo ""
echo "-- Daily by Window -------------------------------"
run_sql_table "
WITH events AS (
  SELECT
    t.*,
    CASE
      WHEN m.end_time IS NOT NULL AND m.start_time IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (m.end_time - m.start_time)))::int
      WHEN m.market_slug ILIKE '%15m%' OR m.market_slug ILIKE '%15-minute%' THEN 900
      WHEN m.market_slug ILIKE '%5m%' OR m.market_slug ILIKE '%5-minute%' THEN 300
      ELSE NULL
    END AS window_secs
  FROM strategy_runtime_event_track_record t
  LEFT JOIN pm_market_metadata m ON m.market_slug = t.event_id
  WHERE t.runtime_mode = 'dry_run' $DATE_FILTER
)
SELECT
  (COALESCE(closed_at, last_fill_at) AT TIME ZONE 'Asia/Shanghai')::date as day,
  CASE
    WHEN window_secs = 300 THEN '5m'
    WHEN window_secs = 900 THEN '15m'
    WHEN window_secs IS NULL THEN 'unknown'
    ELSE window_secs::text || 's'
  END as window,
  COUNT(*) as trades,
  SUM(CASE WHEN is_closed THEN 1 ELSE 0 END) as closed,
  ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2) as net_pnl
FROM events
GROUP BY day, window_secs
ORDER BY day DESC, window_secs;
"

echo ""
echo "-- Recent Trades (last 20 closed) ----------------"
run_sql_table "
WITH events AS (
  SELECT
    t.*,
    CASE
      WHEN m.end_time IS NOT NULL AND m.start_time IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (m.end_time - m.start_time)))::int
      WHEN m.market_slug ILIKE '%15m%' OR m.market_slug ILIKE '%15-minute%' THEN 900
      WHEN m.market_slug ILIKE '%5m%' OR m.market_slug ILIKE '%5-minute%' THEN 300
      ELSE NULL
    END AS window_secs,
    CASE
      WHEN m.end_time IS NOT NULL AND t.opened_at IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (m.end_time - t.opened_at)))::int
      ELSE NULL
    END AS entry_ttr_secs
  FROM strategy_runtime_event_track_record t
  LEFT JOIN pm_market_metadata m ON m.market_slug = t.event_id
  WHERE t.runtime_mode = 'dry_run' AND t.is_closed $DATE_FILTER
)
SELECT
  symbol,
  CASE
    WHEN window_secs = 300 THEN '5m'
    WHEN window_secs = 900 THEN '15m'
    WHEN window_secs IS NULL THEN 'unknown'
    ELSE window_secs::text || 's'
  END as window,
  market_side as side,
  ROUND(avg_entry_price::numeric, 4) as entry,
  ROUND(avg_exit_price::numeric, 4) as exit_px,
  CASE
    WHEN avg_exit_price >= 0.99 THEN 'WIN'
    WHEN avg_exit_price <= 0.01 THEN 'LOSS'
    ELSE 'TP/SL'
  END as exit_type,
  ROUND(buy_quantity::numeric, 2) as qty,
  ROUND(net_pnl::numeric, 2) as net_pnl,
  entry_ttr_secs as entry_ttr,
  to_char(opened_at AT TIME ZONE 'Asia/Shanghai', 'MM-DD HH24:MI') as opened,
  to_char(closed_at AT TIME ZONE 'Asia/Shanghai', 'MM-DD HH24:MI') as closed
FROM events
ORDER BY closed_at DESC
LIMIT 20;
"

echo ""
echo "-- Per Symbol by Window --------------------------"
run_sql_table "
WITH events AS (
  SELECT
    t.*,
    CASE
      WHEN m.end_time IS NOT NULL AND m.start_time IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (m.end_time - m.start_time)))::int
      WHEN m.market_slug ILIKE '%15m%' OR m.market_slug ILIKE '%15-minute%' THEN 900
      WHEN m.market_slug ILIKE '%5m%' OR m.market_slug ILIKE '%5-minute%' THEN 300
      ELSE NULL
    END AS window_secs
  FROM strategy_runtime_event_track_record t
  LEFT JOIN pm_market_metadata m ON m.market_slug = t.event_id
  WHERE t.runtime_mode = 'dry_run' $DATE_FILTER
)
SELECT
  symbol,
  CASE
    WHEN window_secs = 300 THEN '5m'
    WHEN window_secs = 900 THEN '15m'
    WHEN window_secs IS NULL THEN 'unknown'
    ELSE window_secs::text || 's'
  END as window,
  COUNT(*) as trades,
  SUM(CASE WHEN is_closed AND net_pnl > 0 THEN 1 ELSE 0 END) as wins,
  SUM(CASE WHEN is_closed AND net_pnl <= 0 THEN 1 ELSE 0 END) as losses,
  ROUND(SUM(CASE WHEN is_closed THEN net_pnl ELSE 0 END)::numeric, 2) as net_pnl,
  ROUND(AVG(CASE WHEN is_closed THEN avg_entry_price END)::numeric, 4) as avg_entry
FROM events
GROUP BY symbol, window_secs
ORDER BY window_secs, net_pnl DESC;
"

echo ""
echo "-- Pairing Check ---------------------------------"
run_sql_table "
SELECT
  COUNT(*) as mixed_event_groups,
  COALESCE(SUM(fill_count), 0) as fills_in_mixed_groups
FROM (
  SELECT runtime_mode, strategy_id, deployment_id, event_id, COUNT(*) as fill_count
  FROM strategy_runtime_fills
  WHERE runtime_mode = 'dry_run'
    AND event_id IS NOT NULL
    AND event_id <> ''
  GROUP BY runtime_mode, strategy_id, deployment_id, event_id
  HAVING COUNT(DISTINCT token_id) > 1 OR COUNT(DISTINCT market_side) > 1
) mixed;
"
