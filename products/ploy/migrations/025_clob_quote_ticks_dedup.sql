-- Migration: 023_clob_quote_ticks_dedup
-- Description: Add domain column, relax side to nullable (live feed has no side info),
--              and add unique indexes for deduplication of REST-polled quotes and spot ticks.

-- Add domain column if missing (collector.rs inserts it but migration 010 omitted it)
ALTER TABLE clob_quote_ticks
    ADD COLUMN IF NOT EXISTS domain TEXT;

-- Relax side to nullable so live-feed inserts don't require it
ALTER TABLE clob_quote_ticks
    ALTER COLUMN side DROP NOT NULL;

-- Unique index: one quote per token per second.
-- date_trunc on timestamptz is not IMMUTABLE in Postgres, so we cast to UTC timestamp
-- first (which is IMMUTABLE) before truncating.
CREATE UNIQUE INDEX IF NOT EXISTS uq_clob_quote_ticks_token_second
    ON clob_quote_ticks (token_id, date_trunc('second', received_at AT TIME ZONE 'UTC'));

-- Unique index: one spot tick per symbol per second.
CREATE UNIQUE INDEX IF NOT EXISTS uq_binance_price_ticks_symbol_second
    ON binance_price_ticks (symbol, date_trunc('second', trade_time AT TIME ZONE 'UTC'));
