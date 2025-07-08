-- HFT ClickHouse 初始化腳本
-- 用於存儲 Bitget 歷史深度和成交數據

-- 創建資料庫
CREATE DATABASE IF NOT EXISTS hft_db;
USE hft_db;

-- 1. LOB 深度資料表 (Level 2 Order Book)
CREATE TABLE IF NOT EXISTS lob_depth (
    timestamp DateTime64(6, 'UTC'),     -- 微秒級時間戳
    symbol String,                       -- 交易對符號
    exchange String DEFAULT 'bitget',    -- 交易所
    sequence UInt64,                     -- 序列號
    is_snapshot UInt8,                   -- 是否為快照 (0=update, 1=snapshot)
    
    -- 買盤深度 (價格從高到低)
    bid_prices Array(Float64),          -- 買盤價格陣列
    bid_quantities Array(Float64),      -- 買盤數量陣列
    
    -- 賣盤深度 (價格從低到高)  
    ask_prices Array(Float64),          -- 賣盤價格陣列
    ask_quantities Array(Float64),      -- 賣盤數量陣列
    
    -- 統計資訊
    bid_levels UInt16,                   -- 買盤檔數
    ask_levels UInt16,                   -- 賣盤檔數
    spread Float64,                      -- 買賣價差
    mid_price Float64,                   -- 中間價
    
    -- 元資料
    data_source String DEFAULT 'historical', -- 資料來源: historical/realtime
    created_at DateTime DEFAULT now()    -- 入庫時間
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)        -- 按月分片
ORDER BY (symbol, timestamp, sequence)  -- 主鍵排序
SETTINGS index_granularity = 8192;     -- 索引粒度優化

