-- Additive offline migration. Apply paused, after quiescing every old writer.
-- No legacy operation is backfilled as NotStarted: missing history is unknown.
CREATE TABLE research.provider_create_operations (
  task_id text NOT NULL REFERENCES research.tasks(task_id),
  attempt integer NOT NULL CHECK (attempt > 0),
  fence bigint NOT NULL CHECK (fence > 0),
  kind text NOT NULL CHECK (kind IN ('execution','identity')),
  cluster text NOT NULL CHECK (length(cluster) BETWEEN 1 AND 253),
  namespace text NOT NULL CHECK (length(namespace) BETWEEN 1 AND 63),
  resource_name text NOT NULL CHECK (length(resource_name) BETWEEN 1 AND 63),
  task_revision bigint NOT NULL CHECK (task_revision > 0),
  state text NOT NULL CHECK (state IN ('not_started','pending','observed','rejected','not_sent')),
  request_sha256 text CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  public_request jsonb,
  secret_data_sha256 text CHECK (secret_data_sha256 ~ '^[0-9a-f]{64}$'),
  observed_uid text CHECK (length(observed_uid) BETWEEN 1 AND 128),
  rejection_code integer,
  PRIMARY KEY(task_id,attempt,fence,kind),
  CHECK ((state='not_started' AND request_sha256 IS NULL AND public_request IS NULL AND secret_data_sha256 IS NULL)
      OR (state<>'not_started' AND request_sha256 IS NOT NULL AND public_request IS NOT NULL
          AND jsonb_typeof(public_request)='object'
          AND NOT public_request ? 'data' AND NOT public_request ? 'stringData'
          AND ((kind='identity' AND secret_data_sha256 IS NOT NULL) OR (kind='execution' AND secret_data_sha256 IS NULL)))),
  CHECK ((state='observed') = (observed_uid IS NOT NULL)),
  CHECK ((state='rejected') = (rejection_code IS NOT NULL)),
  CHECK (rejection_code IS NULL OR rejection_code IN (400,401,403,404,405,406,410,411,413,414,415,422))
);

CREATE FUNCTION research.guard_provider_create_operation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider create evidence cannot be deleted'; END IF;
  IF TG_OP='INSERT' THEN
    IF NEW.state <> 'not_started' THEN RAISE EXCEPTION 'create operation must start before provider I/O'; END IF;
    RETURN NEW;
  END IF;
  IF ROW(NEW.task_id,NEW.attempt,NEW.fence,NEW.kind,NEW.cluster,NEW.namespace,NEW.resource_name,NEW.task_revision)
     IS DISTINCT FROM ROW(OLD.task_id,OLD.attempt,OLD.fence,OLD.kind,OLD.cluster,OLD.namespace,OLD.resource_name,OLD.task_revision)
  THEN RAISE EXCEPTION 'immutable provider create scope'; END IF;
  IF OLD.state <> 'not_started' AND
     ROW(NEW.request_sha256,NEW.public_request,NEW.secret_data_sha256)
       IS DISTINCT FROM ROW(OLD.request_sha256,OLD.public_request,OLD.secret_data_sha256)
  THEN RAISE EXCEPTION 'immutable original provider request'; END IF;
  IF NEW IS NOT DISTINCT FROM OLD THEN RETURN NEW; END IF;
  IF NOT ((OLD.state='not_started' AND NEW.state='pending') OR
          (OLD.state='pending' AND NEW.state IN ('observed','rejected','not_sent')))
  THEN RAISE EXCEPTION 'illegal provider create transition'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER guard_provider_create_operation BEFORE INSERT OR UPDATE OR DELETE
  ON research.provider_create_operations FOR EACH ROW EXECUTE FUNCTION research.guard_provider_create_operation();

-- SQL is the final quota/refund boundary, including callers bypassing Rust.
-- SECURITY DEFINER grants no writes and exposes no callable evidence override.
CREATE FUNCTION research.guard_provider_create_settlement() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, research AS $$
DECLARE expected integer; present integer; unknown integer;
BEGIN
  IF OLD.state IN ('launching','running','stopping') AND NEW.state NOT IN ('launching','running','stopping') THEN
    expected := CASE WHEN OLD.document->'spec'->>'kind'='cex_campaign' THEN 2 ELSE 1 END;
    SELECT count(*), count(*) FILTER (WHERE state='pending') INTO present, unknown
      FROM research.provider_create_operations
      WHERE task_id=OLD.task_id AND attempt=(OLD.document->>'attempt')::integer
        AND fence=(OLD.document->>'fence')::bigint;
    IF present <> expected OR unknown <> 0 THEN
      RAISE EXCEPTION 'unresolved provider create retains task allocation';
    END IF;
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER guard_provider_create_settlement BEFORE UPDATE ON research.tasks
  FOR EACH ROW EXECUTE FUNCTION research.guard_provider_create_settlement();
REVOKE ALL ON research.provider_create_operations FROM PUBLIC;
REVOKE ALL ON FUNCTION research.guard_provider_create_operation() FROM PUBLIC;
REVOKE ALL ON FUNCTION research.guard_provider_create_settlement() FROM PUBLIC;
