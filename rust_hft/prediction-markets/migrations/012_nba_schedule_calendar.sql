-- Migration: 012_nba_schedule_calendar
-- Purpose: Store near-term NBA schedule calendar to gate sports agent startup
-- and avoid trading on non-game artifacts.

CREATE TABLE IF NOT EXISTS nba_schedule_calendar (
    espn_game_id TEXT PRIMARY KEY,
    season TEXT NOT NULL,
    game_date DATE NOT NULL,
    home_team TEXT NOT NULL,
    away_team TEXT NOT NULL,
    home_abbrev TEXT NOT NULL,
    away_abbrev TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'scheduled',
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_nba_schedule_calendar_game_date
    ON nba_schedule_calendar(game_date);

CREATE INDEX IF NOT EXISTS idx_nba_schedule_calendar_season
    ON nba_schedule_calendar(season, game_date);
