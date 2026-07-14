-- Migration 014: Single-DB Multi-Account + Collector Token Targets
--
-- Goals:
-- 1) Add a minimal "accounts" table for single-DB multi-account deployments.
-- 2) Add `account_id` columns to execution/audit tables so multiple bots can
--    write into one Postgres safely and be queried by account.
-- 3) Add `collector_token_targets` so collectors can explicitly scope which
--    tokens to backfill/record (e.g., crypto + today's NBA only).

-- ============================================================================
-- A) Accounts
-- ============================================================================

CREATE TABLE IF NOT EXISTS accounts (
    account_id TEXT PRIMARY KEY,
    wallet_address TEXT,
    label TEXT,
    metadata JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed default account (keeps existing single-account installs working).
INSERT INTO accounts (account_id, label)
VALUES ('default', 'Default')
ON CONFLICT (account_id) DO NOTHING;

-- Keep updated_at fresh
DO $$
BEGIN
    IF to_regclass('public.accounts') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS update_accounts_updated_at ON accounts;
        CREATE TRIGGER update_accounts_updated_at
        BEFORE UPDATE ON accounts
        FOR EACH ROW
        EXECUTE FUNCTION update_updated_at_column();
    END IF;
EXCEPTION WHEN undefined_function THEN
    -- update_updated_at_column() may not exist on very old installs; ignore.
    NULL;
END $$;

-- ============================================================================
-- B) Multi-account columns for execution/audit tables
-- ============================================================================

-- Coordinator execution log
CREATE TABLE IF NOT EXISTS agent_order_executions (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL DEFAULT 'default',
    agent_id TEXT NOT NULL,
    intent_id UUID NOT NULL,
    domain TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    token_id TEXT NOT NULL,
    market_side TEXT NOT NULL CHECK (market_side IN ('UP', 'DOWN')),
    is_buy BOOLEAN NOT NULL,
    shares BIGINT NOT NULL,
    limit_price NUMERIC(10,6) NOT NULL,
    order_id TEXT,
    status TEXT NOT NULL,
    filled_shares BIGINT NOT NULL DEFAULT 0,
    avg_fill_price NUMERIC(10,6),
    elapsed_ms BIGINT,
    dry_run BOOLEAN NOT NULL DEFAULT FALSE,
    error TEXT,
    intent_created_at TIMESTAMPTZ,
    metadata JSONB,
    executed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(intent_id)
);

ALTER TABLE agent_order_executions
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';

CREATE INDEX IF NOT EXISTS idx_agent_order_executions_account_time
    ON agent_order_executions(account_id, executed_at DESC);

-- Strategy observability tables (migration 013)
ALTER TABLE signal_history
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';
CREATE INDEX IF NOT EXISTS idx_signal_history_account_time
    ON signal_history(account_id, recorded_at DESC);

ALTER TABLE risk_gate_decisions
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';
CREATE INDEX IF NOT EXISTS idx_risk_gate_decisions_account_time
    ON risk_gate_decisions(account_id, decided_at DESC);

ALTER TABLE exit_reasons
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';
CREATE INDEX IF NOT EXISTS idx_exit_reasons_account_time
    ON exit_reasons(account_id, recorded_at DESC);

ALTER TABLE execution_analysis
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';
CREATE INDEX IF NOT EXISTS idx_execution_analysis_account_time
    ON execution_analysis(account_id, recorded_at DESC);

-- Sports observation tables (created by sports agent / migrations 011-012)
ALTER TABLE nba_live_observations
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';
CREATE INDEX IF NOT EXISTS idx_nba_live_observations_account_time
    ON nba_live_observations(account_id, recorded_at DESC);

