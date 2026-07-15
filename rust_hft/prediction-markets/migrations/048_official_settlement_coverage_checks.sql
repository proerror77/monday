-- Migration 048: Make official settlement coverage repair evidence queryable by Research OS.

CREATE TABLE IF NOT EXISTS official_settlement_coverage_checks (
    settlement_coverage_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    source_workflow TEXT NOT NULL,
    workflow_run_id TEXT,
    workflow_run_url TEXT,
    artifact_name TEXT,
    artifact_sha256 TEXT NOT NULL,
    artifact_json JSONB NOT NULL,
    schema_version TEXT NOT NULL DEFAULT 'official_settlement_repair.v1',
    surface TEXT NOT NULL DEFAULT 'pm_token_settlements',
    data_snapshot_id TEXT REFERENCES research_dataset_snapshots(data_snapshot_id),
    window_start_ts TIMESTAMPTZ NOT NULL,
    window_end_ts TIMESTAMPTZ NOT NULL,
    symbols_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    candidate_market_count INTEGER NOT NULL,
    settlement_token_count INTEGER NOT NULL,
    settled_count INTEGER NOT NULL DEFAULT 0,
    unchanged_count INTEGER NOT NULL DEFAULT 0,
    skipped_count INTEGER NOT NULL DEFAULT 0,
    error_count INTEGER NOT NULL DEFAULT 0,
    valid BOOLEAN NOT NULL DEFAULT false,
    blockers_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_official_settlement_coverage_artifact_sha256 UNIQUE (artifact_sha256),
    CONSTRAINT chk_official_settlement_coverage_schema
        CHECK (schema_version = 'official_settlement_repair.v1'),
    CONSTRAINT chk_official_settlement_coverage_window
        CHECK (window_start_ts < window_end_ts),
    CONSTRAINT chk_official_settlement_coverage_symbols_array
        CHECK (jsonb_typeof(symbols_json) = 'array'),
    CONSTRAINT chk_official_settlement_coverage_counts
        CHECK (
            candidate_market_count >= 0
            AND settlement_token_count >= 0
            AND settled_count >= 0
            AND unchanged_count >= 0
            AND skipped_count >= 0
            AND error_count >= 0
        ),
    CONSTRAINT chk_official_settlement_coverage_blockers_array
        CHECK (jsonb_typeof(blockers_json) = 'array')
);

CREATE INDEX IF NOT EXISTS idx_official_settlement_coverage_surface_window
    ON official_settlement_coverage_checks(surface, window_start_ts, window_end_ts);
CREATE INDEX IF NOT EXISTS idx_official_settlement_coverage_valid
    ON official_settlement_coverage_checks(valid, surface, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_official_settlement_coverage_run
    ON official_settlement_coverage_checks(run_id);
