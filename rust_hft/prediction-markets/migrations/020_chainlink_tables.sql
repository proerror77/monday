-- Migration: 020_chainlink_tables
-- Purpose: Chainlink RTDS price tick storage and market window labels for training data

-- Table 1: Raw Chainlink price ticks from RTDS WebSocket
CREATE TABLE IF NOT EXISTS chainlink_price_ticks (
    id BIGSERIAL PRIMARY KEY,
    symbol TEXT NOT NULL,           -- 'btc/usd', 'eth/usd', etc.
    price NUMERIC NOT NULL,
    source_timestamp TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_chainlink_ticks_symbol_time
    ON chainlink_price_ticks(symbol, source_timestamp DESC);

CREATE INDEX IF NOT EXISTS idx_chainlink_ticks_time
    ON chainlink_price_ticks(received_at DESC);

-- Table 2: Computed labels per 15-minute window (for model training)
CREATE TABLE IF NOT EXISTS market_window_labels (
    id BIGSERIAL PRIMARY KEY,
    symbol TEXT NOT NULL,           -- 'btc/usd'
    window_start TIMESTAMPTZ NOT NULL,
    window_end TIMESTAMPTZ NOT NULL,
    open_price NUMERIC NOT NULL,    -- S0 (Chainlink price at window open)
    close_price NUMERIC,            -- ST (Chainlink price at window close, NULL if still open)
    label SMALLINT,                 -- 1=Up, 0=Down, NULL=pending
    condition_id TEXT,              -- PM market condition_id if matched
    UNIQUE(symbol, window_start)
);

CREATE INDEX IF NOT EXISTS idx_market_window_labels_symbol
    ON market_window_labels(symbol, window_start DESC);
