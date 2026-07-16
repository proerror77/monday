-- Migration: 006_position_management
-- Description: Position persistence and reconciliation infrastructure
-- Part of: Phase 2 - Position Management

-- ============================================================================
-- 1. POSITION PERSISTENCE
-- ============================================================================

-- Positions table for persistent position tracking
CREATE TABLE IF NOT EXISTS positions (
    id SERIAL PRIMARY KEY,
    event_id TEXT NOT NULL,
    symbol TEXT NOT NULL,
    token_id TEXT NOT NULL,
    market_side TEXT NOT NULL CHECK (market_side IN ('UP', 'DOWN')),
    shares BIGINT NOT NULL,
    avg_entry_price DECIMAL(10,6) NOT NULL,
    amount_usd DECIMAL(18,8) NOT NULL,
    opened_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    closed_at TIMESTAMPTZ,
    status TEXT NOT NULL DEFAULT 'OPEN' CHECK (status IN ('OPEN', 'CLOSED')),
    pnl DECIMAL(18,8),
    exit_price DECIMAL(10,6),
    strategy_id TEXT,
    UNIQUE(event_id, token_id)
);

-- Indexes for position queries
CREATE INDEX IF NOT EXISTS idx_positions_status
    ON positions(status)
    WHERE status = 'OPEN';

CREATE INDEX IF NOT EXISTS idx_positions_symbol
    ON positions(symbol, status);

CREATE INDEX IF NOT EXISTS idx_positions_strategy
    ON positions(strategy_id, status)
    WHERE strategy_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_positions_opened
    ON positions(opened_at DESC);

-- ============================================================================
-- 2. POSITION RECONCILIATION
-- ============================================================================

-- Reconciliation log for tracking position sync with exchange
CREATE TABLE IF NOT EXISTS position_reconciliation_log (
    id SERIAL PRIMARY KEY,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    discrepancies_found INT NOT NULL DEFAULT 0,
    auto_corrections INT NOT NULL DEFAULT 0,
    details JSONB,
    duration_ms INT
);

CREATE INDEX IF NOT EXISTS idx_reconciliation_timestamp
    ON position_reconciliation_log(timestamp DESC);

