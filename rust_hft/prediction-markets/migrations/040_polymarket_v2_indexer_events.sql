-- Migration: 040_polymarket_v2_indexer_events
-- Description: Persist Polymarket V2 chain-indexer events for reconciliation and research.

CREATE TABLE IF NOT EXISTS polymarket_v2_indexer_sync_state (
    source TEXT PRIMARY KEY,
    last_block_number BIGINT NOT NULL DEFAULT 0,
    last_block_timestamp TIMESTAMPTZ,
    last_transaction_hash TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS polymarket_v2_order_fills (
    id BIGSERIAL PRIMARY KEY,
    chain_id INTEGER NOT NULL DEFAULT 137,
    block_number BIGINT NOT NULL,
    log_index INTEGER NOT NULL,
    block_timestamp TIMESTAMPTZ NOT NULL,
    transaction_hash TEXT NOT NULL,
    tx_from TEXT,
    exchange TEXT NOT NULL,
    order_hash TEXT NOT NULL,
    maker TEXT NOT NULL,
    taker TEXT NOT NULL,
    side SMALLINT NOT NULL CHECK (side IN (0, 1)),
    token_id TEXT NOT NULL,
    market_id TEXT,
    maker_amount_raw NUMERIC(78,0) NOT NULL,
    taker_amount_raw NUMERIC(78,0) NOT NULL,
    fee_raw NUMERIC(78,0) NOT NULL DEFAULT 0,
    builder TEXT NOT NULL DEFAULT '0x0000000000000000000000000000000000000000000000000000000000000000',
    metadata TEXT NOT NULL DEFAULT '0x0000000000000000000000000000000000000000000000000000000000000000',
    raw_event JSONB NOT NULL DEFAULT '{}'::jsonb,
    source TEXT NOT NULL DEFAULT 'envio_polymarket_v2_indexer',
    ingested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (chain_id, block_number, log_index)
);

CREATE INDEX IF NOT EXISTS idx_polymarket_v2_order_fills_token_time
    ON polymarket_v2_order_fills(token_id, block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_polymarket_v2_order_fills_tx
    ON polymarket_v2_order_fills(transaction_hash);
CREATE INDEX IF NOT EXISTS idx_polymarket_v2_order_fills_maker_time
    ON polymarket_v2_order_fills(maker, block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_polymarket_v2_order_fills_taker_time
    ON polymarket_v2_order_fills(taker, block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_polymarket_v2_order_fills_builder_time
    ON polymarket_v2_order_fills(builder, block_timestamp DESC)
    WHERE builder <> '0x0000000000000000000000000000000000000000000000000000000000000000';

CREATE TABLE IF NOT EXISTS polymarket_v2_order_matches (
    id BIGSERIAL PRIMARY KEY,
    chain_id INTEGER NOT NULL DEFAULT 137,
    block_number BIGINT NOT NULL,
    log_index INTEGER NOT NULL,
    block_timestamp TIMESTAMPTZ NOT NULL,
    transaction_hash TEXT NOT NULL,
    exchange TEXT NOT NULL,
    taker_order_hash TEXT NOT NULL,
    taker_order_maker TEXT NOT NULL,
    side SMALLINT NOT NULL CHECK (side IN (0, 1)),
    token_id TEXT NOT NULL,
    market_id TEXT,
    maker_amount_raw NUMERIC(78,0) NOT NULL,
    taker_amount_raw NUMERIC(78,0) NOT NULL,
    raw_event JSONB NOT NULL DEFAULT '{}'::jsonb,
    source TEXT NOT NULL DEFAULT 'envio_polymarket_v2_indexer',
    ingested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (chain_id, block_number, log_index)
);

CREATE INDEX IF NOT EXISTS idx_polymarket_v2_order_matches_token_time
    ON polymarket_v2_order_matches(token_id, block_timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_polymarket_v2_order_matches_tx
    ON polymarket_v2_order_matches(transaction_hash);

CREATE TABLE IF NOT EXISTS polymarket_v2_fee_events (
    id BIGSERIAL PRIMARY KEY,
    chain_id INTEGER NOT NULL DEFAULT 137,
    block_number BIGINT NOT NULL,
    log_index INTEGER NOT NULL,
    block_timestamp TIMESTAMPTZ NOT NULL,
    transaction_hash TEXT NOT NULL,
    receiver TEXT NOT NULL,
    amount_raw NUMERIC(78,0) NOT NULL,
    raw_event JSONB NOT NULL DEFAULT '{}'::jsonb,
    source TEXT NOT NULL DEFAULT 'envio_polymarket_v2_indexer',
    ingested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (chain_id, block_number, log_index)
);

CREATE INDEX IF NOT EXISTS idx_polymarket_v2_fee_events_receiver_time
    ON polymarket_v2_fee_events(receiver, block_timestamp DESC);

CREATE TABLE IF NOT EXISTS polymarket_v2_polyusd_events (
    id BIGSERIAL PRIMARY KEY,
    chain_id INTEGER NOT NULL DEFAULT 137,
    block_number BIGINT NOT NULL,
    log_index INTEGER NOT NULL,
    block_timestamp TIMESTAMPTZ NOT NULL,
    transaction_hash TEXT NOT NULL,
    event_type TEXT NOT NULL CHECK (event_type IN ('transfer', 'wrap', 'unwrap')),
    address_from TEXT,
    address_to TEXT,
    caller TEXT,
    asset TEXT,
    amount_raw NUMERIC(78,0) NOT NULL,
    raw_event JSONB NOT NULL DEFAULT '{}'::jsonb,
    source TEXT NOT NULL DEFAULT 'envio_polymarket_v2_indexer',
    ingested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (chain_id, block_number, log_index, event_type)
);

CREATE INDEX IF NOT EXISTS idx_polymarket_v2_polyusd_events_to_time
    ON polymarket_v2_polyusd_events(address_to, block_timestamp DESC)
    WHERE address_to IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_polymarket_v2_polyusd_events_from_time
    ON polymarket_v2_polyusd_events(address_from, block_timestamp DESC)
    WHERE address_from IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_polymarket_v2_polyusd_events_caller_time
    ON polymarket_v2_polyusd_events(caller, block_timestamp DESC)
    WHERE caller IS NOT NULL;

CREATE OR REPLACE VIEW polymarket_v2_indexer_health AS
SELECT
    source,
    last_block_number,
    last_block_timestamp,
    last_transaction_hash,
    updated_at,
    NOW() - updated_at AS cursor_age
FROM polymarket_v2_indexer_sync_state;

COMMENT ON TABLE polymarket_v2_order_fills IS
    'Polymarket V2 CTFExchange OrderFilled events from the Envio sidecar indexer. Use for reconciliation and research, not realtime trading signals.';
COMMENT ON TABLE polymarket_v2_polyusd_events IS
    'Polymarket V2 pUSD Transfer/Wrapped/Unwrapped events used for collateral and wallet-flow reconciliation.';

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ploy') THEN
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.polymarket_v2_indexer_sync_state TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.polymarket_v2_order_fills TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.polymarket_v2_order_matches TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.polymarket_v2_fee_events TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.polymarket_v2_polyusd_events TO ploy';
        EXECUTE 'GRANT SELECT ON TABLE public.polymarket_v2_indexer_health TO ploy';
    END IF;
END $$;
