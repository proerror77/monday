CREATE TABLE IF NOT EXISTS schema_migrations (
    version VARCHAR PRIMARY KEY,
    applied_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS missions (
    mission_id VARCHAR PRIMARY KEY,
    status VARCHAR NOT NULL,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL,
    updated_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS iterations (
    iteration_id VARCHAR PRIMARY KEY,
    mission_id VARCHAR NOT NULL REFERENCES missions(mission_id),
    verdict VARCHAR NOT NULL,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS candidate_artifacts (
    candidate_id VARCHAR PRIMARY KEY,
    mission_id VARCHAR NOT NULL REFERENCES missions(mission_id),
    iteration_id VARCHAR NOT NULL UNIQUE REFERENCES iterations(iteration_id),
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS evaluation_artifacts (
    evaluation_id VARCHAR PRIMARY KEY,
    mission_id VARCHAR NOT NULL REFERENCES missions(mission_id),
    candidate_id VARCHAR NOT NULL REFERENCES candidate_artifacts(candidate_id),
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS registry_revisions (
    revision_id VARCHAR PRIMARY KEY,
    registry_kind VARCHAR NOT NULL,
    asset_id VARCHAR NOT NULL,
    parent_revision_id VARCHAR,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS research_memory (
    event_id VARCHAR PRIMARY KEY,
    mission_id VARCHAR,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS approvals (
    approval_id VARCHAR PRIMARY KEY,
    approval_class VARCHAR NOT NULL,
    subject_id VARCHAR NOT NULL,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS checkpoints (
    mission_id VARCHAR PRIMARY KEY REFERENCES missions(mission_id),
    last_iteration_id VARCHAR REFERENCES iterations(iteration_id),
    budget_usage_json VARCHAR NOT NULL,
    updated_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS deployment_envelopes (
    deployment_id VARCHAR PRIMARY KEY,
    payload_json VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS consumed_nonces (
    nonce VARCHAR PRIMARY KEY,
    consumed_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS run_journal (
    event_id VARCHAR PRIMARY KEY,
    mission_id VARCHAR,
    event_kind VARCHAR NOT NULL,
    record_id VARCHAR NOT NULL,
    content_hash VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);

INSERT OR IGNORE INTO schema_migrations VALUES ('001_control_plane', CAST(current_timestamp AS VARCHAR));
