-- Preserve original approval bytes and hashes. Revocation is separate evidence;
-- legacy rows already carrying revocation fields remain historical snapshots.
CREATE TABLE IF NOT EXISTS approval_revocations (
    approval_id VARCHAR PRIMARY KEY REFERENCES approvals(approval_id),
    revocation_id VARCHAR NOT NULL UNIQUE,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    auth_tag VARCHAR NOT NULL
);

INSERT OR IGNORE INTO schema_migrations VALUES (
    '004_approval_revocations',
    CAST(current_timestamp AS VARCHAR)
);
