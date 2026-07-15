-- Security and Idempotency Enhancement Migration
-- Addresses critical security vulnerabilities identified in audit

-- ============================================================================
-- 1. ORDER IDEMPOTENCY TABLE
-- ============================================================================
-- Prevents duplicate order submissions during retry scenarios
-- Uses deterministic hash of order parameters for duplicate detection

CREATE TABLE IF NOT EXISTS order_idempotency (
    id SERIAL PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    order_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('pending', 'completed', 'failed')),
    response_data JSONB,
    error_message TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_order_idempotency_key ON order_idempotency(idempotency_key);
CREATE INDEX IF NOT EXISTS idx_order_idempotency_hash ON order_idempotency(request_hash);
CREATE INDEX IF NOT EXISTS idx_order_idempotency_expires ON order_idempotency(expires_at);
CREATE INDEX IF NOT EXISTS idx_order_idempotency_status ON order_idempotency(status);

-- Cleanup function for expired idempotency keys
CREATE OR REPLACE FUNCTION cleanup_expired_idempotency_keys()
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM order_idempotency
    WHERE expires_at < NOW();

    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 2. OPTIMISTIC LOCKING FOR CYCLES
-- ============================================================================
-- Prevents race conditions in state transitions
-- Uses version number to detect concurrent modifications

ALTER TABLE cycles ADD COLUMN IF NOT EXISTS version INT NOT NULL DEFAULT 1;
CREATE INDEX IF NOT EXISTS idx_cycles_version ON cycles(id, version);

-- Function to update cycle with version check
CREATE OR REPLACE FUNCTION update_cycle_with_version(
    p_cycle_id INT,
    p_expected_version INT,
    p_new_state TEXT,
    p_leg1_side TEXT DEFAULT NULL,
    p_leg1_entry_price DECIMAL DEFAULT NULL,
    p_leg1_shares INT DEFAULT NULL,
    p_leg1_filled_at TIMESTAMPTZ DEFAULT NULL,
    p_leg2_entry_price DECIMAL DEFAULT NULL,
    p_leg2_shares INT DEFAULT NULL,
    p_leg2_filled_at TIMESTAMPTZ DEFAULT NULL,
    p_pnl DECIMAL DEFAULT NULL,
    p_abort_reason TEXT DEFAULT NULL
)
RETURNS BOOLEAN AS $$
DECLARE
    rows_affected INT;
BEGIN
    UPDATE cycles
    SET
        state = p_new_state,
        version = version + 1,
        leg1_side = COALESCE(p_leg1_side, leg1_side),
        leg1_entry_price = COALESCE(p_leg1_entry_price, leg1_entry_price),
        leg1_shares = COALESCE(p_leg1_shares, leg1_shares),
        leg1_filled_at = COALESCE(p_leg1_filled_at, leg1_filled_at),
        leg2_entry_price = COALESCE(p_leg2_entry_price, leg2_entry_price),
        leg2_shares = COALESCE(p_leg2_shares, leg2_shares),
        leg2_filled_at = COALESCE(p_leg2_filled_at, leg2_filled_at),
        pnl = COALESCE(p_pnl, pnl),
        abort_reason = COALESCE(p_abort_reason, abort_reason),
        updated_at = NOW()
    WHERE id = p_cycle_id AND version = p_expected_version;

    GET DIAGNOSTICS rows_affected = ROW_COUNT;
    RETURN rows_affected > 0;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 3. QUOTE FRESHNESS TRACKING
-- ============================================================================
-- Tracks quote age to prevent trading on stale data
-- Enforces maximum quote age at trade time

CREATE TABLE IF NOT EXISTS quote_freshness (
    id SERIAL PRIMARY KEY,
    token_id TEXT NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('UP', 'DOWN')),
    best_bid DECIMAL(10,6),
    best_ask DECIMAL(10,6),
    bid_size DECIMAL(18,8),
    ask_size DECIMAL(18,8),
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Generated columns require immutable expressions in Postgres.
    -- `NOW()` is volatile, so keep this as a normal column and refresh it
    -- from `received_at` (see repair/update blocks below).
    is_stale BOOLEAN NOT NULL DEFAULT FALSE
);

-- Legacy/drift-safe repair: older DBs may have quote_freshness without `is_stale`.
DO $$
BEGIN
    IF to_regclass('public.quote_freshness') IS NULL THEN
        RETURN;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = 'public'
          AND table_name = 'quote_freshness'
          AND column_name = 'is_stale'
    ) THEN
        EXECUTE 'ALTER TABLE quote_freshness ADD COLUMN is_stale BOOLEAN NOT NULL DEFAULT FALSE';
    END IF;

    -- If `is_stale` is a normal column, refresh values from `received_at`.
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = 'public'
          AND table_name = 'quote_freshness'
          AND column_name = 'is_stale'
          AND is_generated = 'NEVER'
    ) THEN
        EXECUTE 'UPDATE quote_freshness SET is_stale = (EXTRACT(EPOCH FROM (NOW() - received_at)) > 30)
                 WHERE is_stale IS DISTINCT FROM (EXTRACT(EPOCH FROM (NOW() - received_at)) > 30)';
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_quote_freshness_token ON quote_freshness(token_id, side);
CREATE INDEX IF NOT EXISTS idx_quote_freshness_received ON quote_freshness(received_at DESC);
CREATE INDEX IF NOT EXISTS idx_quote_freshness_stale ON quote_freshness(is_stale) WHERE is_stale = false;

