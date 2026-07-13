-- Migration 018: Formalize runtime-created training data tables
--
-- These 7 tables were previously created at runtime via CREATE TABLE IF NOT EXISTS
-- in Rust code (bootstrap.rs, sync_collector.rs, polymarket_orderbook_history.rs).
-- Formalizing them in a migration ensures new environments have all tables before
-- the application starts, and makes schema changes trackable.
--
-- Also adds a backfill function to populate pm_market_metadata from
-- pm_token_settlements.raw_market JSONB for historical data recovery.

-- ============================================================
-- 1. binance_price_ticks — Binance aggTrade stream
-- ============================================================
CREATE TABLE IF NOT EXISTS binance_price_ticks (
    id BIGSERIAL PRIMARY KEY,
    symbol TEXT NOT NULL,
    price NUMERIC(20,10) NOT NULL,
    quantity NUMERIC(20,10),
    trade_time TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_binance_price_ticks_symbol_time
    ON binance_price_ticks(symbol, trade_time DESC);
CREATE INDEX IF NOT EXISTS idx_binance_price_ticks_time
    ON binance_price_ticks(trade_time DESC);

-- ============================================================
-- 2. binance_lob_ticks — Binance depth@100ms L2 snapshots
-- ============================================================
CREATE TABLE IF NOT EXISTS binance_lob_ticks (
    id BIGSERIAL PRIMARY KEY,
    symbol TEXT NOT NULL,
    update_id BIGINT,
    best_bid NUMERIC(20,10) NOT NULL,
    best_ask NUMERIC(20,10) NOT NULL,
    mid_price NUMERIC(20,10) NOT NULL,
    spread_bps NUMERIC(12,6) NOT NULL,
    obi_5 NUMERIC(12,8) NOT NULL,
    obi_10 NUMERIC(12,8) NOT NULL,
    bid_volume_5 NUMERIC(20,10) NOT NULL,
    ask_volume_5 NUMERIC(20,10) NOT NULL,
    bids JSONB,
    asks JSONB,
    event_time TIMESTAMPTZ NOT NULL,
    source TEXT NOT NULL DEFAULT 'binance_depth_ws',
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_binance_lob_ticks_symbol_time
    ON binance_lob_ticks(symbol, event_time DESC);
CREATE INDEX IF NOT EXISTS idx_binance_lob_ticks_time
    ON binance_lob_ticks(event_time DESC);

-- ============================================================
-- 3. clob_orderbook_snapshots — Polymarket WS book updates
-- ============================================================
CREATE TABLE IF NOT EXISTS clob_orderbook_snapshots (
    id BIGSERIAL PRIMARY KEY,
    domain TEXT,
    token_id TEXT NOT NULL,
    market TEXT,
    bids JSONB NOT NULL,
    asks JSONB NOT NULL,
    book_timestamp TIMESTAMPTZ,
    hash TEXT,
    source TEXT NOT NULL DEFAULT 'polymarket_ws',
    context JSONB,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_token_time
    ON clob_orderbook_snapshots(token_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_time
    ON clob_orderbook_snapshots(received_at DESC);
CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_domain_time
    ON clob_orderbook_snapshots(domain, received_at DESC);

-- ============================================================
-- 4. clob_trade_ticks — Polymarket trade data from Data API
-- ============================================================
CREATE TABLE IF NOT EXISTS clob_trade_ticks (
    id BIGSERIAL PRIMARY KEY,
    domain TEXT,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('BUY','SELL')),
    size NUMERIC(20,10) NOT NULL,
    price NUMERIC(10,6) NOT NULL,
    trade_ts TIMESTAMPTZ NOT NULL,
    trade_ts_unix BIGINT NOT NULL,
    transaction_hash TEXT NOT NULL,
    proxy_wallet TEXT,
    title TEXT,
    slug TEXT,
    outcome TEXT,
    outcome_index INTEGER,
    source TEXT NOT NULL DEFAULT 'polymarket_data_api',
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (transaction_hash, token_id, side, size, price, trade_ts_unix)
);

CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_token_time
    ON clob_trade_ticks(token_id, trade_ts DESC);
CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_market_time
    ON clob_trade_ticks(condition_id, trade_ts DESC);
CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_time
    ON clob_trade_ticks(trade_ts DESC);

-- ============================================================
-- 5. clob_orderbook_history_ticks — REST API backfill
-- ============================================================
CREATE TABLE IF NOT EXISTS clob_orderbook_history_ticks (
    id BIGSERIAL PRIMARY KEY,
    token_id TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    book_ts_ms BIGINT NOT NULL,
    book_ts TIMESTAMPTZ NOT NULL,
    hash TEXT NOT NULL,
    bids JSONB NOT NULL,
    asks JSONB NOT NULL,
    source TEXT NOT NULL DEFAULT 'polymarket_orderbook_history',
    collected_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS uniq_clob_orderbook_history_ticks
    ON clob_orderbook_history_ticks(token_id, book_ts_ms, hash);

-- ============================================================
-- 6. sync_records — 1s aligned Binance+Polymarket snapshots
-- ============================================================
CREATE TABLE IF NOT EXISTS sync_records (
    id BIGSERIAL PRIMARY KEY,
    timestamp TIMESTAMPTZ NOT NULL,
    symbol VARCHAR(20) NOT NULL,
    bn_mid_price DECIMAL(20, 8) NOT NULL,
    bn_best_bid DECIMAL(20, 8) NOT NULL,
    bn_best_ask DECIMAL(20, 8) NOT NULL,
    bn_spread_bps DECIMAL(10, 4) NOT NULL,
    bn_obi_5 DECIMAL(10, 6) NOT NULL,
    bn_obi_10 DECIMAL(10, 6) NOT NULL,
    bn_bid_volume DECIMAL(20, 8) NOT NULL,
    bn_ask_volume DECIMAL(20, 8) NOT NULL,
    pm_yes_price DECIMAL(10, 4),
    pm_no_price DECIMAL(10, 4),
    pm_market_slug VARCHAR(100),
    pm_yes_token_id TEXT,
    pm_no_token_id TEXT,
    bn_price_change_1s DECIMAL(10, 6),
    bn_price_change_5s DECIMAL(10, 6),
    bn_momentum DECIMAL(10, 6),
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_sync_records_ts
    ON sync_records(timestamp);
CREATE INDEX IF NOT EXISTS idx_sync_records_symbol
    ON sync_records(symbol);
CREATE INDEX IF NOT EXISTS idx_sync_records_symbol_ts
    ON sync_records(symbol, timestamp);
CREATE INDEX IF NOT EXISTS idx_sync_records_pm_market_slug
    ON sync_records(pm_market_slug);

-- ============================================================
-- 7. pm_market_metadata — Market slug -> threshold/horizon/time
-- ============================================================
CREATE TABLE IF NOT EXISTS pm_market_metadata (
    market_slug TEXT PRIMARY KEY,
    price_to_beat NUMERIC(20,8) NOT NULL,
    start_time TIMESTAMPTZ,
    end_time TIMESTAMPTZ,
    horizon TEXT,
    symbol TEXT,
    raw_market JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_symbol_horizon
    ON pm_market_metadata(symbol, horizon);
CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_end_time
    ON pm_market_metadata(end_time DESC);
CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_updated_at
    ON pm_market_metadata(updated_at DESC);

-- ============================================================
-- 8. Binance klines — OHLCV candle persistence (NEW)
-- ============================================================
CREATE TABLE IF NOT EXISTS binance_klines (
    id BIGSERIAL PRIMARY KEY,
    symbol TEXT NOT NULL,
    interval TEXT NOT NULL,
    open_time TIMESTAMPTZ NOT NULL,
    close_time TIMESTAMPTZ NOT NULL,
    open NUMERIC(20,10) NOT NULL,
    high NUMERIC(20,10) NOT NULL,
    low NUMERIC(20,10) NOT NULL,
    close NUMERIC(20,10) NOT NULL,
    volume NUMERIC(20,10) NOT NULL,
    quote_volume NUMERIC(20,10) NOT NULL,
    trades BIGINT NOT NULL DEFAULT 0,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (symbol, interval, open_time)
);

-- ============================================================
-- 9. BACKFILL: pm_market_metadata from pm_token_settlements
--
-- Extracts market_slug, threshold, horizon, start/end times,
-- and symbol from the raw_market JSONB stored in settlements.
-- Runs idempotently via ON CONFLICT DO NOTHING.
-- ============================================================
DO $$
DECLARE
    backfilled INT := 0;
BEGIN
    -- Only attempt if pm_token_settlements has data
    IF EXISTS (SELECT 1 FROM pg_tables WHERE tablename = 'pm_token_settlements') THEN
        INSERT INTO pm_market_metadata (market_slug, price_to_beat, start_time, end_time, horizon, symbol, raw_market, updated_at)
        SELECT DISTINCT ON (market_slug)
            market_slug,
            COALESCE(
                -- Gamma API returns camelCase keys
                (raw_market->>'groupItemThreshold')::NUMERIC(20,8),
                -- Fallback: midpoint of upperBound and lowerBound
                ((COALESCE((raw_market->>'upperBound')::NUMERIC, 0) +
                  COALESCE((raw_market->>'lowerBound')::NUMERIC, 0)) / 2),
                0
            ) AS price_to_beat,
            -- Try camelCase first, then snake_case for compatibility
            COALESCE(
                (raw_market->>'eventStartTime')::TIMESTAMPTZ,
                (raw_market->>'startDate')::TIMESTAMPTZ,
                (raw_market->>'start_date')::TIMESTAMPTZ
            ) AS start_time,
            COALESCE(
                (raw_market->>'endDate')::TIMESTAMPTZ,
                (raw_market->>'end_date')::TIMESTAMPTZ
            ) AS end_time,
            -- Infer horizon from slug pattern (most reliable)
            CASE
                WHEN market_slug ~ '-5m-' THEN '5m'
                WHEN market_slug ~ '-15m-' THEN '15m'
                WHEN market_slug ~ '-60m-' THEN '60m'
                ELSE '60m'
            END AS horizon,
            -- Infer symbol from slug prefix
            CASE
                WHEN market_slug LIKE 'btc-%' THEN 'BTCUSDT'
                WHEN market_slug LIKE 'eth-%' THEN 'ETHUSDT'
                WHEN market_slug LIKE 'sol-%' THEN 'SOLUSDT'
                ELSE NULL
            END AS symbol,
            raw_market,
            NOW()
        FROM pm_token_settlements
        WHERE market_slug IS NOT NULL
          AND raw_market IS NOT NULL
          AND (raw_market->>'endDate' IS NOT NULL OR raw_market->>'end_date' IS NOT NULL)
          AND (raw_market->>'startDate' IS NOT NULL OR raw_market->>'start_date' IS NOT NULL)
        ORDER BY market_slug, resolved_at DESC NULLS LAST
        ON CONFLICT (market_slug) DO NOTHING;

        GET DIAGNOSTICS backfilled = ROW_COUNT;
        RAISE NOTICE 'pm_market_metadata backfill: % rows inserted', backfilled;
    END IF;
END $$;

-- ============================================================
-- 10. FIX price_to_beat: For up/down markets the threshold is
--     the Binance spot price at eventStartTime, not a fixed
--     number.  Gamma API returns groupItemThreshold=0 for these
--     relative markets.  This UPDATE fills in the opening price
--     from binance_price_ticks where available.
--     Guarded: only updates rows where price_to_beat is still 0.
-- ============================================================
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_tables WHERE tablename = 'pm_market_metadata')
       AND EXISTS (SELECT 1 FROM pg_tables WHERE tablename = 'binance_price_ticks')
    THEN
        UPDATE pm_market_metadata md
        SET price_to_beat = sub.spot_at_start,
            updated_at = NOW()
        FROM (
            SELECT md2.market_slug,
                (SELECT b.price
                 FROM binance_price_ticks b
                 WHERE b.symbol = md2.symbol
                   AND b.trade_time <= md2.start_time
                 ORDER BY b.trade_time DESC
                 LIMIT 1
                ) AS spot_at_start
            FROM pm_market_metadata md2
            WHERE md2.symbol IS NOT NULL
              AND md2.start_time IS NOT NULL
              AND md2.price_to_beat = 0
        ) sub
        WHERE md.market_slug = sub.market_slug
          AND sub.spot_at_start IS NOT NULL
          AND sub.spot_at_start > 0;
    END IF;
END $$;

-- ============================================================
-- 11. Pattern Memory Samples table
--     (formalized from runtime CREATE TABLE IF NOT EXISTS)
-- ============================================================
CREATE TABLE IF NOT EXISTS pattern_memory_samples (
    id BIGSERIAL PRIMARY KEY,
    symbol TEXT NOT NULL,
    pattern_len SMALLINT NOT NULL,
    pattern DOUBLE PRECISION[] NOT NULL,
    next_return DOUBLE PRECISION NOT NULL,
    sample_ts TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(symbol, pattern_len, sample_ts)
);

CREATE INDEX IF NOT EXISTS idx_pm_samples_symbol_len
    ON pattern_memory_samples(symbol, pattern_len, sample_ts DESC);

-- Optional: grant to app role if present.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ploy') THEN
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.binance_price_ticks TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.binance_lob_ticks TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.clob_orderbook_snapshots TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.clob_trade_ticks TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.clob_orderbook_history_ticks TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.sync_records TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.pm_market_metadata TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.binance_klines TO ploy';
        EXECUTE 'GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO ploy';
    END IF;
END $$;
