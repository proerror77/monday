-- Normalized public market data from Binance Futures and external CEX L2 feeds.

CREATE TABLE IF NOT EXISTS cex_public_market_ticks (
    id BIGSERIAL PRIMARY KEY,
    exchange TEXT NOT NULL CHECK (exchange IN ('binance','okx','bybit','coinbase','kraken')),
    market_type TEXT NOT NULL CHECK (market_type IN ('spot','perpetual')),
    symbol TEXT NOT NULL,
    exchange_symbol TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('derivatives_snapshot','liquidation','lob')),
    source_key TEXT GENERATED ALWAYS AS (exchange || '/' || kind) STORED,
    event_time TIMESTAMPTZ NOT NULL,
    update_id BIGINT,
    sequence_id BIGINT,
    mark_price NUMERIC(30,12),
    index_price NUMERIC(30,12),
    funding_rate NUMERIC(20,12),
    open_interest NUMERIC(30,12),
    basis NUMERIC(30,12),
    basis_rate NUMERIC(20,12),
    annualized_basis_rate NUMERIC(20,12),
    next_funding_time TIMESTAMPTZ,
    side TEXT,
    price NUMERIC(30,12),
    quantity NUMERIC(30,12),
    best_bid NUMERIC(30,12),
    best_ask NUMERIC(30,12),
    mid_price NUMERIC(30,12),
    spread_bps NUMERIC(20,8),
    obi_5 NUMERIC(20,12),
    obi_10 NUMERIC(20,12),
    bid_volume_5 NUMERIC(30,12),
    ask_volume_5 NUMERIC(30,12),
    bids JSONB,
    asks JSONB,
    source TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    raw JSONB NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (exchange, kind, exchange_symbol, dedupe_key)
);

CREATE INDEX IF NOT EXISTS idx_cex_public_ticks_exchange_symbol_time
    ON cex_public_market_ticks(exchange, symbol, event_time DESC);
CREATE INDEX IF NOT EXISTS idx_cex_public_ticks_kind_time
    ON cex_public_market_ticks(kind, event_time DESC);
CREATE INDEX IF NOT EXISTS idx_cex_public_ticks_source_key_time
    ON cex_public_market_ticks(source_key, event_time DESC);

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ploy') THEN
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.cex_public_market_ticks TO ploy';
        EXECUTE 'GRANT USAGE, SELECT ON SEQUENCE public.cex_public_market_ticks_id_seq TO ploy';
    END IF;
END $$;
