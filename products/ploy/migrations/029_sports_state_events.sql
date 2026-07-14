-- Migration 029: persisted sports live-state capture

CREATE TABLE IF NOT EXISTS sports_state_events (
    id BIGSERIAL PRIMARY KEY,
    game_id TEXT NOT NULL,
    league TEXT NOT NULL,
    slug TEXT NOT NULL,
    home_team TEXT NOT NULL,
    away_team TEXT NOT NULL,
    status TEXT NOT NULL,
    period TEXT,
    score TEXT,
    elapsed TEXT,
    live BOOLEAN NOT NULL,
    ended BOOLEAN NOT NULL,
    finished_at TIMESTAMPTZ,
    source TEXT NOT NULL DEFAULT 'polymarket_sports_ws',
    event_time TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    raw_message JSONB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sports_state_events_game_time
    ON sports_state_events(game_id, event_time DESC);
CREATE INDEX IF NOT EXISTS idx_sports_state_events_slug_time
    ON sports_state_events(slug, event_time DESC);
CREATE INDEX IF NOT EXISTS idx_sports_state_events_league_time
    ON sports_state_events(league, event_time DESC);
CREATE INDEX IF NOT EXISTS idx_sports_state_events_received_at
    ON sports_state_events(received_at DESC);

DO $$
BEGIN
    IF to_regclass('public.sports_state_events') IS NOT NULL THEN
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.sports_state_events TO ploy';
    END IF;
END $$;
