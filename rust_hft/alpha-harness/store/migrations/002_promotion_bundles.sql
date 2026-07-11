CREATE TABLE IF NOT EXISTS strategy_bundles (
    bundle_id VARCHAR PRIMARY KEY,
    candidate_id VARCHAR NOT NULL REFERENCES candidate_artifacts(candidate_id),
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS promotions (
    promotion_id VARCHAR PRIMARY KEY,
    mission_id VARCHAR NOT NULL REFERENCES missions(mission_id),
    candidate_id VARCHAR NOT NULL REFERENCES candidate_artifacts(candidate_id),
    bundle_id VARCHAR NOT NULL UNIQUE REFERENCES strategy_bundles(bundle_id),
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

ALTER TABLE approvals ADD COLUMN IF NOT EXISTS signer_id VARCHAR;
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS valid_from VARCHAR;
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS expires_at VARCHAR;
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS revoked_at VARCHAR;
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS revoked_by VARCHAR;
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS revocation_reason VARCHAR;

INSERT OR IGNORE INTO schema_migrations VALUES ('002_promotion_bundles', CAST(current_timestamp AS VARCHAR));
