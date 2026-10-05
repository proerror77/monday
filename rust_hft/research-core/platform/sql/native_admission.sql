-- Additive offline import. Only a separately trusted native reservation importer
-- may INSERT; task/Agent/worker roles receive SELECT only. Never activates PG.
CREATE TABLE research.native_admission_imports (
  request_sha256 text PRIMARY KEY CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  tenant text NOT NULL CHECK (length(tenant) BETWEEN 1 AND 128),
  operation_sha256 text NOT NULL UNIQUE CHECK (operation_sha256 ~ '^[0-9a-f]{64}$'),
  evidence_sha256 text NOT NULL UNIQUE CHECK (evidence_sha256 ~ '^[0-9a-f]{64}$'),
  trust_sha256 text NOT NULL CHECK (trust_sha256 ~ '^[0-9a-f]{64}$'),
  expires_ms bigint NOT NULL CHECK (expires_ms > 0),
  document jsonb NOT NULL,
  trust_document jsonb NOT NULL,
  imported_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  CHECK (document->'evidence'->>'tenant'=tenant),
  CHECK (document->'evidence'->>'operation_sha256'=operation_sha256),
  CHECK (document->>'evidence_sha256'=evidence_sha256),
  CHECK (document->'evidence'->'admission'->>'request_sha256'=request_sha256)
);
CREATE TRIGGER immutable_native_admission_imports BEFORE UPDATE OR DELETE ON research.native_admission_imports
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
REVOKE ALL ON research.native_admission_imports FROM PUBLIC;

-- Preserve the original permit signature, locks, fencing and stop checks.
-- Offline installation by the trusted authority owner. The gateway receives
-- EXECUTE on this function and read access, never admission/task UPDATE grants.
-- This function only holds locks; it cannot change scientific authority or state.
CREATE OR REPLACE FUNCTION research.artifact_write_permit(principal text, task text, ordinal integer, generation bigint)
RETURNS text LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE
  auth research.authority%ROWTYPE;
  current_task research.tasks%ROWTYPE;
  now_ms bigint;
BEGIN
  SELECT * INTO STRICT auth FROM research.authority WHERE singleton FOR SHARE;
  IF auth.mode <> 'postgres' OR auth.legacy_quiescence_sha256 IS NULL OR auth.migration_receipt_sha256 IS NULL THEN
    RAISE EXCEPTION 'artifact authority is paused';
  END IF;
  SELECT * INTO STRICT current_task FROM research.tasks WHERE task_id=task AND tenant=principal FOR SHARE;
  PERFORM request_sha256 FROM research.admissions WHERE request_sha256=task FOR UPDATE;
  IF NOT FOUND THEN RAISE EXCEPTION 'artifact admission absent'; END IF;
  -- A new statement sees revocation committed while admission locking waited.
  IF EXISTS(SELECT 1 FROM research.revocations WHERE request_sha256=task) THEN
    RAISE EXCEPTION 'artifact admission revoked';
  END IF;
  now_ms := floor(extract(epoch FROM clock_timestamp())*1000)::bigint;
  IF NOT EXISTS(SELECT 1 FROM research.native_admission_imports n
      WHERE n.request_sha256=task AND n.tenant=principal AND n.expires_ms>now_ms) THEN
    RAISE EXCEPTION 'native artifact admission absent or expired';
  END IF;
  IF current_task.state <> 'running'
     OR (current_task.document->>'attempt')::integer IS DISTINCT FROM ordinal
     OR (current_task.document->>'fence')::bigint IS DISTINCT FROM generation
     OR (current_task.document->'lease'->>'expires_ms')::bigint IS NULL
     OR (current_task.document->'lease'->>'expires_ms')::bigint <= now_ms
     OR (current_task.document->>'deadline_ms')::bigint IS NULL
     OR (current_task.document->>'deadline_ms')::bigint <= now_ms THEN
    RAISE EXCEPTION 'artifact writer is no longer admitted';
  END IF;
  RETURN current_task.document->'spec'->>'output_prefix' || '/' || task || '/' || ordinal::text || '/';
END $$;
REVOKE ALL ON FUNCTION research.artifact_write_permit(text,text,integer,bigint) FROM PUBLIC;
