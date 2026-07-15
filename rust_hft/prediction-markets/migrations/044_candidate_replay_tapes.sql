-- Migration 044: Make candidate replay artifacts first-class Research OS trace rows.

CREATE TABLE IF NOT EXISTS candidate_replay_tapes (
    candidate_replay_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    source_workflow TEXT NOT NULL,
    workflow_run_id TEXT,
    workflow_run_url TEXT,
    artifact_name TEXT,
    artifact_sha256 TEXT NOT NULL,
    artifact_json JSONB NOT NULL,
    basis TEXT NOT NULL,
    evidence_stage TEXT NOT NULL DEFAULT 'executable_replay',
    deployment_id TEXT,
    strategy_profile TEXT NOT NULL,
    runtime_score TEXT NOT NULL,
    data_snapshot_id TEXT REFERENCES research_dataset_snapshots(data_snapshot_id),
    dsl_hash TEXT,
    target TEXT,
    horizon TEXT,
    recording_path TEXT,
    recording_sha256 TEXT,
    config_path TEXT,
    config_sha256 TEXT,
    runner_source TEXT,
    runner_git_sha TEXT,
    replay_window_start_ts TIMESTAMPTZ,
    replay_window_end_ts TIMESTAMPTZ,
    decision_contract_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    acceptance_criteria_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    metrics_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    blocking_risk_flags_json JSONB NOT NULL DEFAULT '[]'::jsonb,
    promotion_ready BOOLEAN NOT NULL DEFAULT false,
    promotion_decision TEXT NOT NULL DEFAULT 'blocked',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_candidate_replay_tapes_artifact_sha256 UNIQUE (artifact_sha256),
    CONSTRAINT chk_candidate_replay_tapes_basis
        CHECK (basis IN ('runtime_market_update_replay', 'factor_walk_forward_top_bucket_aggregate')),
    CONSTRAINT chk_candidate_replay_tapes_evidence_stage
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
        ),
    CONSTRAINT chk_candidate_replay_tapes_basis_evidence_stage
        CHECK (
            (
                basis = 'runtime_market_update_replay'
                AND evidence_stage = 'executable_replay'
            )
            OR (
                basis = 'factor_walk_forward_top_bucket_aggregate'
                AND evidence_stage = 'diagnostic'
            )
        ),
    CONSTRAINT chk_candidate_replay_tapes_blockers_array
        CHECK (jsonb_typeof(blocking_risk_flags_json) = 'array'),
    CONSTRAINT chk_candidate_replay_tapes_promotion_decision
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
);

CREATE INDEX IF NOT EXISTS idx_candidate_replay_tapes_runtime_score
    ON candidate_replay_tapes(runtime_score, strategy_profile, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_candidate_replay_tapes_run
    ON candidate_replay_tapes(run_id);
CREATE INDEX IF NOT EXISTS idx_candidate_replay_tapes_snapshot_factor
    ON candidate_replay_tapes(data_snapshot_id, dsl_hash);
CREATE INDEX IF NOT EXISTS idx_candidate_replay_tapes_promotion_ready
    ON candidate_replay_tapes(promotion_ready, created_at DESC);

ALTER TABLE experiment_trace
    ADD COLUMN IF NOT EXISTS candidate_replay_id TEXT;

CREATE INDEX IF NOT EXISTS idx_experiment_trace_candidate_replay
    ON experiment_trace(candidate_replay_id, created_at DESC)
    WHERE candidate_replay_id IS NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'fk_factor_evaluations_candidate_replay'
          AND conrelid = 'factor_evaluations'::regclass
    ) THEN
        ALTER TABLE factor_evaluations
            ADD CONSTRAINT fk_factor_evaluations_candidate_replay
            FOREIGN KEY (candidate_replay_id)
            REFERENCES candidate_replay_tapes(candidate_replay_id)
            NOT VALID;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'fk_experiment_trace_candidate_replay'
          AND conrelid = 'experiment_trace'::regclass
    ) THEN
        ALTER TABLE experiment_trace
            ADD CONSTRAINT fk_experiment_trace_candidate_replay
            FOREIGN KEY (candidate_replay_id)
            REFERENCES candidate_replay_tapes(candidate_replay_id)
            NOT VALID;
    END IF;
END $$;
