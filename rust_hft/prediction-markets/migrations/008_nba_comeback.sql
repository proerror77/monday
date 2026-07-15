-- NBA Comeback Trading Agent tables
-- Stores team historical comeback stats and trade logs for PnL tracking

CREATE TABLE IF NOT EXISTS nba_team_stats (
    id              SERIAL PRIMARY KEY,
    team_name       TEXT NOT NULL,           -- "Boston Celtics"
    team_abbrev     TEXT NOT NULL,           -- "BOS"
    season          TEXT NOT NULL,           -- "2025-26"

    -- Win/loss record
    wins            INT NOT NULL DEFAULT 0,
    losses          INT NOT NULL DEFAULT 0,
    win_rate        DOUBLE PRECISION NOT NULL DEFAULT 0.0,

    -- Scoring averages
    avg_points      DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    q1_avg_points   DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    q2_avg_points   DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    q3_avg_points   DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    q4_avg_points   DOUBLE PRECISION NOT NULL DEFAULT 0.0,

    -- Comeback rates (win % when trailing by N points entering Q4)
    comeback_rate_5pt   DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    comeback_rate_10pt  DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    comeback_rate_15pt  DOUBLE PRECISION NOT NULL DEFAULT 0.0,

    -- Q4 performance
    q4_net_rating   DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    q4_pace         DOUBLE PRECISION NOT NULL DEFAULT 0.0,

    -- Strength ratings
    elo_rating          DOUBLE PRECISION,
    offensive_rating    DOUBLE PRECISION,
    defensive_rating    DOUBLE PRECISION,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE (team_abbrev, season)
);

CREATE INDEX IF NOT EXISTS idx_nba_team_stats_team_name ON nba_team_stats (team_name);
CREATE INDEX IF NOT EXISTS idx_nba_team_stats_season ON nba_team_stats (season);

CREATE TABLE IF NOT EXISTS nba_comeback_trades (
    id              SERIAL PRIMARY KEY,
    game_id         TEXT NOT NULL,           -- ESPN game ID
    team_abbrev     TEXT NOT NULL,           -- trailing team we bet on
    team_name       TEXT NOT NULL,
    opponent        TEXT NOT NULL,

    -- Game state at entry
    deficit         INT NOT NULL,            -- points behind at entry
    quarter         INT NOT NULL,            -- quarter at entry (typically 3)
    clock           TEXT,                    -- game clock at entry

    -- Model inputs
    comeback_rate   DOUBLE PRECISION NOT NULL,
    adjusted_win_prob DOUBLE PRECISION NOT NULL,
    market_price    NUMERIC(10,6) NOT NULL,  -- Polymarket YES ask at entry
    edge            DOUBLE PRECISION NOT NULL,

    -- Trade details
    market_slug     TEXT NOT NULL,
    token_id        TEXT NOT NULL,
    shares          BIGINT NOT NULL,
    entry_price     NUMERIC(10,6) NOT NULL,
    exit_price      NUMERIC(10,6),
    pnl             NUMERIC(10,6),

    -- Status
    status          TEXT NOT NULL DEFAULT 'open',  -- open, closed, expired
    entry_time      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    exit_time       TIMESTAMPTZ,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_nba_comeback_trades_game_id ON nba_comeback_trades (game_id);
CREATE INDEX IF NOT EXISTS idx_nba_comeback_trades_status ON nba_comeback_trades (status);
CREATE INDEX IF NOT EXISTS idx_nba_comeback_trades_team ON nba_comeback_trades (team_abbrev);
