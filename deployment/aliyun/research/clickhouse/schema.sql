CREATE DATABASE IF NOT EXISTS monday_analytics;

CREATE TABLE IF NOT EXISTS monday_analytics.dataset_partitions
(
    dataset_id String,
    venue LowCardinality(String),
    market LowCardinality(String),
    dataset LowCardinality(String),
    symbol LowCardinality(String) DEFAULT '',
    schema_version LowCardinality(String),
    format LowCardinality(String),
    start_event_time DateTime64(6, 'UTC'),
    end_event_time DateTime64(6, 'UTC'),
    available_time DateTime64(6, 'UTC'),
    event_count UInt64,
    compressed_bytes UInt64,
    replay_safe Bool,
    sequence_gap_count UInt32,
    oss_uri String,
    sha256 FixedString(64),
    indexed_at DateTime64(6, 'UTC') DEFAULT now64(6)
)
ENGINE = ReplacingMergeTree(indexed_at)
PARTITION BY toYYYYMM(start_event_time)
ORDER BY
(
    venue,
    market,
    dataset,
    symbol,
    start_event_time,
    oss_uri
);

CREATE TABLE IF NOT EXISTS monday_analytics.lob_features_100ms
(
    event_time DateTime64(6, 'UTC'),
    available_time DateTime64(6, 'UTC'),
    ingest_time DateTime64(6, 'UTC'),
    venue LowCardinality(String),
    market LowCardinality(String),
    symbol LowCardinality(String),
    source_sequence UInt64,
    best_bid Float64,
    best_ask Float64,
    mid_price Float64,
    spread_bps Float64,
    bid_depth_10 Float64,
    ask_depth_10 Float64,
    depth_imbalance_10 Float64,
    order_flow_imbalance Float64,
    trade_count UInt32,
    buy_volume Float64,
    sell_volume Float64,
    realized_volatility Float64,
    feature_version LowCardinality(String),
    source_manifest_sha256 FixedString(64)
)
ENGINE = MergeTree
PARTITION BY toYYYYMMDD(event_time)
ORDER BY (venue, market, symbol, event_time, source_sequence)
TTL event_time + INTERVAL 30 DAY DELETE
SETTINGS index_granularity = 8192;

CREATE TABLE IF NOT EXISTS monday_analytics.lob_snapshots_1s
(
    event_time DateTime64(6, 'UTC'),
    available_time DateTime64(6, 'UTC'),
    venue LowCardinality(String),
    market LowCardinality(String),
    symbol LowCardinality(String),
    source_sequence UInt64,
    bid_prices Array(Float64),
    bid_quantities Array(Float64),
    ask_prices Array(Float64),
    ask_quantities Array(Float64),
    source_manifest_sha256 FixedString(64)
)
ENGINE = MergeTree
PARTITION BY toYYYYMMDD(event_time)
ORDER BY (venue, market, symbol, event_time, source_sequence)
TTL event_time + INTERVAL 14 DAY DELETE
SETTINGS index_granularity = 4096;

CREATE TABLE IF NOT EXISTS monday_analytics.analytics_backtest_metrics
(
    run_id String,
    experiment_id String,
    strategy_version String,
    image_digest String,
    dataset_manifest_sha256 FixedString(64),
    started_at DateTime64(6, 'UTC'),
    finished_at DateTime64(6, 'UTC'),
    venue LowCardinality(String),
    market LowCardinality(String),
    symbol LowCardinality(String),
    parameter_json String,
    total_pnl Float64,
    trades UInt64,
    win_rate Float64,
    max_drawdown Float64,
    max_position Float64,
    result_oss_uri String,
    result_manifest_sha256 FixedString(64),
    indexed_at DateTime64(6, 'UTC') DEFAULT now64(6)
)
ENGINE = ReplacingMergeTree(indexed_at)
PARTITION BY toYYYYMM(started_at)
ORDER BY (experiment_id, run_id);
