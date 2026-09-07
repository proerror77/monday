-- Mutable serialization guard; original approval evidence remains insert-only.
CREATE TABLE IF NOT EXISTS approval_control_heads (
    approval_id VARCHAR PRIMARY KEY REFERENCES approvals(approval_id),
    revision BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS campaign_family_heads (
    family_id VARCHAR PRIMARY KEY,
    sequence BIGINT NOT NULL,
    last_receipt_sha256 VARCHAR NOT NULL,
    auth_tag VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS campaign_ledger_receipts (
    family_id VARCHAR NOT NULL REFERENCES campaign_family_heads(family_id),
    sequence BIGINT NOT NULL,
    semantic_id VARCHAR NOT NULL UNIQUE,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    auth_tag VARCHAR NOT NULL,
    PRIMARY KEY (family_id, sequence)
);

CREATE TABLE IF NOT EXISTS campaign_receipt_publications (
    family_id VARCHAR NOT NULL,
    sequence BIGINT NOT NULL,
    object_sha256 VARCHAR NOT NULL,
    auth_tag VARCHAR NOT NULL,
    PRIMARY KEY (family_id, sequence)
);

INSERT OR IGNORE INTO schema_migrations VALUES (
    '005_campaign_family_ledger',
    CAST(current_timestamp AS VARCHAR)
);
