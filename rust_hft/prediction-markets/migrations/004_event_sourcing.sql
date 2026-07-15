-- Migration: 004_event_sourcing
-- Description: Event sourcing tables for audit trail and state replay
-- Part of: Phase 4 - Persistence Layer

-- Strategy events for event sourcing
CREATE TABLE IF NOT EXISTS strategy_events (
    id BIGSERIAL PRIMARY KEY,
    aggregate_id TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    event_type TEXT NOT NULL,
    event_version INT NOT NULL,
    payload JSONB NOT NULL,
    metadata JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Index for aggregate lookups (most common query pattern)
CREATE INDEX IF NOT EXISTS idx_strategy_events_aggregate
    ON strategy_events(aggregate_id, aggregate_type);

-- Index for event type queries
CREATE INDEX IF NOT EXISTS idx_strategy_events_type
    ON strategy_events(event_type);

-- Index for temporal queries
CREATE INDEX IF NOT EXISTS idx_strategy_events_created
    ON strategy_events(created_at);

-- Index for correlation ID lookups (in metadata)
CREATE INDEX IF NOT EXISTS idx_strategy_events_correlation
    ON strategy_events((metadata->>'correlation_id'))
    WHERE metadata->>'correlation_id' IS NOT NULL;

-- Composite index for version ordering within aggregate
CREATE INDEX IF NOT EXISTS idx_strategy_events_aggregate_version
    ON strategy_events(aggregate_id, aggregate_type, event_version);

-- Projections table for materialized read models
CREATE TABLE IF NOT EXISTS event_projections (
    id BIGSERIAL PRIMARY KEY,
    projection_name TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    last_event_id BIGINT NOT NULL DEFAULT 0,
    state JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- Unique constraint for projection per aggregate type
CREATE UNIQUE INDEX IF NOT EXISTS idx_projections_unique
    ON event_projections(projection_name, aggregate_type);

-- Event snapshots for optimized replay
CREATE TABLE IF NOT EXISTS event_snapshots (
    id BIGSERIAL PRIMARY KEY,
    aggregate_id TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    snapshot_version INT NOT NULL,
    last_event_id BIGINT NOT NULL,
    state JSONB NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Index for snapshot lookups
CREATE INDEX IF NOT EXISTS idx_snapshots_aggregate
    ON event_snapshots(aggregate_id, aggregate_type, snapshot_version DESC);

-- Function to get latest snapshot for an aggregate
CREATE OR REPLACE FUNCTION get_latest_snapshot(
    p_aggregate_id TEXT,
    p_aggregate_type TEXT
) RETURNS TABLE (
    snapshot_id BIGINT,
    snapshot_version INT,
    last_event_id BIGINT,
    state JSONB
) AS $$
BEGIN
    RETURN QUERY
    SELECT es.id, es.snapshot_version, es.last_event_id, es.state
    FROM event_snapshots es
    WHERE es.aggregate_id = p_aggregate_id
      AND es.aggregate_type = p_aggregate_type
    ORDER BY es.snapshot_version DESC
    LIMIT 1;
END;
$$ LANGUAGE plpgsql;

-- Function to get events since snapshot
CREATE OR REPLACE FUNCTION get_events_since_snapshot(
    p_aggregate_id TEXT,
    p_aggregate_type TEXT,
    p_since_event_id BIGINT DEFAULT 0
) RETURNS TABLE (
    event_id BIGINT,
    event_type TEXT,
    event_version INT,
    payload JSONB,
    metadata JSONB,
    created_at TIMESTAMPTZ
) AS $$
BEGIN
    RETURN QUERY
    SELECT se.id, se.event_type, se.event_version, se.payload, se.metadata, se.created_at
    FROM strategy_events se
    WHERE se.aggregate_id = p_aggregate_id
      AND se.aggregate_type = p_aggregate_type
      AND se.id > p_since_event_id
    ORDER BY se.event_version ASC, se.created_at ASC;
END;
$$ LANGUAGE plpgsql;

-- Function to rebuild aggregate from events (for debugging)
CREATE OR REPLACE FUNCTION get_aggregate_history(
    p_aggregate_id TEXT,
    p_aggregate_type TEXT
) RETURNS TABLE (
    event_id BIGINT,
    event_type TEXT,
    event_version INT,
    payload JSONB,
    created_at TIMESTAMPTZ,
    time_since_previous INTERVAL
) AS $$
BEGIN
    RETURN QUERY
    SELECT
        se.id,
        se.event_type,
        se.event_version,
        se.payload,
        se.created_at,
        se.created_at - LAG(se.created_at) OVER (ORDER BY se.event_version) as time_since_previous
    FROM strategy_events se
    WHERE se.aggregate_id = p_aggregate_id
      AND se.aggregate_type = p_aggregate_type
    ORDER BY se.event_version ASC;
END;
$$ LANGUAGE plpgsql;

-- View for event statistics
CREATE OR REPLACE VIEW v_event_statistics AS
SELECT
    aggregate_type,
    event_type,
    COUNT(*) as event_count,
    MIN(created_at) as first_event,
    MAX(created_at) as last_event,
    COUNT(DISTINCT aggregate_id) as unique_aggregates
FROM strategy_events
GROUP BY aggregate_type, event_type
ORDER BY aggregate_type, event_count DESC;

-- Function to cleanup old events (for archives/compliance)
CREATE OR REPLACE FUNCTION archive_old_events(
    days_to_keep INT DEFAULT 90,
    archive_table TEXT DEFAULT 'strategy_events_archive'
) RETURNS INT AS $$
DECLARE
    archived_count INT;
BEGIN
    -- Create archive table if not exists (same structure)
    EXECUTE format('
        CREATE TABLE IF NOT EXISTS %I (LIKE strategy_events INCLUDING ALL)
    ', archive_table);

    -- Move old events to archive
    EXECUTE format('
        WITH moved AS (
            DELETE FROM strategy_events
            WHERE created_at < NOW() - ($1 || '' days'')::INTERVAL
            RETURNING *
        )
        INSERT INTO %I SELECT * FROM moved
    ', archive_table) USING days_to_keep;

    GET DIAGNOSTICS archived_count = ROW_COUNT;
    RETURN archived_count;
END;
$$ LANGUAGE plpgsql;

-- Comments
COMMENT ON TABLE strategy_events IS 'Event sourcing store for domain events';
COMMENT ON TABLE event_projections IS 'Materialized read models from events';
COMMENT ON TABLE event_snapshots IS 'Periodic snapshots for optimized event replay';
COMMENT ON FUNCTION get_latest_snapshot IS 'Get most recent snapshot for an aggregate';
COMMENT ON FUNCTION get_events_since_snapshot IS 'Get events since a snapshot for replay';
COMMENT ON FUNCTION get_aggregate_history IS 'Get full event history for debugging';
COMMENT ON FUNCTION archive_old_events IS 'Archive events older than specified days';
