-- Migration 002: Reliability Foundation
-- Purpose: Add tables for 24/7 production reliability

-- Dead Letter Queue for failed operations
-- Stores failed operations for retry with exponential backoff
CREATE TABLE IF NOT EXISTS dead_letter_queue (
    id BIGSERIAL PRIMARY KEY,
    operation_type TEXT NOT NULL,          -- e.g., 'order_submit', 'state_update', 'risk_check'
    payload JSONB NOT NULL,                 -- Original operation data
    error_message TEXT NOT NULL,            -- Error that caused the failure
    error_code TEXT,                        -- Optional error code for categorization
    retry_count INT DEFAULT 0,              -- Number of retry attempts
    max_retries INT DEFAULT 3,              -- Maximum retry attempts before permanent failure
    status TEXT DEFAULT 'pending',          -- 'pending', 'retrying', 'failed', 'resolved'
    created_at TIMESTAMPTZ DEFAULT NOW(),
    last_retry_at TIMESTAMPTZ,              -- When last retry was attempted
    resolved_at TIMESTAMPTZ,                -- When the operation was successfully resolved
    resolved_by TEXT                        -- How it was resolved: 'retry', 'manual', 'expired'
);

-- Component Heartbeats for watchdog monitoring
-- Tracks health status of each system component
CREATE TABLE IF NOT EXISTS component_heartbeats (
    component_name TEXT PRIMARY KEY,        -- e.g., 'strategy_engine', 'quote_feed', 'order_executor'
    last_heartbeat TIMESTAMPTZ DEFAULT NOW(),
    status TEXT DEFAULT 'running',          -- 'running', 'degraded', 'stopped', 'failed'
    metadata JSONB,                         -- Component-specific status data
    restart_count INT DEFAULT 0,            -- Number of times component was restarted
    last_restart TIMESTAMPTZ,               -- When component was last restarted
    failure_reason TEXT,                    -- Last failure reason if applicable
    started_at TIMESTAMPTZ DEFAULT NOW()    -- When component was started
);

-- System Events for audit trail and debugging
-- Records significant system events for analysis
CREATE TABLE IF NOT EXISTS system_events (
    id BIGSERIAL PRIMARY KEY,
    event_type TEXT NOT NULL,               -- e.g., 'circuit_breaker_open', 'component_restart', 'error'
    component TEXT NOT NULL,                -- Which component generated the event
    severity TEXT NOT NULL,                 -- 'info', 'warning', 'error', 'critical'
    message TEXT NOT NULL,                  -- Human-readable event description
    metadata JSONB,                         -- Additional event data
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Strategy State Snapshots for crash recovery
-- Enables recovery to last known good state after crash
CREATE TABLE IF NOT EXISTS state_snapshots (
    id BIGSERIAL PRIMARY KEY,
    snapshot_type TEXT NOT NULL,            -- e.g., 'strategy_engine', 'risk_manager', 'positions'
    component TEXT NOT NULL,                -- Which component's state
    state_data JSONB NOT NULL,              -- Serialized state
    version INT NOT NULL,                   -- State version for ordering
    created_at TIMESTAMPTZ DEFAULT NOW(),
    is_valid BOOLEAN DEFAULT TRUE           -- Whether snapshot is usable for recovery
);

-- Create indexes for efficient querying
CREATE INDEX IF NOT EXISTS idx_dlq_status ON dead_letter_queue(status);
CREATE INDEX IF NOT EXISTS idx_dlq_operation_type ON dead_letter_queue(operation_type, status);
CREATE INDEX IF NOT EXISTS idx_dlq_created ON dead_letter_queue(created_at);

CREATE INDEX IF NOT EXISTS idx_heartbeats_status ON component_heartbeats(status);
CREATE INDEX IF NOT EXISTS idx_heartbeats_last ON component_heartbeats(last_heartbeat);

CREATE INDEX IF NOT EXISTS idx_events_severity ON system_events(severity);
CREATE INDEX IF NOT EXISTS idx_events_created ON system_events(created_at);
CREATE INDEX IF NOT EXISTS idx_events_component ON system_events(component, created_at);
CREATE INDEX IF NOT EXISTS idx_events_type ON system_events(event_type, created_at);

CREATE INDEX IF NOT EXISTS idx_snapshots_type ON state_snapshots(snapshot_type, component);
CREATE INDEX IF NOT EXISTS idx_snapshots_created ON state_snapshots(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_snapshots_valid ON state_snapshots(is_valid, created_at DESC);

-- Add trigger to auto-update heartbeat timestamp
CREATE OR REPLACE FUNCTION update_heartbeat_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.last_heartbeat = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Only create trigger if it doesn't exist
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = 'heartbeat_timestamp_trigger') THEN
        CREATE TRIGGER heartbeat_timestamp_trigger
            BEFORE UPDATE ON component_heartbeats
            FOR EACH ROW
            EXECUTE FUNCTION update_heartbeat_timestamp();
    END IF;
END
$$;

-- Add comments for documentation
COMMENT ON TABLE dead_letter_queue IS 'Stores failed operations for retry with exponential backoff';
COMMENT ON TABLE component_heartbeats IS 'Tracks health status of each system component for watchdog';
COMMENT ON TABLE system_events IS 'Audit trail of significant system events';
COMMENT ON TABLE state_snapshots IS 'State snapshots for crash recovery';

COMMENT ON COLUMN dead_letter_queue.operation_type IS 'Type of operation: order_submit, state_update, risk_check, etc.';
COMMENT ON COLUMN dead_letter_queue.status IS 'pending=waiting, retrying=in progress, failed=permanent, resolved=success';
COMMENT ON COLUMN component_heartbeats.status IS 'running=healthy, degraded=impaired, stopped=graceful, failed=crash';
COMMENT ON COLUMN system_events.severity IS 'info=normal, warning=attention, error=problem, critical=urgent';
