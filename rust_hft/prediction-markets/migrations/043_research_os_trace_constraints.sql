-- Migration 043: Harden Research OS dataset and promotion lineage contracts.

ALTER TABLE factor_registry
    ADD COLUMN IF NOT EXISTS runtime_contract JSONB NOT NULL DEFAULT '{}'::jsonb;

DROP INDEX IF EXISTS idx_factor_registry_dsl_hash;

-- Historical preview runs could insert competing rows before the durable
-- identity became (dsl_hash, target, horizon). Keep the most promoted row, then
-- the newest row, and repoint evaluations before enforcing the final key.
DROP TABLE IF EXISTS tmp_factor_registry_dedup;

CREATE TEMP TABLE tmp_factor_registry_dedup AS
WITH ranked AS (
    SELECT
        factor_id,
        first_value(factor_id) OVER (
            PARTITION BY dsl_hash, target, horizon
            ORDER BY
                CASE status
                    WHEN 'production' THEN 0
                    WHEN 'approved' THEN 1
                    WHEN 'dry_run' THEN 2
                    WHEN 'candidate' THEN 3
                    WHEN 'evaluated' THEN 4
                    WHEN 'compiled' THEN 5
                    WHEN 'draft' THEN 6
                    WHEN 'deprecated' THEN 7
                    ELSE 8
                END,
                created_at DESC,
                factor_id
        ) AS survivor_factor_id,
        row_number() OVER (
            PARTITION BY dsl_hash, target, horizon
            ORDER BY
                CASE status
                    WHEN 'production' THEN 0
                    WHEN 'approved' THEN 1
                    WHEN 'dry_run' THEN 2
                    WHEN 'candidate' THEN 3
                    WHEN 'evaluated' THEN 4
                    WHEN 'compiled' THEN 5
                    WHEN 'draft' THEN 6
                    WHEN 'deprecated' THEN 7
                    ELSE 8
                END,
                created_at DESC,
                factor_id
        ) AS row_num
    FROM factor_registry
)
SELECT
    factor_id AS duplicate_factor_id,
    survivor_factor_id
FROM ranked
WHERE row_num > 1;

UPDATE factor_evaluations
SET factor_id = tmp_factor_registry_dedup.survivor_factor_id
FROM tmp_factor_registry_dedup
WHERE factor_evaluations.factor_id = tmp_factor_registry_dedup.duplicate_factor_id;

DELETE FROM factor_registry
USING tmp_factor_registry_dedup
WHERE factor_registry.factor_id = tmp_factor_registry_dedup.duplicate_factor_id;

DROP TABLE tmp_factor_registry_dedup;

CREATE UNIQUE INDEX IF NOT EXISTS idx_factor_registry_dsl_target_horizon
    ON factor_registry(dsl_hash, target, horizon);

CREATE TABLE IF NOT EXISTS research_dataset_snapshots (
    data_snapshot_id TEXT PRIMARY KEY,
    snapshot_hash TEXT,
    schema_version TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    dataset_start_ts TIMESTAMPTZ NOT NULL,
    dataset_end_ts TIMESTAMPTZ NOT NULL,
    symbols TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    row_counts_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_surfaces_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    input_artifacts_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    sampling_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    manifest_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_research_dataset_snapshots_window
        CHECK (dataset_start_ts < dataset_end_ts),
    CONSTRAINT chk_research_dataset_snapshots_source_surfaces_array
        CHECK (jsonb_typeof(source_surfaces_json) = 'array'),
    CONSTRAINT chk_research_dataset_snapshots_input_artifacts_array
        CHECK (jsonb_typeof(input_artifacts_json) = 'array')
);

CREATE INDEX IF NOT EXISTS idx_research_dataset_snapshots_window
    ON research_dataset_snapshots(dataset_start_ts, dataset_end_ts);
CREATE INDEX IF NOT EXISTS idx_research_dataset_snapshots_hash
    ON research_dataset_snapshots(snapshot_hash)
    WHERE snapshot_hash IS NOT NULL;

ALTER TABLE factor_evaluations
    ADD COLUMN IF NOT EXISTS dataset_start_ts TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS dataset_end_ts TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS evidence_stage TEXT NOT NULL DEFAULT 'factor_attribution',
    ADD COLUMN IF NOT EXISTS evaluation_kind TEXT NOT NULL DEFAULT 'alpha_search_preview',
    ADD COLUMN IF NOT EXISTS candidate_replay_id TEXT,
    ADD COLUMN IF NOT EXISTS runtime_contract JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS promotion_decision TEXT NOT NULL DEFAULT 'not_evaluated',
    ADD COLUMN IF NOT EXISTS promotion_status TEXT NOT NULL DEFAULT 'blocked',
    ADD COLUMN IF NOT EXISTS blockers_json JSONB NOT NULL DEFAULT '[]'::jsonb;

