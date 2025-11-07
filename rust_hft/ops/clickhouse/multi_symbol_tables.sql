-- 多商品高性能數據收集 ClickHouse 表結構
-- Multi-Symbol High Performance Data Collection ClickHouse Tables

-- 創建數據庫
CREATE DATABASE IF NOT EXISTS hft_db;

-- 使用數據庫
USE hft_db;

-- 1. 增強的市場數據表
CREATE TABLE IF NOT EXISTS enhanced_market_data (
    record_id String,
    symbol LowCardinality(String),
    timestamp UInt64,
    data_type LowCardinality(String),
    
    -- 價格信息
    best_bid Float64,
    best_ask Float64,
    mid_price Float64,
    spread_bps Float64,
    
    -- SIMD優化的流動性指標
    obi_l1 Float64,
    obi_l5 Float64,
    obi_l10 Float64,
    obi_l20 Float64,
    
    -- 深度信息
    bid_volume_l5 Float64,
    ask_volume_l5 Float64,
    total_bid_volume Float64,
    total_ask_volume Float64,
    
    -- 質量指標
    processing_latency_us UInt64,
    data_quality_score Float64,
    sequence_number UInt64,
    
    -- 分區和排序
    date Date DEFAULT toDate(timestamp / 1000000),
    hour UInt8 DEFAULT toHour(toDateTime(timestamp / 1000000))
)
ENGINE = MergeTree()
PARTITION BY (symbol, date)
ORDER BY (symbol, timestamp)
TTL toDateTime(timestamp / 1000000) + INTERVAL 30 DAY
SETTINGS index_granularity = 8192;

-- 2. 交易執行數據表
CREATE TABLE IF NOT EXISTS trade_executions (
    execution_id String,
    symbol LowCardinality(String),
    timestamp UInt64,
    side LowCardinality(String),
    price Float64,
    volume Float64,
    amount Float64,
    trade_id String,
    
    -- 時間分區字段
    date Date DEFAULT toDate(timestamp / 1000000),
    hour UInt8 DEFAULT toHour(toDateTime(timestamp / 1000000))
)
ENGINE = MergeTree()
PARTITION BY (symbol, date)
ORDER BY (symbol, timestamp)
TTL toDateTime(timestamp / 1000000) + INTERVAL 7 DAY
SETTINGS index_granularity = 8192;

-- 3. 數據質量監控表
CREATE TABLE IF NOT EXISTS data_quality_metrics (
    metric_id String,
    symbol LowCardinality(String),
    timestamp UInt64,
    
    -- 質量指標
    completeness_score Float64,
    accuracy_score Float64,
    consistency_score Float64,
    timeliness_score Float64,
    overall_score Float64,
    
    -- 統計信息
    message_count UInt64,
    error_count UInt64,
    latency_avg_us Float64,
    latency_p99_us UInt64,
    
    -- 時間分區
    date Date DEFAULT toDate(timestamp / 1000000)
)
ENGINE = MergeTree()
PARTITION BY date
ORDER BY (symbol, timestamp)
TTL toDateTime(timestamp / 1000000) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;

-- 4. 實時統計物化視圖 (1分鐘聚合)
CREATE MATERIALIZED VIEW IF NOT EXISTS market_data_1min_mv
TO market_data_1min AS
SELECT
    symbol,
    toStartOfMinute(toDateTime(timestamp / 1000000)) as time_1min,
    
    -- 價格統計
    avg(mid_price) as avg_price,
    min(best_bid) as min_bid,
    max(best_ask) as max_ask,
    avg(spread_bps) as avg_spread_bps,
    
    -- 流動性統計
    avg(obi_l5) as avg_obi_l5,
    avg(total_bid_volume + total_ask_volume) as avg_total_volume,
    
    -- 性能統計
    count() as message_count,
    avg(processing_latency_us) as avg_latency_us,
    quantile(0.99)(processing_latency_us) as p99_latency_us,
    avg(data_quality_score) as avg_quality_score
    
FROM enhanced_market_data
GROUP BY symbol, time_1min;

-- 創建1分鐘聚合表
CREATE TABLE IF NOT EXISTS market_data_1min (
    symbol LowCardinality(String),
    time_1min DateTime,
    avg_price Float64,
    min_bid Float64,
    max_ask Float64,
    avg_spread_bps Float64,
    avg_obi_l5 Float64,
    avg_total_volume Float64,
    message_count UInt64,
    avg_latency_us Float64,
    p99_latency_us UInt64,
    avg_quality_score Float64
)
ENGINE = MergeTree()
PARTITION BY toDate(time_1min)
ORDER BY (symbol, time_1min)
TTL time_1min + INTERVAL 365 DAY;

