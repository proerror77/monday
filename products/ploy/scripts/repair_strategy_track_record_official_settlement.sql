-- Rebuild strategy-runtime track-record views so historical dry-run/live rows
-- use official Polymarket settlement for expiry payouts.
--
-- Usage from repo root on the DB host:
--   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f scripts/repair_strategy_track_record_official_settlement.sql

CREATE TABLE IF NOT EXISTS strategy_track_record_view_backups (
    backup_id BIGSERIAL PRIMARY KEY,
    backed_up_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    view_name TEXT NOT NULL,
    view_definition TEXT
);

INSERT INTO strategy_track_record_view_backups (view_name, view_definition)
SELECT view_name, pg_get_viewdef(('public.' || view_name)::regclass, true)
FROM (VALUES
    ('strategy_runtime_event_track_record'),
    ('strategy_runtime_daily_track_record')
) AS v(view_name)
WHERE to_regclass('public.' || view_name) IS NOT NULL;

\ir ../migrations/038_track_record_official_residual_settlement.sql
