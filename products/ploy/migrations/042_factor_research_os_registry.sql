-- Migration 042: Factor Research OS registry and append-only trace.

CREATE TABLE IF NOT EXISTS factor_registry (
    factor_id UUID PRIMARY KEY,
    factor_name TEXT NOT NULL,
    factor_family TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('draft', 'compiled', 'evaluated', 'candidate', 'dry_run', 'approved', 'production', 'deprecated')),
    hypothesis TEXT NOT NULL,
    economic_logic TEXT NOT NULL DEFAULT '',
    dsl_source TEXT NOT NULL,
    dsl_hash TEXT NOT NULL,
    ast_json JSONB NOT NULL,
    target TEXT NOT NULL,
    horizon TEXT NOT NULL,
    created_by_agent TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    approved_by TEXT,
    approved_at TIMESTAMPTZ,
    deprecated_at TIMESTAMPTZ,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
);

-- A DSL hash can be reused across distinct targets or horizons. Migration 043
-- owns the durable uniqueness contract after cleaning historical duplicates.
CREATE INDEX IF NOT EXISTS idx_factor_registry_status_family
    ON factor_registry(status, factor_family, created_at DESC);

CREATE TABLE IF NOT EXISTS factor_evaluations (
    eval_id UUID PRIMARY KEY,
    factor_id UUID NOT NULL REFERENCES factor_registry(factor_id),
    run_id TEXT NOT NULL,
    data_snapshot_id TEXT NOT NULL,
    evaluator_version TEXT NOT NULL,
    train_ic DOUBLE PRECISION,
    valid_ic DOUBLE PRECISION,
    test_ic DOUBLE PRECISION,
    oos_ic DOUBLE PRECISION,
    rank_ic DOUBLE PRECISION,
    icir DOUBLE PRECISION,
    sharpe_gross DOUBLE PRECISION,
    sharpe_net DOUBLE PRECISION,
    max_drawdown DOUBLE PRECISION,
    turnover DOUBLE PRECISION,
    poly_ev DOUBLE PRECISION,
    poly_avg_fill DOUBLE PRECISION,
    poly_slippage DOUBLE PRECISION,
    poly_exit_capacity DOUBLE PRECISION,
    reward_total DOUBLE PRECISION,
    passed_gate BOOLEAN NOT NULL DEFAULT false,
    rejection_reason TEXT,
    metrics_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_factor_evaluations_factor_time
    ON factor_evaluations(factor_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_factor_evaluations_run
    ON factor_evaluations(run_id);

CREATE TABLE IF NOT EXISTS experiment_trace (
    trace_id UUID PRIMARY KEY,
    run_id TEXT NOT NULL,
    parent_trace_id UUID,
    event_type TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    input_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    output_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    hash_prev TEXT,
    hash_current TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_experiment_trace_run_time
    ON experiment_trace(run_id, created_at);
CREATE INDEX IF NOT EXISTS idx_experiment_trace_parent
    ON experiment_trace(parent_trace_id)
    WHERE parent_trace_id IS NOT NULL;

CREATE OR REPLACE FUNCTION prevent_experiment_trace_update()
RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'experiment_trace is append-only';
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION prevent_experiment_trace_delete()
RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'experiment_trace is append-only';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_experiment_trace_no_update ON experiment_trace;
CREATE TRIGGER trg_experiment_trace_no_update
    BEFORE UPDATE ON experiment_trace
    FOR EACH ROW EXECUTE FUNCTION prevent_experiment_trace_update();

DROP TRIGGER IF EXISTS trg_experiment_trace_no_delete ON experiment_trace;
CREATE TRIGGER trg_experiment_trace_no_delete
    BEFORE DELETE ON experiment_trace
    FOR EACH ROW EXECUTE FUNCTION prevent_experiment_trace_delete();
