CREATE DATABASE IF NOT EXISTS monday_analytics;

-- Immutable source versions have a single ingestion controller. FINAL is required
-- when reading a ready version: retries can leave physical duplicate parts until
-- ReplacingMergeTree merges them. Readback verifies the actual data before ready.
-- Retention must be explicit and reference-aware; no TTL deletes pinned runs.
CREATE TABLE IF NOT EXISTS monday_analytics.cex_analytics_partitions
(
    partition_identity String,
    manifest_sha256 String,
    artifact_sha256 String,
    source_revision String,
    venue LowCardinality(String),
    market LowCardinality(String),
    symbol LowCardinality(String),
    start_time_us Int64,
    end_time_us Int64,
    schema_version LowCardinality(String),
    dataset_kind LowCardinality(String),
    row_count UInt64,
    materialization_state LowCardinality(String),
    materialization_version UInt64
)
ENGINE = ReplacingMergeTree(materialization_version)
ORDER BY (partition_identity);

CREATE TABLE IF NOT EXISTS monday_analytics.cex_replay_events
(
    partition_identity String,
    manifest_sha256 String,
    artifact_sha256 String,
    source_revision String,
    venue LowCardinality(String),
    market LowCardinality(String),
    symbol LowCardinality(String),
    start_time_us Int64,
    end_time_us Int64,
    schema_version LowCardinality(String),
    row_identity String,
    materialization_version UInt64,
    event_time_us Int64,
    sequence UInt64,
    event LowCardinality(String),
    payload_json String
)
ENGINE = ReplacingMergeTree(materialization_version)
ORDER BY (partition_identity, row_identity);

CREATE TABLE IF NOT EXISTS monday_analytics.cex_pit_features
(
    partition_identity String,
    manifest_sha256 String,
    artifact_sha256 String,
    source_revision String,
    venue LowCardinality(String),
    market LowCardinality(String),
    symbol LowCardinality(String),
    start_time_us Int64,
    end_time_us Int64,
    schema_version LowCardinality(String),
    row_identity String,
    materialization_version UInt64,
    event_time_us Int64,
    feature_available_time_us Int64,
    label_available_time_us Int64,
    ingestion_time_us Int64,
    features_json String,
    label Float64
)
ENGINE = ReplacingMergeTree(materialization_version)
ORDER BY (partition_identity, row_identity);

CREATE TABLE IF NOT EXISTS monday_analytics.cex_backtest_results
(
    partition_identity String,
    manifest_sha256 String,
    artifact_sha256 String,
    source_revision String,
    venue LowCardinality(String),
    market LowCardinality(String),
    symbol LowCardinality(String),
    start_time_us Int64,
    end_time_us Int64,
    schema_version LowCardinality(String),
    row_identity String,
    materialization_version UInt64,
    result_json String
)
ENGINE = ReplacingMergeTree(materialization_version)
ORDER BY (partition_identity, row_identity);

-- Numerical training inputs are separate from the historical PIT JSON format.
-- Features cannot expose targets through their table or column projection.
CREATE TABLE IF NOT EXISTS monday_analytics.cex_market_feature_frames
(
    dataset_identity String,
    row_identity String,
    series_id UInt64,
    observed_at_ms Int64,
    feature_max_available_at_ms Int64,
    channels Array(Float32),
    materialization_version UInt64
)
ENGINE = ReplacingMergeTree(materialization_version)
ORDER BY (dataset_identity, observed_at_ms, series_id);

CREATE TABLE IF NOT EXISTS monday_analytics.cex_market_target_frames
(
    dataset_identity String,
    row_identity String,
    series_id UInt64,
    observed_at_ms Int64,
    available_at_ms Int64,
    simple_return Float32,
    spread_bps Float64,
    materialization_version UInt64
)
ENGINE = ReplacingMergeTree(materialization_version)
ORDER BY (dataset_identity, observed_at_ms, series_id);

CREATE TABLE IF NOT EXISTS monday_analytics.cex_market_datasets
(
    dataset_identity String,
    dataset_kind LowCardinality(String),
    source_manifest_sha256 String,
    manifest_json String,
    row_count UInt64,
    content_sha256 String,
    first_observed_at_ms Int64,
    last_observed_at_ms Int64,
    materialization_state LowCardinality(String),
    materialization_version UInt64
)
ENGINE = ReplacingMergeTree(materialization_version)
ORDER BY (dataset_identity);
