-- Event Registry: shared event pool for cross-strategy discovery and monitoring
-- Funnel: DISCOVER → RESEARCH → MONITOR → TRADE

CREATE TABLE IF NOT EXISTS event_registry (
    id              SERIAL PRIMARY KEY,
    event_id        TEXT,                                    -- Polymarket event ID (nullable, pending resolution)
    title           TEXT NOT NULL,
    slug            TEXT,
    source          TEXT NOT NULL DEFAULT 'polymarket',      -- polymarket/openclaw/manual/espn
    domain          TEXT NOT NULL DEFAULT 'politics',        -- sports/crypto/politics
    strategy_hint   TEXT,                                    -- event_edge/nba_comeback/multi_outcome/NULL
    status          TEXT NOT NULL DEFAULT 'discovered',      -- discovered/researched/monitoring/paused/settled/expired
    confidence      DOUBLE PRECISION,
    settlement_rule TEXT,
    end_time        TIMESTAMPTZ,
    market_slug     TEXT,
    condition_id    TEXT,
    token_ids       JSONB,
    outcome_prices  JSONB,
    metadata        JSONB NOT NULL DEFAULT '{}',
    last_scanned_at TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_event_registry_event_id
    ON event_registry(event_id) WHERE event_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_event_registry_title_source
    ON event_registry(title, source);
CREATE INDEX IF NOT EXISTS idx_event_registry_status
    ON event_registry(status);
CREATE INDEX IF NOT EXISTS idx_event_registry_status_strategy
    ON event_registry(status, strategy_hint);
CREATE INDEX IF NOT EXISTS idx_event_registry_end_time
    ON event_registry(end_time) WHERE end_time IS NOT NULL;
