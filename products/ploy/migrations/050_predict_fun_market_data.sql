-- Predict.fun market catalog and normalized Yes/No top-of-book history.
-- Kept separate from Polymarket's pm_* tables so numeric market identifiers
-- cannot collide across venues.

CREATE TABLE IF NOT EXISTS predict_fun_markets (
    market_id BIGINT PRIMARY KEY,
    condition_id TEXT NOT NULL,
    title TEXT NOT NULL,
    question TEXT NOT NULL,
    description TEXT,
    decimal_precision INTEGER NOT NULL CHECK (decimal_precision BETWEEN 0 AND 18),
    trading_status TEXT NOT NULL,
    status TEXT NOT NULL,
    is_visible BOOLEAN NOT NULL,
    is_neg_risk BOOLEAN NOT NULL,
    is_yield_bearing BOOLEAN NOT NULL,
    fee_rate_bps INTEGER NOT NULL,
    outcomes JSONB NOT NULL,
    resolution JSONB,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_predict_fun_markets_trading_status
    ON predict_fun_markets (trading_status, is_visible);
CREATE INDEX IF NOT EXISTS idx_predict_fun_markets_observed_at
    ON predict_fun_markets (observed_at DESC);

CREATE TABLE IF NOT EXISTS predict_fun_orderbook_ticks (
    id BIGSERIAL PRIMARY KEY,
    market_id BIGINT NOT NULL REFERENCES predict_fun_markets(market_id),
    exchange_timestamp_ms BIGINT NOT NULL,
    best_yes_bid NUMERIC,
    best_yes_bid_size NUMERIC,
    best_yes_ask NUMERIC,
    best_yes_ask_size NUMERIC,
    best_no_bid NUMERIC,
    best_no_bid_size NUMERIC,
    best_no_ask NUMERIC,
    best_no_ask_size NUMERIC,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_predict_fun_books_market_time
    ON predict_fun_orderbook_ticks (market_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_predict_fun_books_received_at
    ON predict_fun_orderbook_ticks (received_at DESC);

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ploy') THEN
        GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE predict_fun_markets TO ploy;
        GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE predict_fun_orderbook_ticks TO ploy;
        GRANT USAGE, SELECT ON SEQUENCE predict_fun_orderbook_ticks_id_seq TO ploy;
    END IF;
END
$$;
