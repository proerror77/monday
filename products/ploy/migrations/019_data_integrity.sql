-- Migration 019: Data Integrity Fixes + Backtest Infrastructure
--
-- Addresses 6 HIGH-severity data integrity issues:
--   1. fills FK missing ON DELETE behaviour
--   2. DLQ retry_count guard + status transition validation
--   3. strategy_events version uniqueness
--   4. ticks duplicate LOB snapshot prevention
--   5. backtest_runs table for momentum backtesting
--   6. check_data_integrity() comprehensive health check
--

-- ============================================================
-- 1. fills: add ON DELETE SET NULL to order_id FK
-- ============================================================
-- The original FK in migration 006 used bare REFERENCES (defaults to RESTRICT),
-- which blocks order cleanup. SET NULL preserves fill history.
ALTER TABLE fills DROP CONSTRAINT IF EXISTS fills_order_id_fkey;
ALTER TABLE fills ADD CONSTRAINT fills_order_id_fkey
  FOREIGN KEY (order_id) REFERENCES orders(id) ON DELETE SET NULL;


-- ============================================================
-- 2. DLQ: retry guard + status transition trigger
-- ============================================================

-- 2a. Prevent retry_count from exceeding max_retries + 1
--     (+1 because count increments after the retry completes)
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
    WHERE conname = 'dlq_retry_count_guard'
      AND conrelid = 'dead_letter_queue'::regclass
  ) THEN
    ALTER TABLE dead_letter_queue
      ADD CONSTRAINT dlq_retry_count_guard
      CHECK (retry_count <= max_retries + 1);
  END IF;
END $$;

-- 2b. Trigger: forbid transitions FROM 'resolved' back to any other status
CREATE OR REPLACE FUNCTION validate_dlq_status_transition()
RETURNS TRIGGER AS $$
BEGIN
  IF OLD.status = 'resolved' AND NEW.status <> 'resolved' THEN
    RAISE EXCEPTION 'Cannot transition DLQ entry % from resolved to %',
      OLD.id, NEW.status;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_dlq_status_transition ON dead_letter_queue;
CREATE TRIGGER trg_dlq_status_transition
  BEFORE UPDATE OF status ON dead_letter_queue
  FOR EACH ROW
  EXECUTE FUNCTION validate_dlq_status_transition();


-- ============================================================
-- 3. strategy_events: enforce version uniqueness per aggregate
-- ============================================================
-- Event sourcing requires exactly one event per (aggregate, version).
-- Duplicate versions corrupt aggregate replay.
--
-- Safe: uses CREATE ... IF NOT EXISTS and DO block for pre-check.
DO $$
BEGIN
  -- Check for existing duplicates before creating unique index
  IF EXISTS (
    SELECT aggregate_id, aggregate_type, event_version, COUNT(*)
    FROM strategy_events
    GROUP BY aggregate_id, aggregate_type, event_version
    HAVING COUNT(*) > 1
  ) THEN
    RAISE WARNING 'Duplicate event versions found in strategy_events — deduplicating';
    -- Keep only the earliest event per (aggregate_id, aggregate_type, event_version)
    DELETE FROM strategy_events
    WHERE id NOT IN (
      SELECT MIN(id)
      FROM strategy_events
      GROUP BY aggregate_id, aggregate_type, event_version
    );
  END IF;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS idx_strategy_events_version_unique
  ON strategy_events(aggregate_id, aggregate_type, event_version);


-- ============================================================
-- 4. ticks: prevent duplicate LOB snapshots
-- ============================================================
-- Duplicate (round_id, side, timestamp) rows waste storage and
-- corrupt LOB replay analysis.
DO $$
BEGIN
  -- Check for existing duplicates before creating unique index
  IF EXISTS (
    SELECT round_id, side, "timestamp", COUNT(*)
    FROM ticks
    GROUP BY round_id, side, "timestamp"
    HAVING COUNT(*) > 1
  ) THEN
    RAISE WARNING 'Duplicate ticks found — deduplicating';
    DELETE FROM ticks
    WHERE id NOT IN (
      SELECT MIN(id)
      FROM ticks
      GROUP BY round_id, side, "timestamp"
    );
  END IF;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS idx_ticks_round_side_time_unique
  ON ticks(round_id, side, "timestamp");


