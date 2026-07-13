-- Migration: 030_strategy_runtime_execution_audit
-- Description: Persist strategy-runtime order/fill audit records with decimal quantities.

CREATE TABLE IF NOT EXISTS strategy_runtime_orders (
    id BIGSERIAL PRIMARY KEY,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    runtime_mode TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL DEFAULT '',
    intent_id TEXT NOT NULL,
    order_id TEXT NOT NULL UNIQUE,
    venue_order_id TEXT,
    event_id TEXT,
    symbol TEXT,
    token_id TEXT NOT NULL,
    market_side TEXT CHECK (market_side IN ('UP', 'DOWN')),
    order_side TEXT NOT NULL CHECK (order_side IN ('BUY', 'SELL')),
    quantity NUMERIC(20,10) NOT NULL,
    limit_price NUMERIC(20,10),
    filled_quantity NUMERIC(20,10) NOT NULL DEFAULT 0,
    avg_fill_price NUMERIC(20,10),
    status TEXT NOT NULL,
    rejection_reason TEXT,
    slippage NUMERIC(20,10),
    market_impact NUMERIC(20,10),
    context JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_strategy_runtime_orders_strategy_time
    ON strategy_runtime_orders(strategy_id, recorded_at DESC);

CREATE INDEX IF NOT EXISTS idx_strategy_runtime_orders_runtime_time
    ON strategy_runtime_orders(runtime_mode, recorded_at DESC);

CREATE INDEX IF NOT EXISTS idx_strategy_runtime_orders_token_time
    ON strategy_runtime_orders(token_id, recorded_at DESC);

CREATE TABLE IF NOT EXISTS strategy_runtime_fills (
    id BIGSERIAL PRIMARY KEY,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    runtime_mode TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL DEFAULT '',
    intent_id TEXT NOT NULL,
    order_id TEXT NOT NULL REFERENCES strategy_runtime_orders(order_id) ON DELETE CASCADE,
    fill_id TEXT NOT NULL UNIQUE,
    event_id TEXT,
    symbol TEXT,
    token_id TEXT NOT NULL,
    market_side TEXT CHECK (market_side IN ('UP', 'DOWN')),
    fill_side TEXT NOT NULL CHECK (fill_side IN ('BUY', 'SELL')),
    quantity NUMERIC(20,10) NOT NULL,
    price NUMERIC(20,10) NOT NULL,
    fee NUMERIC(20,10) NOT NULL DEFAULT 0,
    slippage NUMERIC(20,10),
    market_impact NUMERIC(20,10),
    fill_timestamp TIMESTAMPTZ NOT NULL,
    context JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_strategy_runtime_fills_strategy_time
    ON strategy_runtime_fills(strategy_id, fill_timestamp DESC);

CREATE INDEX IF NOT EXISTS idx_strategy_runtime_fills_runtime_time
    ON strategy_runtime_fills(runtime_mode, fill_timestamp DESC);

CREATE INDEX IF NOT EXISTS idx_strategy_runtime_fills_token_time
    ON strategy_runtime_fills(token_id, fill_timestamp DESC);
