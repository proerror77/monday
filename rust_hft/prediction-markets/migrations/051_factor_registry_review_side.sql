-- Migration 051: Preserve the independent UP/DOWN execution-head identity.

ALTER TABLE factor_registry
    ADD COLUMN IF NOT EXISTS review_side TEXT;

ALTER TABLE factor_registry
    ADD CONSTRAINT chk_factor_registry_review_side
    CHECK (review_side IS NULL OR review_side IN ('up', 'down'))
    NOT VALID;

ALTER TABLE factor_registry
    VALIDATE CONSTRAINT chk_factor_registry_review_side;

-- Historical rows stay pooled (NULL). NULLS NOT DISTINCT preserves their
-- existing uniqueness while allowing one UP and one DOWN row beside them.
CREATE UNIQUE INDEX idx_factor_registry_dsl_target_horizon_review_side
    ON factor_registry(dsl_hash, target, horizon, review_side) NULLS NOT DISTINCT;

DROP INDEX idx_factor_registry_dsl_target_horizon;