CREATE INDEX IF NOT EXISTS idx_factor_evaluations_snapshot
    ON factor_evaluations(data_snapshot_id);
CREATE INDEX IF NOT EXISTS idx_factor_evaluations_promotion
    ON factor_evaluations(promotion_status, promotion_decision, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_factor_evaluations_stage
    ON factor_evaluations(evidence_stage, evaluation_kind, created_at DESC);

ALTER TABLE experiment_trace
    ADD COLUMN IF NOT EXISTS data_snapshot_id TEXT,
    ADD COLUMN IF NOT EXISTS dsl_hash TEXT,
    ADD COLUMN IF NOT EXISTS artifact_kind TEXT NOT NULL DEFAULT 'artifact',
    ADD COLUMN IF NOT EXISTS evidence_stage TEXT NOT NULL DEFAULT 'diagnostic',
    ADD COLUMN IF NOT EXISTS promotion_decision TEXT;

CREATE INDEX IF NOT EXISTS idx_experiment_trace_lineage
    ON experiment_trace(data_snapshot_id, dsl_hash, evidence_stage, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_experiment_trace_artifact_kind
    ON experiment_trace(artifact_kind, created_at DESC);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'fk_factor_evaluations_data_snapshot'
          AND conrelid = 'factor_evaluations'::regclass
    ) THEN
        ALTER TABLE factor_evaluations
            ADD CONSTRAINT fk_factor_evaluations_data_snapshot
            FOREIGN KEY (data_snapshot_id)
            REFERENCES research_dataset_snapshots(data_snapshot_id)
            NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_factor_evaluations_dataset_window'
          AND conrelid = 'factor_evaluations'::regclass
    ) THEN
        ALTER TABLE factor_evaluations
            ADD CONSTRAINT chk_factor_evaluations_dataset_window
            CHECK (
                dataset_start_ts IS NOT NULL
                AND dataset_end_ts IS NOT NULL
                AND dataset_start_ts < dataset_end_ts
            )
            NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_factor_evaluations_evidence_stage'
          AND conrelid = 'factor_evaluations'::regclass
    ) THEN
        ALTER TABLE factor_evaluations
            ADD CONSTRAINT chk_factor_evaluations_evidence_stage
            CHECK (
                evidence_stage IN (
                    'diagnostic',
                    'factor_attribution',
                    'executable_replay',
                    'walk_forward',
                    'runtime_parity',
                    'dry_run_candidate',
                    'live_candidate'
                )
            )
            NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_factor_evaluations_evaluation_kind'
          AND conrelid = 'factor_evaluations'::regclass
    ) THEN
        ALTER TABLE factor_evaluations
            ADD CONSTRAINT chk_factor_evaluations_evaluation_kind
            CHECK (
                evaluation_kind IN (
                    'alpha_search_preview',
                    'promotion_gate',
                    'candidate_replay',
                    'backtest',
                    'runtime_parity',
                    'dry_run_observation'
                )
            )
            NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_factor_evaluations_promotion_decision'
          AND conrelid = 'factor_evaluations'::regclass
    ) THEN
        ALTER TABLE factor_evaluations
            ADD CONSTRAINT chk_factor_evaluations_promotion_decision
            CHECK (
                promotion_decision IN (
                    'not_evaluated',
                    'continue',
                    'revise',
                    'reject',
                    'blocked',
                    'do_not_promote',
                    'promote_to_runtime',
                    'dry_run_candidate',
                    'live_candidate'
                )
            )
            NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_factor_evaluations_promotion_status'
          AND conrelid = 'factor_evaluations'::regclass
    ) THEN
        ALTER TABLE factor_evaluations
            ADD CONSTRAINT chk_factor_evaluations_promotion_status
            CHECK (
                promotion_status IN (
                    'blocked',
                    'watchlist',
                    'candidate',
                    'ready',
                    'dry_run',
                    'promoted',
                    'rejected'
                )
            )
            NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_factor_evaluations_blockers_array'
          AND conrelid = 'factor_evaluations'::regclass
    ) THEN
        ALTER TABLE factor_evaluations
            ADD CONSTRAINT chk_factor_evaluations_blockers_array
            CHECK (jsonb_typeof(blockers_json) = 'array')
            NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_experiment_trace_evidence_stage'
          AND conrelid = 'experiment_trace'::regclass
    ) THEN
        ALTER TABLE experiment_trace
            ADD CONSTRAINT chk_experiment_trace_evidence_stage
            CHECK (
                evidence_stage IN (
                    'diagnostic',
                    'factor_attribution',
                    'executable_replay',
                    'walk_forward',
                    'runtime_parity',
                    'dry_run_candidate',
                    'live_candidate'
                )
            )
            NOT VALID;
    END IF;
END $$;