-- 5. 多商品統計儀表板視圖
CREATE VIEW IF NOT EXISTS multi_symbol_dashboard AS
SELECT
    symbol,
    count() as total_messages,
    avg(mid_price) as avg_price,
    avg(spread_bps) as avg_spread_bps,
    avg(obi_l5) as avg_obi_l5,
    avg(processing_latency_us) as avg_latency_us,
    quantile(0.99)(processing_latency_us) as p99_latency_us,
    avg(data_quality_score) as avg_quality,
    
    -- 時間範圍
    min(toDateTime(timestamp / 1000000)) as first_timestamp,
    max(toDateTime(timestamp / 1000000)) as last_timestamp,
    
    -- 數據頻率
    count() / (max(timestamp) - min(timestamp)) * 1000000 as msg_per_second
    
FROM enhanced_market_data
WHERE timestamp > (toUnixTimestamp(now()) - 3600) * 1000000  -- 最近1小時
GROUP BY symbol
ORDER BY total_messages DESC;

-- 6. 性能監控查詢
CREATE VIEW IF NOT EXISTS performance_monitor AS
SELECT
    symbol,
    toStartOfMinute(toDateTime(timestamp / 1000000)) as minute,
    count() as messages_per_minute,
    avg(processing_latency_us) as avg_latency,
    quantile(0.95)(processing_latency_us) as p95_latency,
    quantile(0.99)(processing_latency_us) as p99_latency,
    max(processing_latency_us) as max_latency,
    avg(data_quality_score) as quality_score
FROM enhanced_market_data
WHERE timestamp > (toUnixTimestamp(now()) - 3600) * 1000000
GROUP BY symbol, minute
ORDER BY symbol, minute DESC;

-- 插入示例索引
CREATE INDEX IF NOT EXISTS idx_symbol_timestamp ON enhanced_market_data (symbol, timestamp) TYPE minmax GRANULARITY 1;
CREATE INDEX IF NOT EXISTS idx_data_type ON enhanced_market_data (data_type) TYPE set(100) GRANULARITY 1;

-- 7. 數據清理和維護
-- 設置TTL策略自動刪除舊數據
ALTER TABLE enhanced_market_data MODIFY TTL toDateTime(timestamp / 1000000) + INTERVAL 30 DAY;
ALTER TABLE trade_executions MODIFY TTL toDateTime(timestamp / 1000000) + INTERVAL 7 DAY;

-- 8. 創建用戶和權限
-- CREATE USER IF NOT EXISTS 'hft_collector' IDENTIFIED BY 'hft_password123';
-- GRANT INSERT, SELECT ON hft_db.* TO 'hft_collector';

-- 完成提示
SELECT 'Multi-Symbol ClickHouse tables created successfully!' as status;

-- 校驗與監控看板查詢

-- 1) 每分鐘 gap 統計（含原因分佈）
CREATE VIEW IF NOT EXISTS gap_stats_1min AS
SELECT
  venue,
  symbol,
  toStartOfMinute(toDateTime(intDiv(ts, 1000000))) AS minute,
  count() AS gap_count,
  sumIf(1, reason = 'checksum_fail') AS checksum_fail,
  sumIf(1, reason = 'seq_gap') AS seq_gap,
  anyLast(detail) AS sample_detail
FROM gap_log
GROUP BY venue, symbol, minute
ORDER BY minute DESC;

-- 2) Binance 每分鐘事件數
CREATE VIEW IF NOT EXISTS binance_events_1min AS
SELECT
  symbol,
  toStartOfMinute(toDateTime(intDiv(event_ts, 1000))) AS minute,
  count() AS msg_count,
  quantile(0.99)(u - U) AS p99_update_span
FROM raw_depth_binance
GROUP BY symbol, minute
ORDER BY minute DESC;

-- 3) Bitget 每分鐘事件數
CREATE VIEW IF NOT EXISTS bitget_events_1min AS
SELECT
  symbol,
  toStartOfMinute(toDateTime(intDiv(event_ts, 1000))) AS minute,
  count() AS msg_count
FROM raw_books_bitget
GROUP BY symbol, minute
ORDER BY minute DESC;
