-- Offline, additive migration. Never changes authority mode or enables compute.
CREATE TABLE research.completion_deliveries (
  intent_sha256 text PRIMARY KEY REFERENCES research.completion_intents(intent_sha256),
  process_generation text NOT NULL CHECK (process_generation ~ '^[0-9a-f]{64}$'),
  native_readback_sha256 text NOT NULL CHECK (native_readback_sha256 ~ '^[0-9a-f]{64}$'),
  document jsonb NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  CHECK (document->>'intent_sha256'=intent_sha256),
  CHECK (document->>'delivery'='accepted'),
  CHECK (document->>'native_readback_sha256'=native_readback_sha256)
);
CREATE TRIGGER immutable_completion_deliveries BEFORE UPDATE OR DELETE
  ON research.completion_deliveries FOR EACH ROW EXECUTE FUNCTION research.immutable_record();
REVOKE ALL ON research.completion_deliveries FROM PUBLIC;
-- Only the trusted Session host may INSERT. Generic Agent and worker roles
-- have no delivery-write privilege; completion intent issuance remains native.
