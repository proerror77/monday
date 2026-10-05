-- Install after native_admission.sql. This preserves manual PG revocation rows.
CREATE TABLE research.native_request_revocations (
  evidence_sha256 text PRIMARY KEY CHECK (evidence_sha256 ~ '^[0-9a-f]{64}$'),
  request_sha256 text NOT NULL REFERENCES research.native_admission_imports(request_sha256),
  tenant text NOT NULL CHECK (length(tenant) BETWEEN 1 AND 128),
  operation_sha256 text NOT NULL CHECK (operation_sha256 ~ '^[0-9a-f]{64}$'),
  family_id text NOT NULL CHECK (length(family_id) BETWEEN 1 AND 128),
  root_grant_sha256 text NOT NULL CHECK (root_grant_sha256 ~ '^[0-9a-f]{64}$'),
  reason_receipt_sha256 text NOT NULL CHECK (reason_receipt_sha256 ~ '^[0-9a-f]{64}$'),
  effective_ms bigint NOT NULL CHECK (effective_ms > 0),
  issued_ms bigint NOT NULL CHECK (issued_ms > 0),
  trust_sha256 text NOT NULL CHECK (trust_sha256 ~ '^[0-9a-f]{64}$'),
  document jsonb NOT NULL,
  trust_document jsonb NOT NULL,
  imported_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  UNIQUE(request_sha256,reason_receipt_sha256),
  CHECK (document->>'evidence_sha256'=evidence_sha256),
  CHECK (document->'evidence'->>'request_sha256'=request_sha256),
  CHECK (document->'evidence'->>'tenant'=tenant),
  CHECK (document->'evidence'->>'operation_sha256'=operation_sha256),
  CHECK (document->'evidence'->>'family_id'=family_id),
  CHECK (document->'evidence'->>'root_grant_sha256'=root_grant_sha256),
  CHECK (document->'evidence'->>'reason_receipt_sha256'=reason_receipt_sha256),
  CHECK ((document->'evidence'->>'effective_ms')::bigint=effective_ms),
  CHECK ((document->'evidence'->>'issued_ms')::bigint=issued_ms)
);
CREATE INDEX native_request_revocations_deadline ON research.native_request_revocations(request_sha256,effective_ms);
CREATE TRIGGER immutable_native_request_revocations BEFORE UPDATE OR DELETE ON research.native_request_revocations
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
REVOKE ALL ON research.native_request_revocations FROM PUBLIC;

-- A fixed read-only predicate lets limited worker roles consume the deadline
-- without reading signed witness documents or gaining any write privilege.
CREATE FUNCTION research.native_request_deadline_ms(task text)
RETURNS bigint LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog AS $$
  SELECT LEAST(n.expires_ms,COALESCE(
    (SELECT MIN(r.effective_ms) FROM research.native_request_revocations r WHERE r.request_sha256=n.request_sha256),
    n.expires_ms)) FROM research.native_admission_imports n WHERE n.request_sha256=task
$$;
REVOKE ALL ON FUNCTION research.native_request_deadline_ms(text) FROM PUBLIC;

-- Same signature and lock ordering as native_admission.sql. The importer locks
-- this admission row too, so revocation is ordered against upload publication.
CREATE OR REPLACE FUNCTION research.artifact_write_permit(principal text, task text, ordinal integer, generation bigint)
RETURNS text LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog AS $$
DECLARE
  auth research.authority%ROWTYPE;
  current_task research.tasks%ROWTYPE;
  now_ms bigint;
  native_deadline bigint;
BEGIN
  SELECT * INTO STRICT auth FROM research.authority WHERE singleton FOR SHARE;
  IF auth.mode <> 'postgres' OR auth.legacy_quiescence_sha256 IS NULL OR auth.migration_receipt_sha256 IS NULL THEN
    RAISE EXCEPTION 'artifact authority is paused';
  END IF;
  SELECT * INTO STRICT current_task FROM research.tasks WHERE task_id=task AND tenant=principal FOR SHARE;
  PERFORM request_sha256 FROM research.admissions WHERE request_sha256=task FOR UPDATE;
  IF NOT FOUND THEN RAISE EXCEPTION 'artifact admission absent'; END IF;
  IF EXISTS(SELECT 1 FROM research.revocations WHERE request_sha256=task) THEN
    RAISE EXCEPTION 'artifact admission revoked';
  END IF;
  now_ms := floor(extract(epoch FROM clock_timestamp())*1000)::bigint;
  native_deadline := research.native_request_deadline_ms(task);
  IF native_deadline IS NULL OR native_deadline<=now_ms OR NOT EXISTS(
    SELECT 1 FROM research.native_admission_imports n WHERE n.request_sha256=task AND n.tenant=principal) THEN
    RAISE EXCEPTION 'native artifact admission absent, revoked or expired';
  END IF;
  IF current_task.state <> 'running'
     OR (current_task.document->>'attempt')::integer IS DISTINCT FROM ordinal
     OR (current_task.document->>'fence')::bigint IS DISTINCT FROM generation
     OR (current_task.document->'lease'->>'expires_ms')::bigint IS NULL
     OR (current_task.document->'lease'->>'expires_ms')::bigint<=now_ms
     OR (current_task.document->>'deadline_ms')::bigint IS NULL
     OR LEAST((current_task.document->>'deadline_ms')::bigint,native_deadline)<=now_ms THEN
    RAISE EXCEPTION 'artifact writer is no longer admitted';
  END IF;
  RETURN current_task.document->'spec'->>'output_prefix' || '/' || task || '/' || ordinal::text || '/';
END $$;
REVOKE ALL ON FUNCTION research.artifact_write_permit(text,text,integer,bigint) FROM PUBLIC;
