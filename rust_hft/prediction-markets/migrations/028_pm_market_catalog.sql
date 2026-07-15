-- Migration 028: normalized market catalog for crypto and sports discovery
--
-- Also fixes the runtime/schema mismatch on pm_market_metadata.price_to_beat:
-- discovery and later correction paths already treat it as nullable.

ALTER TABLE pm_market_metadata
    ALTER COLUMN price_to_beat DROP NOT NULL;

CREATE TABLE IF NOT EXISTS pm_market_catalog (
    market_id TEXT PRIMARY KEY,
    event_id TEXT,
    event_slug TEXT,
    market_slug TEXT,
    title TEXT,
    market_family TEXT NOT NULL,
    market_semantics TEXT NOT NULL,
    strategy_symbol TEXT,
    reference_symbol TEXT,
    settlement_source TEXT NOT NULL,
    league TEXT,
    sport TEXT,
    start_time TIMESTAMPTZ,
    end_time TIMESTAMPTZ,
    token_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    home_team TEXT,
    away_team TEXT,
    active BOOLEAN,
    accepting_orders BOOLEAN,
    raw_event JSONB,
    raw_market JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT pm_market_catalog_token_ids_array
        CHECK (jsonb_typeof(token_ids) = 'array')
);

CREATE INDEX IF NOT EXISTS idx_pm_market_catalog_event_id
    ON pm_market_catalog(event_id);
CREATE INDEX IF NOT EXISTS idx_pm_market_catalog_family_end_time
    ON pm_market_catalog(market_family, end_time DESC);
CREATE INDEX IF NOT EXISTS idx_pm_market_catalog_strategy_symbol
    ON pm_market_catalog(strategy_symbol, end_time DESC);
CREATE INDEX IF NOT EXISTS idx_pm_market_catalog_reference_symbol
    ON pm_market_catalog(reference_symbol, end_time DESC);
CREATE INDEX IF NOT EXISTS idx_pm_market_catalog_league
    ON pm_market_catalog(league, end_time DESC);
CREATE INDEX IF NOT EXISTS idx_pm_market_catalog_updated_at
    ON pm_market_catalog(updated_at DESC);

DO $$
BEGIN
    IF to_regclass('public.pm_market_catalog') IS NOT NULL THEN
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.pm_market_catalog TO ploy';
    END IF;
END $$;