CREATE TABLE IF NOT EXISTS grok_game_intel (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL DEFAULT 'default',
    agent_id TEXT NOT NULL,
    espn_game_id TEXT NOT NULL,
    home_team TEXT NOT NULL,
    away_team TEXT NOT NULL,
    quarter INTEGER NOT NULL,
    clock TEXT NOT NULL,
    score TEXT NOT NULL,
    momentum_direction TEXT NOT NULL,
    home_sentiment_score DOUBLE PRECISION,
    away_sentiment_score DOUBLE PRECISION,
    grok_home_win_prob DOUBLE PRECISION,
    grok_confidence DOUBLE PRECISION,
    injury_updates JSONB DEFAULT '[]',
    key_factors JSONB DEFAULT '[]',
    signal_type TEXT,
    signal_edge DOUBLE PRECISION,
    signal_acted_on BOOLEAN NOT NULL DEFAULT FALSE,
    raw_response TEXT,
    query_duration_ms INTEGER,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE grok_game_intel
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';

CREATE INDEX IF NOT EXISTS idx_grok_game_intel_account_time
    ON grok_game_intel(account_id, recorded_at DESC);

-- Idempotency (order dedupe) is account-scoped.
ALTER TABLE order_idempotency
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';

-- If the PK is on idempotency_key, replace it with a composite PK.
DO $$
DECLARE
    pk_cols TEXT[];
BEGIN
    IF to_regclass('public.order_idempotency') IS NULL THEN
        RETURN;
    END IF;

    SELECT array_agg(a.attname ORDER BY x.ordinality)
    INTO pk_cols
    FROM pg_constraint c
    JOIN unnest(c.conkey) WITH ORDINALITY AS x(attnum, ordinality) ON true
    JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = x.attnum
    WHERE c.conrelid = 'public.order_idempotency'::regclass
      AND c.contype = 'p';

    -- Drop any UNIQUE constraint that enforces global uniqueness of idempotency_key.
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'public.order_idempotency'::regclass
          AND conname = 'order_idempotency_idempotency_key_key'
    ) THEN
        EXECUTE 'ALTER TABLE order_idempotency DROP CONSTRAINT order_idempotency_idempotency_key_key';
    END IF;

    IF pk_cols = ARRAY['idempotency_key'] THEN
        EXECUTE 'ALTER TABLE order_idempotency DROP CONSTRAINT order_idempotency_pkey';
        EXECUTE 'ALTER TABLE order_idempotency ADD PRIMARY KEY (account_id, idempotency_key)';
    ELSE
        -- Otherwise keep existing PK (commonly `id`) and add a composite UNIQUE index.
        EXECUTE 'CREATE UNIQUE INDEX IF NOT EXISTS idx_order_idempotency_account_key ON order_idempotency(account_id, idempotency_key)';
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_order_idempotency_account_expires
    ON order_idempotency(account_id, expires_at);

-- ============================================================================
-- C) Collector token targets (scope collectors to crypto + today NBA)
-- ============================================================================

CREATE TABLE IF NOT EXISTS collector_token_targets (
    token_id TEXT PRIMARY KEY,
    domain TEXT NOT NULL,
    target_date DATE,
    expires_at TIMESTAMPTZ,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_collector_token_targets_domain
    ON collector_token_targets(domain);
CREATE INDEX IF NOT EXISTS idx_collector_token_targets_target_date
    ON collector_token_targets(target_date)
    WHERE target_date IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_collector_token_targets_expires
    ON collector_token_targets(expires_at)
    WHERE expires_at IS NOT NULL;

DO $$
BEGIN
    IF to_regclass('public.collector_token_targets') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS update_collector_token_targets_updated_at ON collector_token_targets;
        CREATE TRIGGER update_collector_token_targets_updated_at
        BEFORE UPDATE ON collector_token_targets
        FOR EACH ROW
        EXECUTE FUNCTION update_updated_at_column();
    END IF;
EXCEPTION WHEN undefined_function THEN
    NULL;
END $$;

-- ============================================================================
-- D) Privileges (optional)
--
-- Many deployments run migrations as `postgres` while services connect as a
-- dedicated app user (commonly `ploy`). Ensure the app user can read/write the
-- tables introduced/extended by this migration.
-- ============================================================================

DO $$
DECLARE
    tbl TEXT;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ploy') THEN
        RETURN;
    END IF;

    FOREACH tbl IN ARRAY ARRAY[
        'accounts',
        'collector_token_targets',
        'agent_order_executions',
        'signal_history',
        'risk_gate_decisions',
        'exit_reasons',
        'execution_analysis',
        'nba_live_observations',
        'grok_game_intel',
        'order_idempotency'
    ]
    LOOP
        IF to_regclass('public.' || tbl) IS NOT NULL THEN
            EXECUTE format(
                'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.%I TO ploy',
                tbl
            );
        END IF;
    END LOOP;

    -- Bigserial ids create sequences; services need USAGE to insert rows.
    EXECUTE 'GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO ploy';

    -- Future objects created by the migration role should also be accessible.
    EXECUTE 'ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO ploy';
    EXECUTE 'ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO ploy';
END $$;
