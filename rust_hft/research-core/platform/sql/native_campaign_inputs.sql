-- Additive offline installation after native_admission.sql. This stages only
-- development input collections; it never activates PG or exposes withheld data.
ALTER TABLE research.inputs DROP CONSTRAINT inputs_kind_check;
ALTER TABLE research.inputs ADD CONSTRAINT inputs_kind_check
  CHECK (kind IN ('plan','prepared','cex_campaign'));
CREATE TABLE research.native_campaign_inputs (
  request_sha256 text PRIMARY KEY REFERENCES research.native_admission_imports(request_sha256),
  manifest_sha256 text NOT NULL REFERENCES research.inputs(manifest_sha256),
  tenant text NOT NULL CHECK (length(tenant) BETWEEN 1 AND 128),
  verification_sha256 text NOT NULL CHECK (verification_sha256 ~ '^[0-9a-f]{64}$'),
  imported_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TRIGGER immutable_native_campaign_inputs BEFORE UPDATE OR DELETE ON research.native_campaign_inputs
  FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
REVOKE ALL ON research.native_campaign_inputs FROM PUBLIC;
