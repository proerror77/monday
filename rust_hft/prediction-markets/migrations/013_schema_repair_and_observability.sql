-- Migration 013: Schema Repair + Strategy Observability
--
-- Goals:
-- 1) Repair broken/invalid performance indexes introduced by migration 007.
-- 2) Reconcile `order_idempotency` schema drift from duplicated 005 migrations.
-- 3) Add strategy/risk/exit/execution observability tables for audit + backtesting.

-- ============================================================================
-- A. Repair migration 007 index drift (safe on existing DBs)
-- ============================================================================

-- Orders: `leg_number` -> `leg`
DROP INDEX IF EXISTS idx_orders_cycle_leg;
CREATE INDEX IF NOT EXISTS idx_orders_cycle_leg
ON orders(cycle_id, leg, created_at DESC)
WHERE cycle_id IS NOT NULL;

-- Positions: status values are uppercase in schema
DROP INDEX IF EXISTS idx_positions_status_opened;
CREATE INDEX IF NOT EXISTS idx_positions_status_opened
ON positions(status, opened_at DESC)
WHERE status = 'OPEN';

-- Reconciliation: canonical timestamp column is `timestamp`
DROP INDEX IF EXISTS idx_reconciliation_log_created;
CREATE INDEX IF NOT EXISTS idx_reconciliation_log_created
ON position_reconciliation_log(timestamp DESC);

-- Additional unresolved severity filter index (without clobbering existing names)
CREATE INDEX IF NOT EXISTS idx_discrepancies_severity_unresolved
ON position_discrepancies(severity, created_at DESC)
WHERE resolved = FALSE;

-- Nonce usage: active = not released; sort by allocation time
DROP INDEX IF EXISTS idx_nonce_usage_active;
DO $$
BEGIN
    IF to_regclass('public.nonce_usage') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'nonce_usage'
              AND column_name = 'allocated_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active
                     ON nonce_usage(wallet_address, allocated_at DESC)
                     WHERE released_at IS NULL';
        ELSIF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'nonce_usage'
              AND column_name = 'used_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active
                     ON nonce_usage(wallet_address, used_at DESC)
                     WHERE released_at IS NULL';
        END IF;
    END IF;
END $$;

-- Fills: canonical fill time column is `timestamp`
DROP INDEX IF EXISTS idx_fills_position_time;
CREATE INDEX IF NOT EXISTS idx_fills_position_time
ON fills(position_id, timestamp DESC);

DROP INDEX IF EXISTS idx_fills_order_time;
CREATE INDEX IF NOT EXISTS idx_fills_order_time
ON fills(order_id, timestamp DESC);

-- Balance snapshots: table has no wallet_address in current schema
DROP INDEX IF EXISTS idx_balance_snapshots_latest;
CREATE INDEX IF NOT EXISTS idx_balance_snapshots_latest
ON balance_snapshots(timestamp DESC);

-- Heartbeats: component column name is `component_name`
DROP INDEX IF EXISTS idx_heartbeats_component_time;
CREATE INDEX IF NOT EXISTS idx_heartbeats_component_time
ON component_heartbeats(component_name, last_heartbeat DESC);

-- System events: component column name is `component`
DROP INDEX IF EXISTS idx_system_events_component_time;
CREATE INDEX IF NOT EXISTS idx_system_events_component_time
ON system_events(component, created_at DESC);

-- ============================================================================
-- B. Reconcile order_idempotency schema drift
-- ============================================================================

ALTER TABLE order_idempotency
    ADD COLUMN IF NOT EXISTS request_hash TEXT,
    ADD COLUMN IF NOT EXISTS response_data JSONB,
    ADD COLUMN IF NOT EXISTS error_message TEXT,
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- Ensure unique idempotency key for ON CONFLICT usage
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'order_idempotency_idempotency_key_key'
    ) THEN
        ALTER TABLE order_idempotency
            ADD CONSTRAINT order_idempotency_idempotency_key_key UNIQUE (idempotency_key);
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_order_idempotency_key ON order_idempotency(idempotency_key);
CREATE INDEX IF NOT EXISTS idx_order_idempotency_hash ON order_idempotency(request_hash);
CREATE INDEX IF NOT EXISTS idx_order_idempotency_status ON order_idempotency(status, created_at);
CREATE INDEX IF NOT EXISTS idx_order_idempotency_expires ON order_idempotency(expires_at);

