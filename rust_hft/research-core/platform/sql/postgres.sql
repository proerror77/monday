-- Offline migration. Applying it and changing production authority are separate
-- operator actions. Installation starts paused and never activates a backend.
CREATE SCHEMA IF NOT EXISTS research;
CREATE TABLE research.authority (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  mode text NOT NULL CHECK (mode IN ('paused', 'postgres')),
  legacy_quiescence_sha256 text,
  migration_receipt_sha256 text,
  concurrency_limit integer NOT NULL CHECK (concurrency_limit BETWEEN 1 AND 1024),
  CHECK (mode <> 'postgres' OR (
    legacy_quiescence_sha256 IS NOT NULL AND migration_receipt_sha256 IS NOT NULL AND
    legacy_quiescence_sha256 ~ '^[0-9a-f]{64}$' AND
    migration_receipt_sha256 ~ '^[0-9a-f]{64}$'))
);
INSERT INTO research.authority VALUES (true, 'paused', NULL, NULL, 1);

CREATE TABLE research.inputs (
  manifest_sha256 text PRIMARY KEY CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
  kind text NOT NULL CHECK (kind IN ('plan','prepared')),
  document jsonb NOT NULL
);
CREATE TABLE research.views (
  view_id text PRIMARY KEY CHECK (view_id ~ '^[0-9a-f]{64}$'),
  manifest_sha256 text NOT NULL UNIQUE CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
  manifest jsonb NOT NULL,
  source_receipt_sha256 text NOT NULL CHECK (source_receipt_sha256 ~ '^[0-9a-f]{64}$'),
  published_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE research.backends (
  acceptance_sha256 text PRIMARY KEY CHECK (acceptance_sha256 ~ '^[0-9a-f]{64}$'),
  acceptance jsonb NOT NULL,
  enabled boolean NOT NULL DEFAULT false
);
CREATE TABLE research.experiments (
  experiment_sha256 text PRIMARY KEY CHECK (experiment_sha256 ~ '^[0-9a-f]{64}$'),
  tenant text NOT NULL, document jsonb NOT NULL
);
CREATE TABLE research.build_artifacts (
  artifact_sha256 text PRIMARY KEY CHECK (artifact_sha256 ~ '^[0-9a-f]{64}$'),
  build_sha256 text NOT NULL CHECK (build_sha256 ~ '^[0-9a-f]{64}$'),
  document jsonb NOT NULL
);
CREATE TABLE research.runs (
  run_sha256 text PRIMARY KEY CHECK (run_sha256 ~ '^[0-9a-f]{64}$'),
  experiment_sha256 text NOT NULL REFERENCES research.experiments(experiment_sha256),
  build_artifact_sha256 text NOT NULL REFERENCES research.build_artifacts(artifact_sha256),
  tenant text NOT NULL, document jsonb NOT NULL
);
CREATE TABLE research.sessions (
  session_sha256 text PRIMARY KEY CHECK (session_sha256 ~ '^[0-9a-f]{64}$'),
  experiment_sha256 text NOT NULL REFERENCES research.experiments(experiment_sha256),
  tenant text NOT NULL, document jsonb NOT NULL
);
CREATE TABLE research.session_snapshots (
  snapshot_sha256 text PRIMARY KEY CHECK (snapshot_sha256 ~ '^[0-9a-f]{64}$'),
  session_sha256 text NOT NULL REFERENCES research.sessions(session_sha256),
  parent_snapshot_sha256 text REFERENCES research.session_snapshots(snapshot_sha256),
  document jsonb NOT NULL, recorded_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
-- Explicit subscriptions, distinct from Session ownership. Intents are committed
-- with terminal task evidence; delivery to a provider is a separate operation.
CREATE TABLE research.subscriptions (
  session_sha256 text NOT NULL REFERENCES research.sessions(session_sha256),
  run_sha256 text NOT NULL REFERENCES research.runs(run_sha256),
  PRIMARY KEY(session_sha256,run_sha256)
);
CREATE TABLE research.completion_intents (
  intent_sha256 text PRIMARY KEY CHECK (intent_sha256 ~ '^[0-9a-f]{64}$'),
  session_sha256 text NOT NULL REFERENCES research.sessions(session_sha256),
  run_sha256 text NOT NULL REFERENCES research.runs(run_sha256),
  task_id text NOT NULL,
  terminal_revision bigint NOT NULL,
  document jsonb NOT NULL,
  UNIQUE(session_sha256,run_sha256)
);
-- Native governance import is an explicit Gate, not a caller-supplied bool.
-- The task service has no INSERT privilege on admissions or revocations.
CREATE TABLE research.admissions (
  request_sha256 text PRIMARY KEY CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  document jsonb NOT NULL,
  CHECK (document->>'request_sha256'=request_sha256)
);
CREATE TABLE research.revocations (
  request_sha256 text PRIMARY KEY REFERENCES research.admissions(request_sha256),
  reason_receipt_sha256 text NOT NULL CHECK (reason_receipt_sha256 ~ '^[0-9a-f]{64}$'),
  revoked_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE research.tasks (
  task_id text PRIMARY KEY CHECK (task_id ~ '^[0-9a-f]{64}$'),
  tenant text NOT NULL,
  idempotency_key text NOT NULL,
  request_sha256 text NOT NULL CHECK (request_sha256 = task_id),
  run_manifest_sha256 text NOT NULL UNIQUE REFERENCES research.runs(run_sha256),
  view_manifest_sha256 text NOT NULL REFERENCES research.inputs(manifest_sha256),
  state text NOT NULL CHECK (state IN ('queued','launching','running','stopping','succeeded','failed','cancelled','timed_out')),
  document jsonb NOT NULL,
  revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  UNIQUE (tenant, idempotency_key),
  CHECK (document->>'id' = task_id),
  CHECK (document->>'state' = state)
);
CREATE INDEX research_queue ON research.tasks(created_at, task_id) WHERE state = 'queued';
CREATE TABLE research.events (
  event_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  task_id text NOT NULL REFERENCES research.tasks(task_id),
  revision bigint NOT NULL,
  event text NOT NULL,
  document jsonb NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  UNIQUE(task_id, revision)
);
CREATE TABLE research.results (
  task_id text PRIMARY KEY REFERENCES research.tasks(task_id),
  attempt integer NOT NULL CHECK (attempt > 0),
  fence bigint NOT NULL CHECK (fence > 0),
  receipt_sha256 text NOT NULL CHECK (receipt_sha256 ~ '^[0-9a-f]{64}$'),
  receipt jsonb NOT NULL,
  accepted_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE FUNCTION research.immutable_record() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'immutable research record'; END $$;
CREATE TRIGGER immutable_build_artifacts BEFORE UPDATE OR DELETE ON research.build_artifacts FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_subscriptions BEFORE UPDATE OR DELETE ON research.subscriptions FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_completion_intents BEFORE UPDATE OR DELETE ON research.completion_intents FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_experiments BEFORE UPDATE OR DELETE ON research.experiments FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_runs BEFORE UPDATE OR DELETE ON research.runs FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_sessions BEFORE UPDATE OR DELETE ON research.sessions FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_session_snapshots BEFORE UPDATE OR DELETE ON research.session_snapshots FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_admissions BEFORE UPDATE OR DELETE ON research.admissions
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_revocations BEFORE UPDATE OR DELETE ON research.revocations
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_views BEFORE UPDATE OR DELETE ON research.views
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_inputs BEFORE UPDATE OR DELETE ON research.inputs
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_events BEFORE UPDATE OR DELETE ON research.events
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_results BEFORE UPDATE OR DELETE ON research.results
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
-- Operator must assign distinct submitter, data-publisher, reconciler and
-- authority-owner roles. Workers never receive direct PG write credentials.
REVOKE ALL ON SCHEMA research FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA research FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA research FROM PUBLIC;
