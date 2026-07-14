-- Migration 047: Make full-depth execution-surface proofs queryable by Research OS.

CREATE TABLE IF NOT EXISTS full_depth_execution_surfaces (
    full_depth_execution_surface_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    source_workflow TEXT NOT NULL,
    workflow_run_id TEXT,
    workflow_run_url TEXT,
    artifact_name TEXT,
    artifact_sha256 TEXT NOT NULL,
    artifact_json JSONB NOT NULL,
    schema_version TEXT NOT NULL DEFAULT 'full_depth_execution_surface.v1',
    surface TEXT NOT NULL,
    source TEXT NOT NULL,
    data_snapshot_id TEXT REFERENCES research_dataset_snapshots(data_snapshot_id),
    window_start_ts TIMESTAMPTZ NOT NULL,
    window_end_ts TIMESTAMPTZ NOT NULL,
    checked_hours INTEGER NOT NULL,
    existing_hours INTEGER NOT NULL,
    exported_hours INTEGER NOT NULL DEFAULT 0,
    row_count BIGINT NOT NULL,
    full_fidelity BOOLEAN NOT NULL DEFAULT false,
    incomplete BOOLEAN NOT NULL DEFAULT true,
    valid BOOLEAN NOT NULL DEFAULT false,
    blockers_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_full_depth_execution_surfaces_artifact_sha256 UNIQUE (artifact_sha256),
    CONSTRAINT chk_full_depth_execution_surfaces_schema
        CHECK (schema_version = 'full_depth_execution_surface.v1'),
    CONSTRAINT chk_full_depth_execution_surfaces_window
        CHECK (window_start_ts < window_end_ts),
    CONSTRAINT chk_full_depth_execution_surfaces_hours
        CHECK (checked_hours >= 0 AND existing_hours >= 0 AND exported_hours >= 0),
    CONSTRAINT chk_full_depth_execution_surfaces_rows
        CHECK (row_count >= 0),
    CONSTRAINT chk_full_depth_execution_surfaces_blockers_array
        CHECK (jsonb_typeof(blockers_json) = 'array')
);

CREATE INDEX IF NOT EXISTS idx_full_depth_execution_surfaces_surface_window
    ON full_depth_execution_surfaces(surface, window_start_ts, window_end_ts);
CREATE INDEX IF NOT EXISTS idx_full_depth_execution_surfaces_valid
    ON full_depth_execution_surfaces(valid, surface, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_full_depth_execution_surfaces_run
    ON full_depth_execution_surfaces(run_id);

ALTER TABLE experiment_trace
    ADD COLUMN IF NOT EXISTS full_depth_execution_surface_id TEXT;

CREATE INDEX IF NOT EXISTS idx_experiment_trace_full_depth_execution_surface
    ON experiment_trace(full_depth_execution_surface_id, created_at DESC)
    WHERE full_depth_execution_surface_id IS NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'fk_experiment_trace_full_depth_execution_surface'
          AND conrelid = 'experiment_trace'::regclass
    ) THEN
        ALTER TABLE experiment_trace
            ADD CONSTRAINT fk_experiment_trace_full_depth_execution_surface
            FOREIGN KEY (full_depth_execution_surface_id)
            REFERENCES full_depth_execution_surfaces(full_depth_execution_surface_id)
            NOT VALID;
    END IF;
END $$;
