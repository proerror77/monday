-- Separate offline migration. Preserve existing rows as audit history; only
-- Builds with a verified release record can be read for a new Run.
CREATE TABLE research.build_releases (
  artifact_sha256 text PRIMARY KEY REFERENCES research.build_artifacts(artifact_sha256),
  receipt_sha256 text NOT NULL CHECK (receipt_sha256 ~ '^[0-9a-f]{64}$'),
  trust_sha256 text NOT NULL CHECK (trust_sha256 ~ '^[0-9a-f]{64}$'),
  document jsonb NOT NULL,
  imported_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TRIGGER immutable_build_releases BEFORE UPDATE OR DELETE ON research.build_releases
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
REVOKE ALL ON research.build_releases FROM PUBLIC;
