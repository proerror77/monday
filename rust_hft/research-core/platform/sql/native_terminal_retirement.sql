-- Offline additive migration after native_admission/native_campaign_inputs.
-- Default-disabled operator retirement only. No task transition or budget refund.
CREATE TABLE research.native_terminal_retirement_audits (
  evidence_sha256 text PRIMARY KEY CHECK (evidence_sha256 ~ '^[0-9a-f]{64}$'),
  task_id text NOT NULL UNIQUE REFERENCES research.tasks(task_id),
  tenant text NOT NULL CHECK (length(tenant) BETWEEN 1 AND 128),
  terminal_revision bigint NOT NULL CHECK (terminal_revision > 0),
  document jsonb NOT NULL,
  registered_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  CHECK (document->'signed'->>'evidence_sha256'=evidence_sha256),
  CHECK (document->'signed'->'evidence'->>'task_id'=task_id),
  CHECK (document->'signed'->'evidence'->>'tenant'=tenant),
  CHECK ((document->'signed'->'evidence'->>'terminal_revision')::bigint=terminal_revision)
);
CREATE TABLE research.native_terminal_retirement_events (
  evidence_sha256 text NOT NULL REFERENCES research.native_terminal_retirement_audits(evidence_sha256),
  event text NOT NULL CHECK (event IN ('delete_requested','retired')),
  document jsonb NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY(evidence_sha256,event)
);
CREATE FUNCTION research.check_native_retirement_scope() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE original research.tasks%ROWTYPE; signed jsonb; native research.native_admission_imports%ROWTYPE;
BEGIN
  SELECT * INTO STRICT original FROM research.tasks WHERE task_id=NEW.task_id AND tenant=NEW.tenant FOR UPDATE;
  SELECT * INTO STRICT native FROM research.native_admission_imports WHERE request_sha256=NEW.task_id AND tenant=NEW.tenant;
  signed := NEW.document->'signed'->'evidence';
  IF original.revision IS DISTINCT FROM NEW.terminal_revision
    OR original.state NOT IN ('succeeded','failed','cancelled','timed_out')
    OR NOT EXISTS(SELECT 1 FROM research.events e WHERE e.task_id=NEW.task_id AND e.revision=NEW.terminal_revision AND e.event='stop_reconciled' AND e.document=original.document)
    OR signed->>'request_sha256' IS DISTINCT FROM NEW.task_id
    OR signed->>'task_id' IS DISTINCT FROM NEW.task_id
    OR signed->>'tenant' IS DISTINCT FROM NEW.tenant
    OR signed->>'operation_sha256' IS DISTINCT FROM native.operation_sha256
    OR signed->>'native_evidence_sha256' IS DISTINCT FROM native.evidence_sha256
    OR signed->>'run_sha256' IS DISTINCT FROM original.run_manifest_sha256
    OR (signed->>'attempt')::integer IS DISTINCT FROM (original.document->>'attempt')::integer
    OR (signed->>'fence')::bigint IS DISTINCT FROM (original.document->>'fence')::bigint
    OR NEW.document->>'trust_sha256' IS DISTINCT FROM native.trust_sha256 THEN
      RAISE EXCEPTION 'retirement changed immutable native terminal scope';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER bound_native_terminal_retirement_audit BEFORE INSERT ON research.native_terminal_retirement_audits
  FOR EACH ROW EXECUTE FUNCTION research.check_native_retirement_scope();
CREATE FUNCTION research.check_native_retirement_event() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE audit research.native_terminal_retirement_audits%ROWTYPE; signed jsonb;
BEGIN
  SELECT * INTO STRICT audit FROM research.native_terminal_retirement_audits WHERE evidence_sha256=NEW.evidence_sha256;
  signed := audit.document->'signed'->'evidence';
  IF NEW.document IS DISTINCT FROM jsonb_build_object('task_id',signed->>'task_id','terminal_revision',(signed->>'terminal_revision')::bigint,'job_uid',signed->>'job_uid','pod_uid',signed->>'pod_uid')
    OR (NEW.event='retired' AND NOT EXISTS(SELECT 1 FROM research.native_terminal_retirement_events e WHERE e.evidence_sha256=NEW.evidence_sha256 AND e.event='delete_requested')) THEN
    RAISE EXCEPTION 'retirement event changed fixed evidence or lacks durable intent';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER bound_native_terminal_retirement_event BEFORE INSERT ON research.native_terminal_retirement_events
  FOR EACH ROW EXECUTE FUNCTION research.check_native_retirement_event();
REVOKE ALL ON FUNCTION research.check_native_retirement_scope(),research.check_native_retirement_event() FROM PUBLIC;
CREATE TRIGGER immutable_native_terminal_retirement_audits BEFORE UPDATE OR DELETE ON research.native_terminal_retirement_audits
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE TRIGGER immutable_native_terminal_retirement_events BEFORE UPDATE OR DELETE ON research.native_terminal_retirement_events
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
REVOKE ALL ON research.native_terminal_retirement_audits,research.native_terminal_retirement_events FROM PUBLIC;
-- Only the trusted retirement host gets SELECT/INSERT on these two tables and
-- the existing task row-lock permission. Agents/workers get no retirement write.
