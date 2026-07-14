-- Migration 045: Prevent candidate replay artifacts from spoofing evidence stage.

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_candidate_replay_tapes_basis_evidence_stage'
    ) THEN
        ALTER TABLE candidate_replay_tapes
            ADD CONSTRAINT chk_candidate_replay_tapes_basis_evidence_stage
            CHECK (
                (
                    basis = 'runtime_market_update_replay'
                    AND evidence_stage = 'executable_replay'
                )
                OR (
                    basis = 'factor_walk_forward_top_bucket_aggregate'
                    AND evidence_stage = 'diagnostic'
                )
            ) NOT VALID;
    END IF;
END $$;