-- ============================================================
-- 5. backtest_runs: persistent backtest results
-- ============================================================
CREATE TABLE IF NOT EXISTS backtest_runs (
  id            BIGSERIAL     PRIMARY KEY,
  account_id    TEXT          NOT NULL DEFAULT 'default',
  evaluation_id BIGINT        REFERENCES strategy_evaluations(id) ON DELETE SET NULL,
  strategy_id   TEXT          NOT NULL,
  config_hash   TEXT          NOT NULL,
  config_json   JSONB         NOT NULL,
  started_at    TIMESTAMPTZ   NOT NULL,
  completed_at  TIMESTAMPTZ,
  data_range_start TIMESTAMPTZ,
  data_range_end   TIMESTAMPTZ,
  total_trades     INT,
  winning_trades   INT,
  losing_trades    INT,
  win_rate         NUMERIC(12,6),
  total_pnl        NUMERIC(20,10),
  sharpe_ratio     NUMERIC(12,6),
  max_drawdown_pct NUMERIC(12,6),
  max_drawdown_usd NUMERIC(20,10),
  profit_factor    NUMERIC(12,6),
  avg_trade_pnl    NUMERIC(20,10),
  avg_holding_secs BIGINT,
  equity_curve     JSONB,
  trades           JSONB,
  metadata         JSONB
);

CREATE INDEX IF NOT EXISTS idx_backtest_runs_account_time
  ON backtest_runs(account_id, started_at DESC);

CREATE INDEX IF NOT EXISTS idx_backtest_runs_strategy_time
  ON backtest_runs(strategy_id, started_at DESC);

CREATE INDEX IF NOT EXISTS idx_backtest_runs_config_hash
  ON backtest_runs(config_hash);


-- ============================================================
-- 6. check_data_integrity() — comprehensive health check
-- ============================================================
CREATE OR REPLACE FUNCTION check_data_integrity()
RETURNS JSONB AS $$
DECLARE
  result JSONB := '{}'::JSONB;
  v_count BIGINT;
  v_healthy BOOLEAN := TRUE;
BEGIN

  -- 6a. Duplicate ticks (should be zero after unique index)
  SELECT COUNT(*) INTO v_count
  FROM (
    SELECT round_id, side, "timestamp"
    FROM ticks
    GROUP BY round_id, side, "timestamp"
    HAVING COUNT(*) > 1
  ) dup;
  result := result || jsonb_build_object(
    'duplicate_ticks', jsonb_build_object('count', v_count, 'ok', v_count = 0)
  );
  IF v_count > 0 THEN v_healthy := FALSE; END IF;

  -- 6b. Orphaned fills (order_id not null but references deleted order)
  SELECT COUNT(*) INTO v_count
  FROM fills f
  WHERE f.order_id IS NOT NULL
    AND NOT EXISTS (SELECT 1 FROM orders o WHERE o.id = f.order_id);
  result := result || jsonb_build_object(
    'orphaned_fills', jsonb_build_object('count', v_count, 'ok', v_count = 0)
  );
  IF v_count > 0 THEN v_healthy := FALSE; END IF;

  -- 6c. DLQ over-retry entries (retry_count > max_retries + 1)
  SELECT COUNT(*) INTO v_count
  FROM dead_letter_queue
  WHERE retry_count > max_retries + 1;
  result := result || jsonb_build_object(
    'dlq_over_retry', jsonb_build_object('count', v_count, 'ok', v_count = 0)
  );
  IF v_count > 0 THEN v_healthy := FALSE; END IF;

  -- 6d. Duplicate event versions
  SELECT COUNT(*) INTO v_count
  FROM (
    SELECT aggregate_id, aggregate_type, event_version
    FROM strategy_events
    GROUP BY aggregate_id, aggregate_type, event_version
    HAVING COUNT(*) > 1
  ) dup;
  result := result || jsonb_build_object(
    'duplicate_event_versions', jsonb_build_object('count', v_count, 'ok', v_count = 0)
  );
  IF v_count > 0 THEN v_healthy := FALSE; END IF;

  -- 6e. Stale open positions (opened > 7 days ago, no activity)
  SELECT COUNT(*) INTO v_count
  FROM positions
  WHERE status = 'OPEN'
    AND opened_at < NOW() - INTERVAL '7 days';
  result := result || jsonb_build_object(
    'stale_open_positions', jsonb_build_object('count', v_count, 'ok', v_count = 0)
  );
  -- Stale positions are a warning, not a hard failure
  -- (intentional long-hold strategies exist)

  -- 6f. Unresolved discrepancies older than 24h
  SELECT COUNT(*) INTO v_count
  FROM position_discrepancies
  WHERE resolved = FALSE
    AND created_at < NOW() - INTERVAL '24 hours';
  result := result || jsonb_build_object(
    'unresolved_discrepancies_24h', jsonb_build_object('count', v_count, 'ok', v_count = 0)
  );
  IF v_count > 0 THEN v_healthy := FALSE; END IF;

  -- Final verdict
  result := result || jsonb_build_object('healthy', v_healthy);

  RETURN result;
END;
$$ LANGUAGE plpgsql;


-- ============================================================
-- Optional: grant to ploy role if it exists
-- ============================================================
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ploy') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON backtest_runs TO ploy;
    GRANT USAGE, SELECT ON SEQUENCE backtest_runs_id_seq TO ploy;
  END IF;
END $$;
