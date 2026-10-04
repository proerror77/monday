-- Offline installation. Data import authority is separate from research task
-- activation and defaults paused. No application connection installs this SQL.
CREATE SCHEMA market_data;
CREATE TABLE market_data.authority (
  singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton),
  mode text NOT NULL CHECK(mode IN ('paused','import')),
  writer_activation_receipt_sha256 text,
  legacy_writer_quiescence_sha256 text,
  CHECK(mode='paused' OR (writer_activation_receipt_sha256 ~ '^[0-9a-f]{64}$'
    AND legacy_writer_quiescence_sha256 ~ '^[0-9a-f]{64}$'
    AND writer_activation_receipt_sha256 IS NOT NULL AND legacy_writer_quiescence_sha256 IS NOT NULL))
);
INSERT INTO market_data.authority VALUES(true,'paused',NULL,NULL);
CREATE TABLE market_data.batches (
  batch_id text PRIMARY KEY CHECK(batch_id ~ '^[0-9a-f]{64}$'),
  scope_sha256 text NOT NULL CHECK(scope_sha256 ~ '^[0-9a-f]{64}$'),
  manifest_sha256 text NOT NULL UNIQUE CHECK(manifest_sha256 ~ '^[0-9a-f]{64}$'),
  manifest jsonb NOT NULL,
  source_start_ns bigint NOT NULL CHECK(source_start_ns>0),
  source_end_ns bigint NOT NULL CHECK(source_end_ns>=source_start_ns),
  registered_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE market_data.attempts (
  fence bigserial PRIMARY KEY,
  generation text NOT NULL UNIQUE CHECK(generation ~ '^[0-9a-f]{64}$'),
  batch_id text NOT NULL REFERENCES market_data.batches,
  started_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE market_data.events (
  event_id bigserial PRIMARY KEY,
  fence bigint NOT NULL REFERENCES market_data.attempts,
  event text NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE market_data.publications (
  batch_id text PRIMARY KEY REFERENCES market_data.batches,
  receipt_sha256 text NOT NULL UNIQUE CHECK(receipt_sha256 ~ '^[0-9a-f]{64}$'),
  fence bigint NOT NULL REFERENCES market_data.attempts,
  receipt jsonb NOT NULL,
  published_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE market_data.watermarks (
  scope_sha256 text PRIMARY KEY CHECK(scope_sha256 ~ '^[0-9a-f]{64}$'),
  source_end_ns bigint NOT NULL CHECK(source_end_ns>0),
  batch_id text NOT NULL REFERENCES market_data.publications(batch_id),
  revision bigint NOT NULL CHECK(revision>0)
);
CREATE FUNCTION market_data.immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'market data evidence is append-only'; END $$;
CREATE TRIGGER immutable_batch BEFORE UPDATE OR DELETE ON market_data.batches
  FOR EACH ROW EXECUTE FUNCTION market_data.immutable();
CREATE TRIGGER immutable_attempt BEFORE UPDATE OR DELETE ON market_data.attempts
  FOR EACH ROW EXECUTE FUNCTION market_data.immutable();
CREATE TRIGGER immutable_event BEFORE UPDATE OR DELETE ON market_data.events
  FOR EACH ROW EXECUTE FUNCTION market_data.immutable();
CREATE TRIGGER immutable_publication BEFORE UPDATE OR DELETE ON market_data.publications
  FOR EACH ROW EXECUTE FUNCTION market_data.immutable();
CREATE FUNCTION market_data.verify_watermark() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_start bigint; source_end bigint; source_scope text;
BEGIN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'data watermark cannot be removed'; END IF;
  SELECT b.source_start_ns,b.source_end_ns,b.scope_sha256
    INTO STRICT source_start,source_end,source_scope
    FROM market_data.batches b JOIN market_data.publications p USING(batch_id)
    WHERE b.batch_id=NEW.batch_id;
  IF NEW.scope_sha256<>source_scope OR NEW.source_end_ns<>source_end THEN
    RAISE EXCEPTION 'watermark does not match an immutable published batch';
  END IF;
  IF TG_OP='INSERT' AND NEW.revision<>1 THEN
    RAISE EXCEPTION 'invalid initial data watermark revision';
  END IF;
  IF TG_OP='UPDATE' AND (NEW.scope_sha256<>OLD.scope_sha256 OR
      NEW.revision<>OLD.revision+1 OR source_start<>OLD.source_end_ns OR
      NEW.source_end_ns<=OLD.source_end_ns) THEN
    RAISE EXCEPTION 'data watermark gap, overlap, regression, or stale revision';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER verified_watermark BEFORE INSERT OR UPDATE OR DELETE ON market_data.watermarks
  FOR EACH ROW EXECUTE FUNCTION market_data.verify_watermark();
