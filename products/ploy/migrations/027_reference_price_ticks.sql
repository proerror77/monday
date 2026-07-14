-- Migration: 027_reference_price_ticks
-- Purpose: Canonical non-crypto reference-price capture for Pyth-backed markets.

CREATE TABLE IF NOT EXISTS reference_price_ticks (
    id BIGSERIAL PRIMARY KEY,
    symbol TEXT NOT NULL,
    source TEXT NOT NULL,
    asset_class TEXT NOT NULL,
    price NUMERIC NOT NULL,
    full_accuracy_value TEXT,
    price_time TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    is_carried_forward BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX IF NOT EXISTS idx_reference_price_ticks_symbol_time
    ON reference_price_ticks(symbol, price_time DESC);

CREATE INDEX IF NOT EXISTS idx_reference_price_ticks_source_symbol_time
    ON reference_price_ticks(source, symbol, price_time DESC);

CREATE INDEX IF NOT EXISTS idx_reference_price_ticks_received_at
    ON reference_price_ticks(received_at DESC);
