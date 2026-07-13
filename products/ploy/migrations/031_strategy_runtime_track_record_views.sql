-- Migration: 031_strategy_runtime_track_record_views
-- Description: Add readable track-record views on top of strategy runtime fill/order audits.

-- Drop existing views to allow column changes (CASCADE handles dependent views).
DROP VIEW IF EXISTS strategy_runtime_daily_track_record CASCADE;
DROP VIEW IF EXISTS strategy_runtime_event_track_record CASCADE;

CREATE OR REPLACE VIEW strategy_runtime_event_track_record AS
WITH normalized_fills AS (
    SELECT
        runtime_mode,
        strategy_id,
        deployment_id,
        COALESCE(NULLIF(event_id, ''), intent_id) AS trade_key,
        event_id,
        intent_id,
        symbol,
        token_id,
        market_side,
        fill_side,
        quantity,
        price,
        fee,
        fill_timestamp
    FROM strategy_runtime_fills
),
aggregated AS (
    SELECT
        runtime_mode,
        strategy_id,
        deployment_id,
        trade_key,
        MAX(event_id) AS event_id,
        MIN(intent_id) AS intent_id,
        MAX(symbol) AS symbol,
        MAX(token_id) AS token_id,
        MAX(market_side) AS market_side,
        MIN(fill_timestamp) AS first_fill_at,
        MAX(fill_timestamp) AS last_fill_at,
        MIN(fill_timestamp) FILTER (WHERE fill_side = 'BUY') AS opened_at,
        MAX(fill_timestamp) FILTER (WHERE fill_side = 'SELL') AS closed_at,
        COUNT(*) AS fill_count,
        COUNT(*) FILTER (WHERE fill_side = 'BUY') AS buy_fill_count,
        COUNT(*) FILTER (WHERE fill_side = 'SELL') AS sell_fill_count,
        COALESCE(SUM(quantity) FILTER (WHERE fill_side = 'BUY'), 0::NUMERIC) AS buy_quantity,
        COALESCE(SUM(quantity) FILTER (WHERE fill_side = 'SELL'), 0::NUMERIC) AS sell_quantity,
        COALESCE(SUM(quantity * price) FILTER (WHERE fill_side = 'BUY'), 0::NUMERIC) AS buy_notional,
        COALESCE(SUM(quantity * price) FILTER (WHERE fill_side = 'SELL'), 0::NUMERIC) AS sell_notional,
        COALESCE(SUM(fee), 0::NUMERIC) AS total_fee
    FROM normalized_fills
    GROUP BY runtime_mode, strategy_id, deployment_id, trade_key
)
SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    trade_key,
    event_id,
    intent_id,
    symbol,
    token_id,
    market_side,
    first_fill_at,
    last_fill_at,
    opened_at,
    closed_at,
    fill_count,
    buy_fill_count,
    sell_fill_count,
    buy_quantity,
    sell_quantity,
    buy_notional,
    sell_notional,
    total_fee,
    CASE
        WHEN buy_quantity > 0 THEN buy_notional / buy_quantity
        ELSE NULL
    END AS avg_entry_price,
    CASE
        WHEN sell_quantity > 0 THEN sell_notional / sell_quantity
        ELSE NULL
    END AS avg_exit_price,
    sell_notional - buy_notional AS gross_pnl,
    sell_notional - buy_notional - total_fee AS net_pnl,
    buy_quantity > 0
        AND sell_quantity > 0
        AND ABS(buy_quantity - sell_quantity) <= 0.00000001::NUMERIC AS is_closed,
    CASE
        WHEN buy_quantity > sell_quantity THEN buy_quantity - sell_quantity
        ELSE 0::NUMERIC
    END AS open_quantity
FROM aggregated;

CREATE OR REPLACE VIEW strategy_runtime_daily_track_record AS
SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    (COALESCE(closed_at, last_fill_at) AT TIME ZONE 'Asia/Shanghai')::date AS trading_day_cst,
    COUNT(*) AS trade_count,
    COUNT(*) FILTER (WHERE is_closed) AS closed_trade_count,
    COUNT(*) FILTER (WHERE is_closed AND net_pnl > 0) AS winning_trade_count,
    COUNT(*) FILTER (WHERE is_closed AND net_pnl < 0) AS losing_trade_count,
    COALESCE(SUM(buy_notional), 0::NUMERIC) AS total_buy_notional,
    COALESCE(SUM(sell_notional), 0::NUMERIC) AS total_sell_notional,
    COALESCE(SUM(total_fee), 0::NUMERIC) AS total_fee,
    COALESCE(SUM(gross_pnl), 0::NUMERIC) AS gross_pnl,
    COALESCE(SUM(net_pnl), 0::NUMERIC) AS net_pnl,
    COALESCE(AVG(net_pnl), 0::NUMERIC) AS avg_net_pnl,
    COALESCE(SUM(open_quantity), 0::NUMERIC) AS residual_open_quantity
FROM strategy_runtime_event_track_record
GROUP BY
    runtime_mode,
    strategy_id,
    deployment_id,
    (COALESCE(closed_at, last_fill_at) AT TIME ZONE 'Asia/Shanghai')::date;
