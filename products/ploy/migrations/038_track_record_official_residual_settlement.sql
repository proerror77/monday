-- Migration: 038_track_record_official_residual_settlement
-- Description: Account for official settlement on residual positions even when
--              live mode skipped synthetic settle_* exits.

DROP VIEW IF EXISTS strategy_runtime_daily_track_record;
DROP VIEW IF EXISTS strategy_runtime_event_track_record;

CREATE VIEW strategy_runtime_event_track_record AS
WITH settlement_prices AS (
    SELECT
        token_id,
        CASE
            WHEN settled_price >= 0.99 THEN 1.0::NUMERIC
            WHEN settled_price <= 0.01 THEN 0.0::NUMERIC
            ELSE settled_price
        END AS official_price,
        resolved_at,
        fetched_at
    FROM pm_token_settlements
    WHERE resolved = true
      AND settled_price IS NOT NULL
),
normalized_fills AS (
    SELECT
        f.runtime_mode,
        f.strategy_id,
        f.deployment_id,
        COALESCE(NULLIF(f.event_id, ''), f.intent_id) AS trade_key,
        f.event_id,
        f.intent_id,
        f.symbol,
        f.token_id,
        f.market_side,
        f.fill_side,
        f.quantity,
        f.price AS recorded_price,
        sp.official_price AS official_settlement_price,
        COALESCE(sp.resolved_at, sp.fetched_at) AS official_settlement_at,
        f.fill_side = 'SELL' AND f.intent_id LIKE '%settle_%' AS is_settlement_exit,
        CASE
            WHEN f.fill_side = 'SELL'
                AND f.intent_id LIKE '%settle_%'
                AND sp.official_price IS NOT NULL
            THEN sp.official_price
            ELSE f.price
        END AS effective_price,
        CASE
            WHEN f.fill_side = 'SELL'
                AND f.intent_id LIKE '%settle_%'
                AND sp.official_price IS NOT NULL
                AND ABS(f.price - sp.official_price) > 0.00000001::NUMERIC
            THEN true
            ELSE false
        END AS settlement_exit_corrected,
        f.fee,
        f.fill_timestamp
    FROM strategy_runtime_fills f
    LEFT JOIN settlement_prices sp ON sp.token_id = f.token_id
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
        BOOL_OR(settlement_exit_corrected) AS settlement_exit_corrected,
        MAX(official_settlement_price) AS official_settlement_price,
        MAX(official_settlement_at) AS official_settlement_at,
        MIN(fill_timestamp) AS first_fill_at,
        MAX(fill_timestamp) AS last_fill_at,
        MIN(fill_timestamp) FILTER (WHERE fill_side = 'BUY') AS opened_at,
        MAX(fill_timestamp) FILTER (WHERE fill_side = 'SELL') AS recorded_closed_at,
        COUNT(*) AS fill_count,
        COUNT(*) FILTER (WHERE fill_side = 'BUY') AS buy_fill_count,
        COUNT(*) FILTER (WHERE fill_side = 'SELL') AS sell_fill_count,
        COALESCE(SUM(quantity) FILTER (WHERE fill_side = 'BUY'), 0::NUMERIC) AS buy_quantity,
        COALESCE(
            SUM(quantity) FILTER (WHERE fill_side = 'SELL' AND NOT is_settlement_exit),
            0::NUMERIC
        ) AS market_sell_quantity,
        COALESCE(
            SUM(quantity) FILTER (WHERE fill_side = 'SELL' AND is_settlement_exit),
            0::NUMERIC
        ) AS settlement_exit_quantity,
        COALESCE(
            SUM(quantity * recorded_price) FILTER (WHERE fill_side = 'SELL'),
            0::NUMERIC
        ) AS recorded_sell_notional,
        COALESCE(
            SUM(quantity * effective_price) FILTER (WHERE fill_side = 'BUY'),
            0::NUMERIC
        ) AS buy_notional,
        COALESCE(
            SUM(quantity * effective_price)
                FILTER (WHERE fill_side = 'SELL' AND NOT is_settlement_exit),
            0::NUMERIC
        ) AS market_sell_notional,
        COALESCE(
            SUM(quantity * effective_price)
                FILTER (WHERE fill_side = 'SELL' AND is_settlement_exit),
            0::NUMERIC
        ) AS settlement_exit_notional,
        COALESCE(SUM(fee), 0::NUMERIC) AS total_fee
    FROM normalized_fills
    GROUP BY runtime_mode, strategy_id, deployment_id, trade_key
),
computed AS (
    SELECT
        a.*,
        CASE
            WHEN a.official_settlement_price IS NOT NULL
             AND a.buy_quantity > a.market_sell_quantity + a.settlement_exit_quantity
            THEN a.buy_quantity - a.market_sell_quantity - a.settlement_exit_quantity
            ELSE 0::NUMERIC
        END AS official_residual_quantity
    FROM aggregated a
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
    CASE
        WHEN official_residual_quantity > 0 THEN COALESCE(official_settlement_at, recorded_closed_at)
        ELSE recorded_closed_at
    END AS closed_at,
    fill_count,
    buy_fill_count,
    sell_fill_count,
    buy_quantity,
    market_sell_quantity + settlement_exit_quantity + official_residual_quantity AS sell_quantity,
    market_sell_quantity + settlement_exit_quantity AS recorded_sell_quantity,
    settlement_exit_quantity,
    official_residual_quantity,
    buy_notional,
    market_sell_notional
        + settlement_exit_notional
        + official_residual_quantity * COALESCE(official_settlement_price, 0::NUMERIC)
        AS sell_notional,
    recorded_sell_notional,
    total_fee,
    CASE
        WHEN buy_quantity > 0 THEN buy_notional / buy_quantity
        ELSE NULL
    END AS avg_entry_price,
    CASE
        WHEN market_sell_quantity + settlement_exit_quantity > 0
        THEN recorded_sell_notional / (market_sell_quantity + settlement_exit_quantity)
        ELSE NULL
    END AS recorded_exit_price,
    CASE
        WHEN settlement_exit_quantity > 0 OR official_residual_quantity > 0
        THEN official_settlement_price
        ELSE NULL
    END AS official_exit_price,
    CASE
        WHEN market_sell_quantity + settlement_exit_quantity + official_residual_quantity > 0
        THEN (
            market_sell_notional
            + settlement_exit_notional
            + official_residual_quantity * COALESCE(official_settlement_price, 0::NUMERIC)
        ) / (market_sell_quantity + settlement_exit_quantity + official_residual_quantity)
        ELSE NULL
    END AS avg_exit_price,
    settlement_exit_corrected OR official_residual_quantity > 0 AS settlement_corrected,
    buy_quantity > 0
        AND market_sell_quantity + settlement_exit_quantity + official_residual_quantity > 0
        AND ABS(buy_quantity - (
            market_sell_quantity + settlement_exit_quantity + official_residual_quantity
        )) <= 0.00000001::NUMERIC AS is_confirmed,
    (
        market_sell_notional
        + settlement_exit_notional
        + official_residual_quantity * COALESCE(official_settlement_price, 0::NUMERIC)
    ) - buy_notional AS gross_pnl,
    (
        market_sell_notional
        + settlement_exit_notional
        + official_residual_quantity * COALESCE(official_settlement_price, 0::NUMERIC)
    ) - buy_notional - total_fee AS net_pnl,
    buy_quantity > 0
        AND market_sell_quantity + settlement_exit_quantity + official_residual_quantity > 0
        AND ABS(buy_quantity - (
            market_sell_quantity + settlement_exit_quantity + official_residual_quantity
        )) <= 0.00000001::NUMERIC AS is_closed,
    CASE
        WHEN buy_quantity > market_sell_quantity + settlement_exit_quantity + official_residual_quantity
        THEN buy_quantity - market_sell_quantity - settlement_exit_quantity - official_residual_quantity
        ELSE 0::NUMERIC
    END AS open_quantity
FROM computed;

CREATE VIEW strategy_runtime_daily_track_record AS
SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    (COALESCE(closed_at, last_fill_at) AT TIME ZONE 'Asia/Shanghai')::date AS trading_day_cst,
    COUNT(*) AS trade_count,
    COUNT(*) FILTER (WHERE is_closed) AS closed_trade_count,
    COUNT(*) FILTER (WHERE is_closed AND is_confirmed) AS confirmed_trade_count,
    COUNT(*) FILTER (WHERE is_closed AND settlement_corrected) AS corrected_trade_count,
    COUNT(*) FILTER (WHERE is_closed AND is_confirmed AND net_pnl > 0) AS winning_trade_count,
    COUNT(*) FILTER (WHERE is_closed AND is_confirmed AND net_pnl < 0) AS losing_trade_count,
    COUNT(*) FILTER (WHERE is_closed AND net_pnl > 0) AS winning_trade_count_all,
    COUNT(*) FILTER (WHERE is_closed AND net_pnl < 0) AS losing_trade_count_all,
    COALESCE(SUM(buy_notional), 0::NUMERIC) AS total_buy_notional,
    COALESCE(SUM(sell_notional), 0::NUMERIC) AS total_sell_notional,
    COALESCE(SUM(recorded_sell_notional), 0::NUMERIC) AS total_recorded_sell_notional,
    COALESCE(SUM(total_fee), 0::NUMERIC) AS total_fee,
    COALESCE(SUM(gross_pnl), 0::NUMERIC) AS gross_pnl,
    COALESCE(SUM(net_pnl), 0::NUMERIC) AS net_pnl,
    COALESCE(AVG(net_pnl), 0::NUMERIC) AS avg_net_pnl,
    COALESCE(SUM(net_pnl) FILTER (WHERE is_confirmed), 0::NUMERIC) AS confirmed_net_pnl,
    COALESCE(AVG(net_pnl) FILTER (WHERE is_confirmed), 0::NUMERIC) AS confirmed_avg_net_pnl,
    COALESCE(SUM(open_quantity), 0::NUMERIC) AS residual_open_quantity
FROM strategy_runtime_event_track_record
GROUP BY
    runtime_mode,
    strategy_id,
    deployment_id,
    (COALESCE(closed_at, last_fill_at) AT TIME ZONE 'Asia/Shanghai')::date;
