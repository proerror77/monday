-- Migration: 017_strategy_evaluations
-- Description: Persist strategy-level evaluation evidence for deployment gating

CREATE TABLE IF NOT EXISTS strategy_evaluations (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL DEFAULT 'default',
    evaluated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    strategy_id TEXT NOT NULL,
    deployment_id TEXT,
    domain TEXT NOT NULL,
    stage TEXT NOT NULL CHECK (stage IN ('BACKTEST','PAPER','LIVE')),
    status TEXT NOT NULL CHECK (status IN ('PASS','FAIL','WARN','UNKNOWN')),
    score NUMERIC(12,6),
    timeframe TEXT,
    sample_size BIGINT,
    pnl_usd NUMERIC(20,10),
    win_rate NUMERIC(12,6),
    sharpe NUMERIC(20,10),
    max_drawdown_pct NUMERIC(12,6),
    max_drawdown_usd NUMERIC(20,10),
    evidence_kind TEXT NOT NULL DEFAULT 'report',
    evidence_ref TEXT,
    evidence_hash TEXT,
    evidence_payload JSONB,
    metadata JSONB
);

ALTER TABLE strategy_evaluations
    ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default';

CREATE INDEX IF NOT EXISTS idx_strategy_evaluations_account_time
    ON strategy_evaluations(account_id, evaluated_at DESC);

CREATE INDEX IF NOT EXISTS idx_strategy_evaluations_strategy_stage_time
    ON strategy_evaluations(account_id, strategy_id, stage, evaluated_at DESC);

CREATE INDEX IF NOT EXISTS idx_strategy_evaluations_status_time
    ON strategy_evaluations(account_id, status, evaluated_at DESC);

CREATE INDEX IF NOT EXISTS idx_strategy_evaluations_deployment_stage_time
    ON strategy_evaluations(account_id, deployment_id, stage, evaluated_at DESC)
    WHERE deployment_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_strategy_evaluations_strategy_domain_timeframe_stage_time
    ON strategy_evaluations(account_id, strategy_id, domain, timeframe, stage, evaluated_at DESC);

CREATE UNIQUE INDEX IF NOT EXISTS idx_strategy_evaluations_evidence_hash
    ON strategy_evaluations(account_id, strategy_id, stage, evidence_hash)
    WHERE evidence_hash IS NOT NULL;
