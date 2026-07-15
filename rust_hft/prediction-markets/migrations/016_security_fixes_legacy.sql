-- Migration: 016_security_fixes_legacy
-- Description: Security fixes for duplicate orders, race conditions, and nonce management
-- Part of: Phase 1 - Critical Security Fixes

-- ============================================================================
-- 1. IDEMPOTENCY MANAGEMENT (Fix Duplicate Order Submission)
-- ============================================================================

-- Idempotency keys for order deduplication
CREATE TABLE IF NOT EXISTS order_idempotency (
    idempotency_key TEXT PRIMARY KEY,
    order_id TEXT,
    request_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'completed', 'failed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    response_data JSONB,
    error_message TEXT
);

-- Index for cleanup of expired keys
CREATE INDEX IF NOT EXISTS idx_idempotency_expires
    ON order_idempotency(expires_at)
    WHERE status IN ('completed', 'failed');

-- Index for order lookup
CREATE INDEX IF NOT EXISTS idx_idempotency_order_id
    ON order_idempotency(order_id)
    WHERE order_id IS NOT NULL;

-- Index for status queries
CREATE INDEX IF NOT EXISTS idx_idempotency_status
    ON order_idempotency(status, created_at);

-- Function to cleanup expired idempotency keys
CREATE OR REPLACE FUNCTION cleanup_expired_idempotency_keys()
RETURNS INT AS $$
DECLARE
    deleted_count INT;
BEGIN
    DELETE FROM order_idempotency
    WHERE expires_at < NOW();

    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 2. STATE TRANSITION MANAGEMENT (Fix Race Conditions)
-- ============================================================================

-- Add version column for optimistic locking
ALTER TABLE cycles ADD COLUMN IF NOT EXISTS version INT NOT NULL DEFAULT 1;

