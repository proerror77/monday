-- A study serializes cumulative budget across a finite set of Campaign
-- families.  Member rows are immutable and a family can belong to at most one
-- study, so registering a new root cannot reset a shared budget.
CREATE TABLE IF NOT EXISTS campaign_study_heads (
    study_id VARCHAR PRIMARY KEY,
    sequence BIGINT NOT NULL,
    last_receipt_sha256 VARCHAR NOT NULL,
    auth_tag VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS campaign_study_receipts (
    study_id VARCHAR NOT NULL REFERENCES campaign_study_heads(study_id),
    sequence BIGINT NOT NULL,
    semantic_id VARCHAR NOT NULL UNIQUE,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    auth_tag VARCHAR NOT NULL,
    PRIMARY KEY (study_id, sequence)
);

CREATE TABLE IF NOT EXISTS campaign_study_members (
    study_id VARCHAR NOT NULL REFERENCES campaign_study_heads(study_id),
    family_id VARCHAR PRIMARY KEY,
    root_grant_sha256 VARCHAR NOT NULL UNIQUE,
    binding_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    UNIQUE (study_id, family_id),
    UNIQUE (study_id, root_grant_sha256)
);

CREATE TABLE IF NOT EXISTS campaign_study_receipt_publications (
    study_id VARCHAR NOT NULL,
    sequence BIGINT NOT NULL,
    object_sha256 VARCHAR NOT NULL,
    auth_tag VARCHAR NOT NULL,
    PRIMARY KEY (study_id, sequence)
);

INSERT OR IGNORE INTO schema_migrations VALUES (
    '006_campaign_study_ledger',
    CAST(current_timestamp AS VARCHAR)
);