-- 2. 成交資料表 (Trade Data)
CREATE TABLE IF NOT EXISTS trade_data (
    timestamp DateTime64(6, 'UTC'),     -- 微秒級時間戳
    symbol String,                       -- 交易對符號
    exchange String DEFAULT 'bitget',    -- 交易所
    trade_id String,                     -- 成交ID
    
    -- 成交資訊
    price Float64,                       -- 成交價格
    quantity Float64,                    -- 成交數量
    side Enum8('buy' = 1, 'sell' = 2),  -- 買賣方向
    
    -- 統計資訊
    notional Float64,                    -- 成交金額 (price * quantity)
    
    -- 元資料
    data_source String DEFAULT 'historical',
    created_at DateTime DEFAULT now()
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (symbol, timestamp, trade_id)
SETTINGS index_granularity = 8192;

-- 3. 機器學習特徵表 (ML Features)
CREATE TABLE IF NOT EXISTS ml_features (
    timestamp DateTime64(6, 'UTC'),     -- 時間戳
    symbol String,                       -- 交易對
    
    -- 價格特徵
    mid_price Float64,                   -- 中間價
    weighted_mid_price Float64,          -- 加權中間價
    spread Float64,                      -- 價差
    relative_spread Float64,             -- 相對價差
    
    -- 訂單簿特徵
    order_book_imbalance Float64,        -- 訂單簿不平衡度
    bid_ask_ratio Float64,               -- 買賣比率
    depth_ratio Float64,                 -- 深度比率
    
    -- 成交量特徵
    volume_5s Float64,                   -- 5秒成交量
    volume_10s Float64,                  -- 10秒成交量
    volume_30s Float64,                  -- 30秒成交量
    
    -- 波動性特徵
    price_volatility Float64,            -- 價格波動率
    volume_volatility Float64,           -- 成交量波動率
    
    -- 趨勢特徵
    price_trend_5s Float64,              -- 5秒價格趨勢
    price_trend_10s Float64,             -- 10秒價格趨勢
    
    -- 技術指標
    rsi Float64,                         -- RSI指標
    macd Float64,                        -- MACD指標
    bollinger_upper Float64,             -- 布林帶上軌
    bollinger_lower Float64,             -- 布林帶下軌
    
    -- 預測目標
    price_change_3s Float64,             -- 3秒後價格變化
    price_change_5s Float64,             -- 5秒後價格變化
    price_change_10s Float64,            -- 10秒後價格變化
    
    created_at DateTime DEFAULT now()
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (symbol, timestamp)
SETTINGS index_granularity = 8192;

-- 4. 系統日誌表 (System Logs)
CREATE TABLE IF NOT EXISTS system_logs (
    timestamp DateTime64(6, 'UTC'),
    level Enum8('DEBUG' = 1, 'INFO' = 2, 'WARN' = 3, 'ERROR' = 4),
    component String,                    -- 組件名稱 (downloader, trainer, etc.)
    message String,                      -- 日誌訊息
    metadata String,                     -- 元資料 (JSON格式)
    
    created_at DateTime DEFAULT now()
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (timestamp, level, component)
SETTINGS index_granularity = 8192;

-- 創建對應的聚合表 (先創建表再創建視圖)
CREATE TABLE IF NOT EXISTS lob_depth_1min (
    minute DateTime,
    symbol String,
    exchange String,
    tick_count UInt64,
    avg_mid_price Float64,
    max_mid_price Float64,
    min_mid_price Float64,
    avg_spread Float64,
    total_bid_volume Float64,
    total_ask_volume Float64
) ENGINE = SummingMergeTree()
PARTITION BY toYYYYMM(minute)
ORDER BY (symbol, exchange, minute);

-- 創建物化視圖用於實時聚合 (在表創建之後)
-- 確保基礎表存在後再創建視圖
CREATE MATERIALIZED VIEW IF NOT EXISTS lob_depth_1min_mv TO lob_depth_1min AS
SELECT
    toStartOfMinute(timestamp) as minute,
    symbol,
    exchange,
    count() as tick_count,
    avg(mid_price) as avg_mid_price,
    max(mid_price) as max_mid_price,
    min(mid_price) as min_mid_price,
    avg(spread) as avg_spread,
    sum(arraySum(bid_quantities)) as total_bid_volume,
    sum(arraySum(ask_quantities)) as total_ask_volume
FROM lob_depth
WHERE data_source = 'realtime'
GROUP BY minute, symbol, exchange;

-- 用戶通過環境變量和容器配置創建，不需要在這裡定義
-- 容器會自動處理用戶創建和權限分配
-- CREATE USER IF NOT EXISTS hft_user IDENTIFIED BY 'hft_password';
-- GRANT SELECT, INSERT, ALTER, CREATE, DROP ON hft_db.* TO hft_user;

-- 創建有用的索引
-- 為常用查詢創建跳過索引 (使用 IF NOT EXISTS 避免重複創建)
ALTER TABLE lob_depth ADD INDEX IF NOT EXISTS idx_symbol_timestamp (symbol, timestamp) TYPE minmax GRANULARITY 4;
ALTER TABLE trade_data ADD INDEX IF NOT EXISTS idx_symbol_timestamp (symbol, timestamp) TYPE minmax GRANULARITY 4;
ALTER TABLE ml_features ADD INDEX IF NOT EXISTS idx_symbol_timestamp (symbol, timestamp) TYPE minmax GRANULARITY 4;

-- 顯示創建的表
SHOW TABLES FROM hft_db;

-- 顯示表結構
DESCRIBE TABLE lob_depth;
DESCRIBE TABLE trade_data;
DESCRIBE TABLE ml_features;

-- 插入一些測試數據以驗證表結構
INSERT INTO lob_depth (
    timestamp, symbol, sequence, is_snapshot,
    bid_prices, bid_quantities, ask_prices, ask_quantities,
    bid_levels, ask_levels, spread, mid_price
) VALUES (
    now64(6), 'BTCUSDT', 1, 1,
    [50000.0, 49999.0, 49998.0], [1.0, 2.0, 3.0],
    [50001.0, 50002.0, 50003.0], [1.5, 2.5, 3.5],
    3, 3, 1.0, 50000.5
);

INSERT INTO trade_data (
    timestamp, symbol, trade_id, price, quantity, side, notional
) VALUES (
    now64(6), 'BTCUSDT', 'test_trade_1', 50000.5, 0.1, 'buy', 5000.05
);

-- 驗證數據插入成功
SELECT 'LOB Depth Count:' as description, count(*) as count FROM lob_depth
UNION ALL
SELECT 'Trade Data Count:' as description, count(*) as count FROM trade_data;

-- 創建一些有用的查詢示例
-- 1. 獲取某個交易對的最新深度
-- SELECT * FROM lob_depth WHERE symbol = 'BTCUSDT' ORDER BY timestamp DESC LIMIT 1;

-- 2. 獲取某個時間範圍內的成交統計
-- SELECT symbol, count() as trade_count, sum(quantity) as total_volume FROM trade_data WHERE timestamp >= now() - INTERVAL 1 HOUR GROUP BY symbol;

-- 3. 計算訂單簿不平衡度  
-- SELECT timestamp, symbol, (arraySum(bid_quantities) - arraySum(ask_quantities)) / (arraySum(bid_quantities) + arraySum(ask_quantities)) as imbalance FROM lob_depth WHERE symbol = 'BTCUSDT' ORDER BY timestamp DESC LIMIT 10;