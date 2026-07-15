-- Migration 015: Polymarket official settlement outcomes (token-level)
--
-- Purpose:
-- - Persist official settlement state for each CLOB token id.
-- - Enables prediction accuracy scoring using Polymarket's own resolution
--   (token settles to 1.0 for winner, 0.0 for loser).

CREATE TABLE IF NOT EXISTS pm_token_settlements (
    token_id TEXT PRIMARY KEY,
    condition_id TEXT,
    market_id TEXT,
    market_slug TEXT,
    outcome TEXT,
    settled_price NUMERIC(10,6),
    resolved BOOLEAN NOT NULL DEFAULT FALSE,
    resolved_at TIMESTAMPTZ,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    raw_market JSONB
);

CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_condition
    ON pm_token_settlements(condition_id);
CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_market_slug
    ON pm_token_settlements(market_slug);
CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_resolved_at
    ON pm_token_settlements(resolved_at DESC)
    WHERE resolved_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_fetched_at
    ON pm_token_settlements(fetched_at DESC);

-- Optional: grant to app role if present.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ploy') THEN
        IF to_regclass('public.pm_token_settlements') IS NOT NULL THEN
            EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.pm_token_settlements TO ploy';
        END IF;
    END IF;
END $$;