-- Function to get fresh quote (< 30 seconds old)
CREATE OR REPLACE FUNCTION get_fresh_quote(
    p_token_id TEXT,
    p_side TEXT,
    p_max_age_seconds INT DEFAULT 30
)
RETURNS TABLE (
    best_bid DECIMAL(10,6),
    best_ask DECIMAL(10,6),
    bid_size DECIMAL(18,8),
    ask_size DECIMAL(18,8),
    age_seconds NUMERIC
) AS $$
BEGIN
    RETURN QUERY
    SELECT
        qf.best_bid,
        qf.best_ask,
        qf.bid_size,
        qf.ask_size,
        EXTRACT(EPOCH FROM (NOW() - qf.received_at)) as age_seconds
    FROM quote_freshness qf
    WHERE qf.token_id = p_token_id
      AND qf.side = p_side
      AND EXTRACT(EPOCH FROM (NOW() - qf.received_at)) <= p_max_age_seconds
    ORDER BY qf.received_at DESC
    LIMIT 1;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 4. NONCE MANAGEMENT
-- ============================================================================
-- Persistent nonce tracking for exchange API calls
-- Prevents nonce collisions after restart

CREATE TABLE IF NOT EXISTS nonce_state (
    id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),  -- Singleton
    current_nonce BIGINT NOT NULL DEFAULT 0,
    last_updated TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Initialize nonce state
INSERT INTO nonce_state (current_nonce)
VALUES (EXTRACT(EPOCH FROM NOW())::BIGINT * 1000)
ON CONFLICT (id) DO NOTHING;

-- Function to get next nonce atomically
CREATE OR REPLACE FUNCTION get_next_nonce()
RETURNS BIGINT AS $$
DECLARE
    next_nonce BIGINT;
BEGIN
    UPDATE nonce_state
    SET current_nonce = current_nonce + 1,
        last_updated = NOW()
    WHERE id = 1
    RETURNING current_nonce INTO next_nonce;

    RETURN next_nonce;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 5. AUDIT TRAIL FOR SECURITY EVENTS
-- ============================================================================
-- Comprehensive logging of security-relevant events

CREATE TABLE IF NOT EXISTS security_audit_log (
    id BIGSERIAL PRIMARY KEY,
    event_type TEXT NOT NULL,
    severity TEXT NOT NULL CHECK (severity IN ('INFO', 'WARNING', 'ERROR', 'CRITICAL')),
    component TEXT NOT NULL,
    message TEXT NOT NULL,
    metadata JSONB,
    user_id TEXT,
    ip_address INET,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_security_audit_timestamp ON security_audit_log(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_security_audit_event_type ON security_audit_log(event_type);
CREATE INDEX IF NOT EXISTS idx_security_audit_severity ON security_audit_log(severity);
CREATE INDEX IF NOT EXISTS idx_security_audit_component ON security_audit_log(component);

-- Function to log security event
CREATE OR REPLACE FUNCTION log_security_event(
    p_event_type TEXT,
    p_severity TEXT,
    p_component TEXT,
    p_message TEXT,
    p_metadata JSONB DEFAULT NULL
)
RETURNS VOID AS $$
BEGIN
    INSERT INTO security_audit_log (event_type, severity, component, message, metadata)
    VALUES (p_event_type, p_severity, p_component, p_message, p_metadata);
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 6. APPLY UPDATED_AT TRIGGER TO NEW TABLES
-- ============================================================================

DO $$
BEGIN
    IF to_regclass('public.order_idempotency') IS NULL THEN
        RETURN;
    END IF;

    DROP TRIGGER IF EXISTS update_order_idempotency_updated_at ON order_idempotency;
    CREATE TRIGGER update_order_idempotency_updated_at
    BEFORE UPDATE ON order_idempotency
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN undefined_function THEN
    NULL;
END $$;

-- ============================================================================
-- 7. MIGRATION METADATA
-- ============================================================================

DO $$
BEGIN
    IF to_regclass('public.system_events') IS NULL THEN
        RETURN;
    END IF;

    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = 'public'
          AND table_name = 'system_events'
          AND column_name = 'component'
    ) THEN
        INSERT INTO system_events (event_type, component, severity, message, metadata)
        VALUES (
            'migration_applied',
            'migration',
            'INFO',
            'Applied migration 005: Security and Idempotency Enhancement',
            jsonb_build_object(
                'migration_version', '005',
                'features', jsonb_build_array(
                    'order_idempotency',
                    'optimistic_locking',
                    'quote_freshness',
                    'nonce_management',
                    'security_audit_log'
                ),
                'applied_at', NOW()
            )
        );
    ELSE
        INSERT INTO system_events (event_type, severity, message, metadata)
        VALUES (
            'migration_applied',
            'INFO',
            'Applied migration 005: Security and Idempotency Enhancement',
            jsonb_build_object(
                'migration_version', '005',
                'features', jsonb_build_array(
                    'order_idempotency',
                    'optimistic_locking',
                    'quote_freshness',
                    'nonce_management',
                    'security_audit_log'
                ),
                'applied_at', NOW()
            )
        );
    END IF;
END $$;

-- ============================================================================
-- VERIFICATION QUERIES (for testing)
-- ============================================================================

-- Verify idempotency table
-- SELECT COUNT(*) FROM order_idempotency;

-- Verify optimistic locking
-- SELECT id, version FROM cycles LIMIT 5;

-- Verify quote freshness
-- SELECT * FROM get_fresh_quote('test_token', 'UP', 30);

-- Verify nonce management
-- SELECT get_next_nonce();

-- Verify security audit log
-- SELECT * FROM security_audit_log ORDER BY timestamp DESC LIMIT 10;
