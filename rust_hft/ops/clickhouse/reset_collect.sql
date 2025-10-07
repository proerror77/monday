-- Reset collector database to correct name
DROP DATABASE IF EXISTS hft_db;
CREATE DATABASE IF NOT EXISTS HTf_db_collect;

-- Common snapshot table (microsecond timestamp)
CREATE TABLE IF NOT EXISTS HTf_db_collect.snapshot_books (
    ts UInt64,
    symbol String,
    venue String,
    last_id UInt64,
    bids_px Array(Float64),
    bids_qty Array(Float64),
    asks_px Array(Float64),
    asks_qty Array(Float64),
    source String
) ENGINE = MergeTree()
ORDER BY (symbol, ts)
SETTINGS index_granularity = 8192;

