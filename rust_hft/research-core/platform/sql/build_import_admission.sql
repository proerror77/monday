-- Offline installation after verified_build_release.sql. The trusted schema
-- owner installs this migration; neither CI nor the importer may do so.
CREATE TABLE research.build_import_admissions (
  build_sha256 text NOT NULL CHECK (build_sha256 ~ '^[0-9a-f]{64}$'),
  image_sha256 text NOT NULL CHECK (image_sha256 ~ '^[0-9a-f]{64}$'),
  publication_proof_sha256 text NOT NULL CHECK (publication_proof_sha256 ~ '^[0-9a-f]{64}$'),
  envelope_sha256 text NOT NULL CHECK (envelope_sha256 ~ '^[0-9a-f]{64}$'),
  revision bigint NOT NULL CHECK (revision > 0),
  expires_ms bigint NOT NULL,
  revoked boolean NOT NULL,
  document jsonb NOT NULL,
  PRIMARY KEY (build_sha256,image_sha256,publication_proof_sha256),
  CHECK ((document->>'schema' = '2'
    AND document#>>'{admission,schema}' = '2'
    AND document#>>'{admission,build_sha256}' = build_sha256
    AND document#>>'{admission,image_sha256}' = image_sha256
    AND document#>>'{admission,publication_proof_sha256}' = publication_proof_sha256
    AND (document#>>'{admission,revision}')::bigint = revision
    AND (document#>>'{admission,expires_ms}')::bigint = expires_ms
    AND (document#>>'{admission,revoked}')::boolean = revoked) IS TRUE)
);
REVOKE ALL ON research.build_import_admissions FROM PUBLIC;

-- The trigger holds a shared row lock until the import transaction commits.
-- The independent owner's UPDATE/revocation conflicts with that lock. Owner
-- revocation completion therefore orders before or after import, never between
-- its approval check and commit. State is keyed by selectors, NOT envelope hash:
-- restoring an older signed file cannot restore an older active approval.
CREATE FUNCTION research.lock_build_import_admission(build_id text,image_id text,proof_id text,envelope_id text) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE approval research.build_import_admissions%ROWTYPE;
BEGIN
  SELECT * INTO approval FROM research.build_import_admissions
    WHERE build_sha256 = build_id AND image_sha256 = image_id
      AND publication_proof_sha256 = proof_id FOR SHARE;
  IF NOT FOUND OR approval.revoked
    OR approval.expires_ms <= floor(extract(epoch FROM clock_timestamp())*1000)::bigint
    OR approval.envelope_sha256 IS DISTINCT FROM envelope_id OR envelope_id IS NULL
  THEN RAISE EXCEPTION 'current independent Build import admission required'; END IF;
END $$;
REVOKE ALL ON FUNCTION research.lock_build_import_admission(text,text,text,text) FROM PUBLIC;
CREATE FUNCTION research.require_build_import_admission() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
  PERFORM research.lock_build_import_admission(NEW.document#>>'{receipt,build_sha256}',
    split_part(NEW.document#>>'{receipt,image}','@sha256:',2),
    NEW.document#>>'{receipt,publication_readback_sha256}',
    current_setting('monday.build_import_admission_sha256',true));
  RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION research.require_build_import_admission() FROM PUBLIC;
CREATE FUNCTION research.monotonic_build_import_admission() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
  IF (NEW.build_sha256,NEW.image_sha256,NEW.publication_proof_sha256)
      IS DISTINCT FROM (OLD.build_sha256,OLD.image_sha256,OLD.publication_proof_sha256)
    OR NEW.revision < OLD.revision
    OR (NEW.revision = OLD.revision AND NEW IS DISTINCT FROM OLD)
  THEN RAISE EXCEPTION 'Build import admission requires a newer signed revision'; END IF;
  RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION research.monotonic_build_import_admission() FROM PUBLIC;
CREATE TRIGGER monotonic_build_import_admission BEFORE UPDATE ON research.build_import_admissions
  FOR EACH ROW EXECUTE FUNCTION research.monotonic_build_import_admission();
CREATE TRIGGER check_build_import_admission BEFORE INSERT ON research.build_releases
  FOR EACH ROW EXECUTE FUNCTION research.require_build_import_admission();
-- Additional expiry check for the native transaction at its default deferred
-- completion point. SQL clients may SET CONSTRAINTS IMMEDIATE, so expiry's
-- mandatory boundary is the serialized insertion gate, not wall-clock COMMIT.
-- Revocation remains serialized through COMMIT regardless of constraint timing.
CREATE CONSTRAINT TRIGGER commit_build_import_admission AFTER INSERT ON research.build_releases
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
  EXECUTE FUNCTION research.require_build_import_admission();

-- Preserve every independently installed approval/revocation, not only the tip.
CREATE TABLE research.build_import_admission_audits (
  event_id bigserial PRIMARY KEY,
  document jsonb NOT NULL,
  envelope_sha256 text NOT NULL,
  actor text NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
REVOKE ALL ON research.build_import_admission_audits FROM PUBLIC;
CREATE TRIGGER immutable_build_import_admission_audits BEFORE UPDATE OR DELETE
  ON research.build_import_admission_audits FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
CREATE FUNCTION research.audit_build_import_admission() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
  INSERT INTO research.build_import_admission_audits(document,envelope_sha256,actor)
    VALUES(NEW.document,NEW.envelope_sha256,session_user);
  RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION research.audit_build_import_admission() FROM PUBLIC;
CREATE TRIGGER audit_build_import_admission AFTER INSERT OR UPDATE
  ON research.build_import_admissions FOR EACH ROW EXECUTE FUNCTION research.audit_build_import_admission();
