-- Explicit, offline schema. No raw import or activation is performed by CI.
CREATE DATABASE IF NOT EXISTS research;
-- Rust normalizer verifies exchange update sequences, session identity and
-- archive digests once. A gap starts a new segment and needs another seed.
CREATE TABLE IF NOT EXISTS research.normalized_books (
  source_sha256 FixedString(64), venue LowCardinality(String), instrument String, market LowCardinality(String),
  segment String, ordinal UInt64, event_ns Int64, available_ns Int64,
  bids_price Array(Float64), bids_quantity Array(Float64),
  asks_price Array(Float64), asks_quantity Array(Float64)
) ENGINE = MergeTree ORDER BY (venue, market, instrument, source_sha256, segment, available_ns, ordinal);
CREATE TABLE IF NOT EXISTS research.normalized_events (
  source_sha256 FixedString(64), venue LowCardinality(String), instrument String, market LowCardinality(String),
  segment String, ordinal UInt64, event_ns Int64, available_ns Int64,
  kind Enum8('snapshot'=1,'delta'=2,'trade'=3),
  bids_price Array(Float64), bids_quantity Array(Float64),
  asks_price Array(Float64), asks_quantity Array(Float64),
  trade_price Float64, trade_quantity Float64, buyer_initiated UInt8
) ENGINE = MergeTree ORDER BY (venue, market, instrument, source_sha256, segment, available_ns, ordinal);
CREATE TABLE IF NOT EXISTS research.prepared_features (
  view_id FixedString(64), segment String, ordinal UInt64,
  event_ns Int64, available_ns Int64, values Array(Float64)
) ENGINE = MergeTree ORDER BY (view_id, available_ns, segment, ordinal);
CREATE TABLE IF NOT EXISTS research.prepared_labels (
  view_id FixedString(64), segment String, ordinal UInt64,
  horizon_ns Int64, target_event_ns Int64, mature_ns Int64, value Float64
) ENGINE = MergeTree ORDER BY (view_id, segment, ordinal, horizon_ns);
-- PG owns batch completion and publication. MergeTree keys are not unique;
-- publisher enforces exclusive ownership and exact counts/content on readback.
-- SELECT never treats an uncommitted view_id as published. Correction creates a
-- new source identity and DataView. Published versions are never mutated.
