-- Migration: 035_binance_agg_trade_ticks
-- Description: Persist Binance aggTrade stream with aggressor-side metadata.

CREATE TABLE IF NOT EXISTS binance_agg_trade_ticks (
    id BIGSERIAL PRIMARY KEY,
    symbol TEXT NOT NULL,
    agg_trade_id BIGINT NOT NULL,
    first_trade_id BIGINT,
    last_trade_id BIGINT,
    price NUMERIC(20,10) NOT NULL,
    quantity NUMERIC(20,10) NOT NULL,
    trade_time TIMESTAMPTZ NOT NULL,
    event_time TIMESTAMPTZ,
    is_buyer_maker BOOLEAN NOT NULL,
    source TEXT NOT NULL DEFAULT 'binance_agg_trade_ws',
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (symbol, agg_trade_id)
);

CREATE INDEX IF NOT EXISTS idx_binance_agg_trade_ticks_symbol_time
    ON binance_agg_trade_ticks(symbol, trade_time DESC);
CREATE INDEX IF NOT EXISTS idx_binance_agg_trade_ticks_time
    ON binance_agg_trade_ticks(trade_time DESC);