-- State transition audit log
CREATE TABLE IF NOT EXISTS state_transitions (
    id BIGSERIAL PRIMARY KEY,
    cycle_id INT REFERENCES cycles(id),
    from_state TEXT NOT NULL,
    to_state TEXT NOT NULL,
    transition_type TEXT NOT NULL,
    success BOOLEAN NOT NULL,
    error_message TEXT,
    metadata JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes for state transition queries
CREATE INDEX IF NOT EXISTS idx_transitions_cycle
    ON state_transitions(cycle_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_transitions_type
    ON state_transitions(transition_type, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_transitions_success
    ON state_transitions(success, created_at DESC)
    WHERE success = false;

-- Function to log state transitions
CREATE OR REPLACE FUNCTION log_state_transition(
    p_cycle_id INT,
    p_from_state TEXT,
    p_to_state TEXT,
    p_transition_type TEXT,
    p_success BOOLEAN,
    p_error_message TEXT DEFAULT NULL,
    p_metadata JSONB DEFAULT NULL
) RETURNS BIGINT AS $$
DECLARE
    v_id BIGINT;
BEGIN
    INSERT INTO state_transitions (
        cycle_id, from_state, to_state, transition_type,
        success, error_message, metadata
    ) VALUES (
        p_cycle_id, p_from_state, p_to_state, p_transition_type,
        p_success, p_error_message, p_metadata
    ) RETURNING id INTO v_id;

    RETURN v_id;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 3. NONCE MANAGEMENT (Fix Nonce Collisions)
-- ============================================================================

-- Nonce counter for each wallet
CREATE TABLE IF NOT EXISTS nonce_counter (
    wallet_address TEXT PRIMARY KEY,
    current_nonce BIGINT NOT NULL DEFAULT 0,
    last_used_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Nonce usage log for debugging and recovery
CREATE TABLE IF NOT EXISTS nonce_usage (
    id BIGSERIAL PRIMARY KEY,
    wallet_address TEXT NOT NULL,
    nonce BIGINT NOT NULL,
    order_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('allocated', 'used', 'failed', 'released')),
    allocated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    used_at TIMESTAMPTZ,
    released_at TIMESTAMPTZ,
    error_message TEXT,
    UNIQUE(wallet_address, nonce)
);

-- Indexes for nonce queries
CREATE INDEX IF NOT EXISTS idx_nonce_usage_wallet
    ON nonce_usage(wallet_address, nonce DESC);

CREATE INDEX IF NOT EXISTS idx_nonce_usage_status
    ON nonce_usage(status, allocated_at DESC);

CREATE INDEX IF NOT EXISTS idx_nonce_usage_order
    ON nonce_usage(order_id)
    WHERE order_id IS NOT NULL;

-- Function to get and increment nonce atomically
CREATE OR REPLACE FUNCTION get_next_nonce(p_wallet_address TEXT)
RETURNS BIGINT AS $$
DECLARE
    v_nonce BIGINT;
BEGIN
    -- Insert or update nonce counter atomically
    INSERT INTO nonce_counter (wallet_address, current_nonce, last_used_at)
    VALUES (p_wallet_address, 1, NOW())
    ON CONFLICT (wallet_address) DO UPDATE
    SET current_nonce = nonce_counter.current_nonce + 1,
        last_used_at = NOW()
    RETURNING current_nonce INTO v_nonce;

    -- Log allocation
    INSERT INTO nonce_usage (wallet_address, nonce, status)
    VALUES (p_wallet_address, v_nonce, 'allocated');

    RETURN v_nonce;
END;
$$ LANGUAGE plpgsql;

-- Function to mark nonce as used
CREATE OR REPLACE FUNCTION mark_nonce_used(
    p_wallet_address TEXT,
    p_nonce BIGINT,
    p_order_id TEXT
) RETURNS VOID AS $$
BEGIN
    UPDATE nonce_usage
    SET status = 'used',
        order_id = p_order_id,
        used_at = NOW()
    WHERE wallet_address = p_wallet_address
      AND nonce = p_nonce
      AND status = 'allocated';

    IF NOT FOUND THEN
        RAISE EXCEPTION 'Nonce % for wallet % not found or already used', p_nonce, p_wallet_address;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Function to release nonce (on order failure)
CREATE OR REPLACE FUNCTION release_nonce(
    p_wallet_address TEXT,
    p_nonce BIGINT,
    p_error_message TEXT
) RETURNS VOID AS $$
BEGIN
    UPDATE nonce_usage
    SET status = 'released',
        error_message = p_error_message,
        released_at = NOW()
    WHERE wallet_address = p_wallet_address
      AND nonce = p_nonce
      AND status = 'allocated';
END;
$$ LANGUAGE plpgsql;

-- Function to get current nonce (for recovery)
CREATE OR REPLACE FUNCTION get_current_nonce(p_wallet_address TEXT)
RETURNS BIGINT AS $$
DECLARE
    v_nonce BIGINT;
BEGIN
    SELECT current_nonce INTO v_nonce
    FROM nonce_counter
    WHERE wallet_address = p_wallet_address;

    IF NOT FOUND THEN
        RETURN 0;
    END IF;

    RETURN v_nonce;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 4. VIEWS FOR MONITORING
-- ============================================================================

-- View for idempotency statistics
CREATE OR REPLACE VIEW v_idempotency_stats AS
SELECT
    status,
    COUNT(*) as count,
    MIN(created_at) as oldest,
    MAX(created_at) as newest,
    COUNT(*) FILTER (WHERE expires_at < NOW()) as expired_count
FROM order_idempotency
GROUP BY status;

-- View for nonce usage statistics
CREATE OR REPLACE VIEW v_nonce_stats AS
SELECT
    wallet_address,
    COUNT(*) as total_allocations,
    COUNT(*) FILTER (WHERE status = 'used') as used_count,
    COUNT(*) FILTER (WHERE status = 'released') as released_count,
    COUNT(*) FILTER (WHERE status = 'allocated') as pending_count,
    MAX(nonce) as highest_nonce,
    MAX(allocated_at) as last_allocation
FROM nonce_usage
GROUP BY wallet_address;

-- View for state transition failures
CREATE OR REPLACE VIEW v_failed_transitions AS
SELECT
    cycle_id,
    from_state,
    to_state,
    transition_type,
    error_message,
    created_at
FROM state_transitions
WHERE success = false
ORDER BY created_at DESC;

-- ============================================================================
-- COMMENTS
-- ============================================================================

COMMENT ON TABLE order_idempotency IS 'Idempotency keys to prevent duplicate order submissions';
COMMENT ON TABLE state_transitions IS 'Audit log of all state machine transitions';
COMMENT ON TABLE nonce_counter IS 'Current nonce counter for each wallet address';
COMMENT ON TABLE nonce_usage IS 'Detailed log of nonce allocations and usage';

COMMENT ON FUNCTION get_next_nonce(TEXT) IS 'Atomically allocate next nonce for a wallet';
COMMENT ON FUNCTION mark_nonce_used(TEXT, BIGINT, TEXT) IS 'Mark nonce as successfully used in an order';
COMMENT ON FUNCTION release_nonce(TEXT, BIGINT, TEXT) IS 'Release nonce when order fails';
COMMENT ON FUNCTION cleanup_expired_idempotency_keys() IS 'Remove expired idempotency keys';
COMMENT ON FUNCTION log_state_transition(INT, TEXT, TEXT, TEXT, BOOLEAN, TEXT, JSONB) IS 'Log a state machine transition for audit';
