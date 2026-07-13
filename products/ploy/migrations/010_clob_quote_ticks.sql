-- Migration: 010_clob_quote_ticks
-- Description: Persist Polymarket CLOB quote stream for audit and replay

CREATE TABLE IF NOT EXISTS clob_quote_ticks (
    id BIGSERIAL PRIMARY KEY,
    token_id TEXT NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('UP', 'DOWN')),
    best_bid NUMERIC(10,6),
    best_ask NUMERIC(10,6),
    bid_size NUMERIC(18,8),
    ask_size NUMERIC(18,8),
    source TEXT NOT NULL DEFAULT 'polymarket_ws',
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_clob_quote_ticks_token_time
    ON clob_quote_ticks(token_id, received_at DESC);

CREATE INDEX IF NOT EXISTS idx_clob_quote_ticks_time
    ON clob_quote_ticks(received_at DESC);
