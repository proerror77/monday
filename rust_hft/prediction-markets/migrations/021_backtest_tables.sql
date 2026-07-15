-- Migration: 021_backtest_tables
-- Purpose: Backtest run metadata, signals, and closed trades.
--
-- NOTE:
-- `backtest_runs` may already exist from migration 019 with a different schema
-- (id/evaluation_id/strategy_id...). This migration reconciles that table so
-- both legacy (019) and report-oriented (021) code paths can coexist.

-- 1) Reconcile/Create backtest_runs metadata table.
DO $$
BEGIN
    IF to_regclass('public.backtest_runs') IS NULL THEN
        CREATE TABLE backtest_runs (
            run_id        UUID PRIMARY KEY DEFAULT (md5(random()::text || clock_timestamp()::text)::uuid),
            strategy      TEXT NOT NULL,
            mode          TEXT NOT NULL,
            config_json   JSONB NOT NULL,
            symbols       TEXT[] NOT NULL,
            data_start    TIMESTAMPTZ,
            data_end      TIMESTAMPTZ,
            total_trades  INT,
            win_rate      DOUBLE PRECISION,
            total_pnl     NUMERIC,
            sharpe_ratio  DOUBLE PRECISION,
            max_drawdown  NUMERIC,
            profit_factor DOUBLE PRECISION,
            created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );
    END IF;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS run_id UUID;
    UPDATE backtest_runs
    SET run_id = md5(random()::text || clock_timestamp()::text)::uuid
    WHERE run_id IS NULL;
    ALTER TABLE backtest_runs
        ALTER COLUMN run_id SET DEFAULT (md5(random()::text || clock_timestamp()::text)::uuid);
    ALTER TABLE backtest_runs ALTER COLUMN run_id SET NOT NULL;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS strategy TEXT;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'backtest_runs' AND column_name = 'strategy_id'
    ) THEN
        UPDATE backtest_runs
        SET strategy = COALESCE(strategy, strategy_id)
        WHERE strategy IS NULL;
    END IF;
    UPDATE backtest_runs SET strategy = 'unknown' WHERE strategy IS NULL;
    ALTER TABLE backtest_runs ALTER COLUMN strategy SET NOT NULL;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS mode TEXT;
    UPDATE backtest_runs SET mode = 'backtest' WHERE mode IS NULL;
    ALTER TABLE backtest_runs ALTER COLUMN mode SET NOT NULL;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS config_json JSONB;
    UPDATE backtest_runs SET config_json = '{}'::jsonb WHERE config_json IS NULL;
    ALTER TABLE backtest_runs ALTER COLUMN config_json SET NOT NULL;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS symbols TEXT[];
    UPDATE backtest_runs SET symbols = ARRAY[]::TEXT[] WHERE symbols IS NULL;
    ALTER TABLE backtest_runs ALTER COLUMN symbols SET NOT NULL;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS data_start TIMESTAMPTZ;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'backtest_runs' AND column_name = 'data_range_start'
    ) THEN
        UPDATE backtest_runs
        SET data_start = COALESCE(data_start, data_range_start)
        WHERE data_start IS NULL;
    END IF;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS data_end TIMESTAMPTZ;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'backtest_runs' AND column_name = 'data_range_end'
    ) THEN
        UPDATE backtest_runs
        SET data_end = COALESCE(data_end, data_range_end)
        WHERE data_end IS NULL;
    END IF;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS max_drawdown NUMERIC;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'backtest_runs' AND column_name = 'max_drawdown_usd'
    ) THEN
        UPDATE backtest_runs
        SET max_drawdown = COALESCE(max_drawdown, max_drawdown_usd)
        WHERE max_drawdown IS NULL;
    END IF;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'backtest_runs' AND column_name = 'max_drawdown_pct'
    ) THEN
        UPDATE backtest_runs
        SET max_drawdown = COALESCE(max_drawdown, max_drawdown_pct)
        WHERE max_drawdown IS NULL;
    END IF;

    ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'backtest_runs' AND column_name = 'started_at'
    ) THEN
        UPDATE backtest_runs
        SET created_at = COALESCE(created_at, started_at)
        WHERE created_at IS NULL;
    END IF;
    UPDATE backtest_runs SET created_at = NOW() WHERE created_at IS NULL;
    ALTER TABLE backtest_runs ALTER COLUMN created_at SET NOT NULL;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS idx_bt_runs_run_id_unique ON backtest_runs(run_id);
CREATE INDEX IF NOT EXISTS idx_bt_runs_created_at ON backtest_runs(created_at DESC);

-- 2) Backtest signals (entry + exit + filtered)
CREATE TABLE IF NOT EXISTS backtest_signals (
    id            BIGSERIAL PRIMARY KEY,
    run_id        UUID NOT NULL REFERENCES backtest_runs(run_id),
    signal_type   TEXT NOT NULL,          -- 'entry', 'exit', 'filtered'
    symbol        TEXT NOT NULL,
    direction     TEXT NOT NULL,          -- 'UP', 'DOWN'
    timestamp     TIMESTAMPTZ NOT NULL,
    p_hat         DOUBLE PRECISION,
    ev_net        DOUBLE PRECISION,
    sigma         DOUBLE PRECISION,
    market_price  NUMERIC,
    spot_price    NUMERIC,
    s0            NUMERIC,
    time_remaining_secs DOUBLE PRECISION,
    filter_reason TEXT,                   -- for 'filtered': cooldown/max_positions/price bounds...
    exit_reason   TEXT,                   -- for 'exit': settlement/time_stop/hard_stop/prob_stop
    exit_price    NUMERIC,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_bt_signals_run ON backtest_signals(run_id);

-- 3) Backtest closed trades
CREATE TABLE IF NOT EXISTS backtest_trades (
    id            BIGSERIAL PRIMARY KEY,
    run_id        UUID NOT NULL REFERENCES backtest_runs(run_id),
    symbol        TEXT NOT NULL,
    direction     TEXT NOT NULL,
    entry_time    TIMESTAMPTZ NOT NULL,
    exit_time     TIMESTAMPTZ NOT NULL,
    entry_price   NUMERIC NOT NULL,
    exit_price    NUMERIC NOT NULL,
    shares        INT NOT NULL,
    pnl           NUMERIC NOT NULL,
    won           BOOLEAN NOT NULL,
    holding_secs  BIGINT NOT NULL,
    exit_reason   TEXT NOT NULL,
    entry_p_hat   DOUBLE PRECISION,
    entry_ev_net  DOUBLE PRECISION,
    entry_sigma   DOUBLE PRECISION,
    s0            NUMERIC,
    -- Gamma settlement dual verification
    gamma_settled_price NUMERIC,
    gamma_resolved      BOOLEAN,
    gamma_match         BOOLEAN,          -- tick outcome vs Gamma agreement
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_bt_trades_run ON backtest_trades(run_id);