-- Position discrepancies table for detailed tracking
CREATE TABLE IF NOT EXISTS position_discrepancies (
    id SERIAL PRIMARY KEY,
    reconciliation_id INT REFERENCES position_reconciliation_log(id),
    token_id TEXT NOT NULL,
    local_shares BIGINT NOT NULL,
    exchange_shares BIGINT NOT NULL,
    difference BIGINT NOT NULL,
    severity TEXT NOT NULL CHECK (severity IN ('INFO', 'WARNING', 'CRITICAL')),
    resolved BOOLEAN NOT NULL DEFAULT FALSE,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_discrepancies_unresolved
    ON position_discrepancies(resolved, created_at DESC)
    WHERE resolved = FALSE;

CREATE INDEX IF NOT EXISTS idx_discrepancies_token
    ON position_discrepancies(token_id, created_at DESC);

-- ============================================================================
-- 3. FILL TRACKING
-- ============================================================================

-- Fills table for detailed execution history
CREATE TABLE IF NOT EXISTS fills (
    id SERIAL PRIMARY KEY,
    order_id INT REFERENCES orders(id),
    position_id INT REFERENCES positions(id),
    trade_id TEXT NOT NULL UNIQUE,
    price DECIMAL(10,6) NOT NULL,
    shares INT NOT NULL,
    fee DECIMAL(18,8) NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_fills_order
    ON fills(order_id);

CREATE INDEX IF NOT EXISTS idx_fills_position
    ON fills(position_id);

CREATE INDEX IF NOT EXISTS idx_fills_timestamp
    ON fills(timestamp DESC);

-- ============================================================================
-- 4. BALANCE SNAPSHOTS
-- ============================================================================

-- Balance snapshots for tracking balance history
CREATE TABLE IF NOT EXISTS balance_snapshots (
    id SERIAL PRIMARY KEY,
    balance DECIMAL(18,8) NOT NULL,
    available DECIMAL(18,8) NOT NULL,
    reserved DECIMAL(18,8) NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_balance_snapshots_timestamp
    ON balance_snapshots(timestamp DESC);

-- ============================================================================
-- 5. HELPER FUNCTIONS
-- ============================================================================

-- Function to get open positions
CREATE OR REPLACE FUNCTION get_open_positions()
RETURNS TABLE (
    position_id INT,
    event_id TEXT,
    symbol TEXT,
    token_id TEXT,
    market_side TEXT,
    shares BIGINT,
    avg_entry_price DECIMAL,
    amount_usd DECIMAL,
    opened_at TIMESTAMPTZ,
    strategy_id TEXT
) AS $$
BEGIN
    RETURN QUERY
    SELECT
        p.id,
        p.event_id,
        p.symbol,
        p.token_id,
        p.market_side,
        p.shares,
        p.avg_entry_price,
        p.amount_usd,
        p.opened_at,
        p.strategy_id
    FROM positions p
    WHERE p.status = 'OPEN'
    ORDER BY p.opened_at DESC;
END;
$$ LANGUAGE plpgsql;

-- Function to close position
CREATE OR REPLACE FUNCTION close_position(
    p_position_id INT,
    p_exit_price DECIMAL,
    p_pnl DECIMAL
) RETURNS VOID AS $$
BEGIN
    UPDATE positions
    SET status = 'CLOSED',
        closed_at = NOW(),
        exit_price = p_exit_price,
        pnl = p_pnl
    WHERE id = p_position_id
      AND status = 'OPEN';

    IF NOT FOUND THEN
        RAISE EXCEPTION 'Position % not found or already closed', p_position_id;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Function to get position summary
CREATE OR REPLACE FUNCTION get_position_summary()
RETURNS TABLE (
    total_open INT,
    total_closed INT,
    total_pnl DECIMAL,
    avg_pnl DECIMAL,
    win_rate DECIMAL
) AS $$
BEGIN
    RETURN QUERY
    SELECT
        COUNT(*) FILTER (WHERE status = 'OPEN')::INT as total_open,
        COUNT(*) FILTER (WHERE status = 'CLOSED')::INT as total_closed,
        COALESCE(SUM(pnl) FILTER (WHERE status = 'CLOSED'), 0) as total_pnl,
        COALESCE(AVG(pnl) FILTER (WHERE status = 'CLOSED'), 0) as avg_pnl,
        CASE
            WHEN COUNT(*) FILTER (WHERE status = 'CLOSED') > 0 THEN
                COUNT(*) FILTER (WHERE status = 'CLOSED' AND pnl > 0)::DECIMAL /
                COUNT(*) FILTER (WHERE status = 'CLOSED')::DECIMAL
            ELSE 0
        END as win_rate
    FROM positions;
END;
$$ LANGUAGE plpgsql;

-- Function to record reconciliation
CREATE OR REPLACE FUNCTION record_reconciliation(
    p_discrepancies_found INT,
    p_auto_corrections INT,
    p_details JSONB,
    p_duration_ms INT
) RETURNS INT AS $$
DECLARE
    v_id INT;
BEGIN
    INSERT INTO position_reconciliation_log (
        discrepancies_found,
        auto_corrections,
        details,
        duration_ms
    ) VALUES (
        p_discrepancies_found,
        p_auto_corrections,
        p_details,
        p_duration_ms
    ) RETURNING id INTO v_id;

    RETURN v_id;
END;
$$ LANGUAGE plpgsql;

-- Function to cleanup old reconciliation logs
CREATE OR REPLACE FUNCTION cleanup_old_reconciliation_logs(
    p_days_to_keep INT DEFAULT 30
) RETURNS INT AS $$
DECLARE
    deleted_count INT;
BEGIN
    DELETE FROM position_reconciliation_log
    WHERE timestamp < NOW() - (p_days_to_keep || ' days')::INTERVAL;

    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 6. VIEWS FOR MONITORING
-- ============================================================================

-- View for position statistics
CREATE OR REPLACE VIEW v_position_stats AS
SELECT
    status,
    COUNT(*) as count,
    SUM(amount_usd) as total_amount_usd,
    AVG(amount_usd) as avg_amount_usd,
    SUM(pnl) FILTER (WHERE status = 'CLOSED') as total_pnl,
    AVG(pnl) FILTER (WHERE status = 'CLOSED') as avg_pnl,
    COUNT(*) FILTER (WHERE pnl > 0) as winning_positions,
    COUNT(*) FILTER (WHERE pnl < 0) as losing_positions
FROM positions
GROUP BY status;

-- View for recent reconciliations
CREATE OR REPLACE VIEW v_recent_reconciliations AS
SELECT
    id,
    timestamp,
    discrepancies_found,
    auto_corrections,
    duration_ms,
    CASE
        WHEN discrepancies_found = 0 THEN 'CLEAN'
        WHEN auto_corrections = discrepancies_found THEN 'AUTO_FIXED'
        ELSE 'NEEDS_ATTENTION'
    END as status
FROM position_reconciliation_log
ORDER BY timestamp DESC
LIMIT 100;

-- View for unresolved discrepancies
CREATE OR REPLACE VIEW v_unresolved_discrepancies AS
SELECT
    d.id,
    d.token_id,
    d.local_shares,
    d.exchange_shares,
    d.difference,
    d.severity,
    d.created_at,
    EXTRACT(EPOCH FROM (NOW() - d.created_at)) / 3600 as hours_unresolved
FROM position_discrepancies d
WHERE d.resolved = FALSE
ORDER BY d.severity DESC, d.created_at ASC;

-- ============================================================================
-- COMMENTS
-- ============================================================================

COMMENT ON TABLE positions IS 'Persistent position tracking with full lifecycle';
COMMENT ON TABLE position_reconciliation_log IS 'Log of position reconciliation runs';
COMMENT ON TABLE position_discrepancies IS 'Detailed tracking of position mismatches';
COMMENT ON TABLE fills IS 'Detailed execution history for orders';
COMMENT ON TABLE balance_snapshots IS 'Historical balance tracking';

COMMENT ON FUNCTION get_open_positions IS 'Get all currently open positions';
COMMENT ON FUNCTION close_position IS 'Close a position with exit price and PnL';
COMMENT ON FUNCTION get_position_summary IS 'Get summary statistics for all positions';
COMMENT ON FUNCTION record_reconciliation IS 'Record a reconciliation run';
COMMENT ON FUNCTION cleanup_old_reconciliation_logs IS 'Remove old reconciliation logs';
