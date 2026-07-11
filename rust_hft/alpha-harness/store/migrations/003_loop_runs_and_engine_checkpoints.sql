ALTER TABLE checkpoints ADD COLUMN IF NOT EXISTS engine_kind VARCHAR;
ALTER TABLE checkpoints ADD COLUMN IF NOT EXISTS engine_version BIGINT;
ALTER TABLE checkpoints ADD COLUMN IF NOT EXISTS checkpoint_json VARCHAR;
ALTER TABLE checkpoints ADD COLUMN IF NOT EXISTS content_hash VARCHAR;
ALTER TABLE checkpoints ADD COLUMN IF NOT EXISTS auth_tag VARCHAR;

CREATE TABLE IF NOT EXISTS loop_runs (
    loop_run_id VARCHAR PRIMARY KEY,
    root_mission_id VARCHAR NOT NULL REFERENCES missions(mission_id),
    status VARCHAR NOT NULL,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    auth_tag VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL,
    updated_at VARCHAR NOT NULL
);

ALTER TABLE loop_runs ADD COLUMN IF NOT EXISTS auth_tag VARCHAR;

INSERT OR IGNORE INTO schema_migrations VALUES (
    '003_loop_runs_and_engine_checkpoints',
    CAST(current_timestamp AS VARCHAR)
);