-- ============================================================================
-- C. Strategy observability tables
-- ============================================================================

CREATE TABLE IF NOT EXISTS signal_history (
    id BIGSERIAL PRIMARY KEY,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    intent_id UUID,
    agent_id TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    domain TEXT NOT NULL,
    signal_type TEXT NOT NULL,
    market_slug TEXT,
    token_id TEXT,
    symbol TEXT,
    side TEXT,
    confidence NUMERIC(12,6),
    momentum_value NUMERIC(20,10),
    short_ma NUMERIC(20,10),
    long_ma NUMERIC(20,10),
    rolling_volatility NUMERIC(20,10),
    fair_value NUMERIC(12,6),
    market_price NUMERIC(12,6),
    edge NUMERIC(20,10),
    config_hash TEXT,
    context JSONB
);

CREATE INDEX IF NOT EXISTS idx_signal_history_time ON signal_history(recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_signal_history_agent_time ON signal_history(agent_id, recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_signal_history_strategy_time ON signal_history(strategy_id, recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_signal_history_intent ON signal_history(intent_id);

CREATE TABLE IF NOT EXISTS risk_gate_decisions (
    id BIGSERIAL PRIMARY KEY,
    decided_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    intent_id UUID NOT NULL UNIQUE,
    agent_id TEXT NOT NULL,
    domain TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('PASSED','BLOCKED','ADJUSTED')),
    block_reason TEXT,
    suggestion_max_shares BIGINT,
    suggestion_reason TEXT,
    notional_value NUMERIC(20,10),
    config_hash TEXT,
    metadata JSONB
);

CREATE INDEX IF NOT EXISTS idx_risk_gate_decisions_time ON risk_gate_decisions(decided_at DESC);
CREATE INDEX IF NOT EXISTS idx_risk_gate_decisions_agent_time ON risk_gate_decisions(agent_id, decided_at DESC);

CREATE TABLE IF NOT EXISTS exit_reasons (
    id BIGSERIAL PRIMARY KEY,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    intent_id UUID NOT NULL UNIQUE,
    agent_id TEXT NOT NULL,
    domain TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    token_id TEXT NOT NULL,
    market_side TEXT,
    reason_code TEXT NOT NULL,
    reason_detail TEXT,
    entry_price NUMERIC(12,6),
    exit_price NUMERIC(12,6),
    pnl_pct NUMERIC(20,10),
    status TEXT NOT NULL,
    config_hash TEXT,
    metadata JSONB
);

CREATE INDEX IF NOT EXISTS idx_exit_reasons_time ON exit_reasons(recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_exit_reasons_reason_time ON exit_reasons(reason_code, recorded_at DESC);

CREATE TABLE IF NOT EXISTS execution_analysis (
    id BIGSERIAL PRIMARY KEY,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    intent_id UUID NOT NULL UNIQUE,
    agent_id TEXT NOT NULL,
    domain TEXT NOT NULL,
    market_slug TEXT NOT NULL,
    token_id TEXT NOT NULL,
    is_buy BOOLEAN NOT NULL,
    expected_price NUMERIC(12,6) NOT NULL,
    executed_price NUMERIC(12,6),
    expected_slippage_bps NUMERIC(20,10),
    actual_slippage_bps NUMERIC(20,10),
    queue_delay_ms BIGINT,
    execution_latency_ms BIGINT,
    total_latency_ms BIGINT,
    status TEXT NOT NULL,
    dry_run BOOLEAN NOT NULL DEFAULT FALSE,
    config_hash TEXT,
    metadata JSONB
);

CREATE INDEX IF NOT EXISTS idx_execution_analysis_time ON execution_analysis(recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_execution_analysis_agent_time ON execution_analysis(agent_id, recorded_at DESC);
