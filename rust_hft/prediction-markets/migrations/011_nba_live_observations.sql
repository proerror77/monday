-- Migration: 011_nba_live_observations
-- Purpose: Persist live sports observations that combine ESPN score state
-- and Polymarket moneyline/CLOB snapshots for audit and analytics.

CREATE TABLE IF NOT EXISTS nba_live_observations (
    id BIGSERIAL PRIMARY KEY,
    agent_id TEXT NOT NULL,
    espn_game_id TEXT NOT NULL,
    home_team TEXT NOT NULL,
    away_team TEXT NOT NULL,
    home_abbrev TEXT NOT NULL,
    away_abbrev TEXT NOT NULL,
    home_score INTEGER NOT NULL,
    away_score INTEGER NOT NULL,
    quarter INTEGER NOT NULL,
    clock TEXT NOT NULL,
    time_remaining_mins DOUBLE PRECISION NOT NULL,
    game_status TEXT NOT NULL,
    trailing_team TEXT,
    trailing_abbrev TEXT,
    deficit INTEGER,
    comeback_rate DOUBLE PRECISION,
    adjusted_win_prob DOUBLE PRECISION,
    pm_event_id TEXT,
    pm_event_title TEXT,
    pm_event_slug TEXT,
    pm_live_status TEXT,
    pm_yes_token_id TEXT,
    pm_no_token_id TEXT,
    pm_yes_mid NUMERIC(10,6),
    pm_no_mid NUMERIC(10,6),
    pm_yes_best_bid NUMERIC(10,6),
    pm_yes_best_ask NUMERIC(10,6),
    pm_no_best_bid NUMERIC(10,6),
    pm_no_best_ask NUMERIC(10,6),
    pm_trailing_token_id TEXT,
    pm_trailing_price NUMERIC(10,6),
    pm_trailing_price_source TEXT,
    edge DOUBLE PRECISION,
    is_trade_candidate BOOLEAN NOT NULL DEFAULT FALSE,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_nba_live_observations_game_time
    ON nba_live_observations(espn_game_id, recorded_at DESC);

CREATE INDEX IF NOT EXISTS idx_nba_live_observations_time
    ON nba_live_observations(recorded_at DESC);

CREATE INDEX IF NOT EXISTS idx_nba_live_observations_tokens
    ON nba_live_observations(pm_trailing_token_id, recorded_at DESC);
