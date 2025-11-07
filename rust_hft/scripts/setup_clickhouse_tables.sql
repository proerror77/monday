-- ClickHouse 數據收集表結構
-- 支持 Binance 和 Bitget 的完整市場數據收集

-- 1. 原始 WebSocket 事件表（所有原始數據）
CREATE TABLE IF NOT EXISTS raw_ws_events (
    timestamp UInt64,                    -- 本地接收時間戳 (微秒)
    venue LowCardinality(String),        -- 交易所 (binance/bitget)
    channel LowCardinality(String),      -- 數據通道 (depth/trade/ticker/books)
    symbol String,                       -- 交易對符號
    payload String                       -- 原始 JSON 數據
)
ENGINE = MergeTree()
ORDER BY (venue, channel, symbol, timestamp)
TTL toDateTime(intDiv(timestamp, 1000000)) + INTERVAL 30 DAY
SETTINGS index_granularity = 8192;

-- 2. Binance 深度數據表 (結構化)
CREATE TABLE IF NOT EXISTS raw_depth_binance (
    event_ts UInt64,                     -- 交易所事件時間戳 (毫秒)
    ingest_ts UInt64,                    -- 本地接收時間戳 (微秒)
    symbol String,                       -- 交易對符號
    U UInt64,                            -- 起始更新 ID
    u UInt64,                            -- 結束更新 ID  
    pu UInt64,                           -- 前一個更新 ID
    bids_px Array(Float64),              -- 買盤價格數組
    bids_qty Array(Float64),             -- 買盤數量數組
    asks_px Array(Float64),              -- 賣盤價格數組
    asks_qty Array(Float64),             -- 賣盤數量數組
    price_scale UInt64,                  -- 價格精度倍數
    qty_scale UInt64,                    -- 數量精度倍數
    bids_px_i64 Array(Int64),            -- 買盤價格整數化
    bids_qty_i64 Array(Int64),           -- 買盤數量整數化
    asks_px_i64 Array(Int64),            -- 賣盤價格整數化
    asks_qty_i64 Array(Int64)            -- 賣盤數量整數化
) 
ENGINE = MergeTree()
ORDER BY (symbol, event_ts, u)
TTL toDateTime(intDiv(ingest_ts, 1000000)) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;

-- 3. Bitget 深度數據表 (結構化)  
CREATE TABLE IF NOT EXISTS raw_books_bitget (
    event_ts UInt64,                     -- 交易所事件時間戳 (毫秒)
    ingest_ts UInt64,                    -- 本地接收時間戳 (微秒)  
    symbol String,                       -- 交易對符號
    inst_type String,                    -- 產品類型 (SPOT/USDT-FUTURES等)
    channel String,                      -- 數據通道 (books/books5/books15等)
    action String,                       -- 動作類型 (snapshot/update)
    seq UInt64,                          -- 序列號
    checksum Int64,                      -- CRC32 校驗碼
    bids_px Array(Float64),              -- 買盤價格數組
    bids_qty Array(Float64),             -- 買盤數量數組
    asks_px Array(Float64),              -- 賣盤價格數組
    asks_qty Array(Float64),             -- 賣盤數量數組
    bids_px_s Array(String),             -- 買盤價格字串（用於CRC校驗）
    bids_qty_s Array(String),            -- 買盤數量字串（用於CRC校驗）
    asks_px_s Array(String),             -- 賣盤價格字串（用於CRC校驗）
    asks_qty_s Array(String),            -- 賣盤數量字串（用於CRC校驗）
    price_scale UInt64,                  -- 價格精度倍數
    qty_scale UInt64,                    -- 數量精度倍數
    bids_px_i64 Array(Int64),            -- 買盤價格整數化
    bids_qty_i64 Array(Int64),           -- 買盤數量整數化
    asks_px_i64 Array(Int64),            -- 賣盤價格整數化
    asks_qty_i64 Array(Int64)            -- 賣盤數量整數化
) 
ENGINE = MergeTree()
ORDER BY (symbol, event_ts, seq)
TTL toDateTime(intDiv(ingest_ts, 1000000)) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;

-- 4. Binance 交易數據表
CREATE TABLE IF NOT EXISTS raw_trades_binance (
    event_ts UInt64,                     -- 交易所事件時間戳 (毫秒)
    ingest_ts UInt64,                    -- 本地接收時間戳 (微秒)
    symbol String,                       -- 交易對符號
    trade_id UInt64,                     -- 交易 ID
    price Float64,                       -- 交易價格
    quantity Float64,                    -- 交易數量  
    is_buyer_maker Bool,                 -- 是否為買方掛單
    price_i64 Int64,                     -- 價格整數化
    qty_i64 Int64,                       -- 數量整數化
    price_scale UInt64,                  -- 價格精度倍數
    qty_scale UInt64                     -- 數量精度倍數
)
ENGINE = MergeTree()
ORDER BY (symbol, event_ts, trade_id)
TTL toDateTime(intDiv(ingest_ts, 1000000)) + INTERVAL 365 DAY
SETTINGS index_granularity = 8192;

-- 5. Bitget 交易數據表
CREATE TABLE IF NOT EXISTS raw_trades_bitget (
    event_ts UInt64,                     -- 交易所事件時間戳 (毫秒)
    ingest_ts UInt64,                    -- 本地接收時間戳 (微秒)
    symbol String,                       -- 交易對符號
    inst_type String,                    -- 產品類型
    trade_id String,                     -- 交易 ID
    price Float64,                       -- 交易價格
    quantity Float64,                    -- 交易數量
    side String,                         -- 交易方向 (buy/sell)
    price_i64 Int64,                     -- 價格整數化
    qty_i64 Int64,                       -- 數量整數化 
    price_scale UInt64,                  -- 價格精度倍數
    qty_scale UInt64                     -- 數量精度倍數
)
ENGINE = MergeTree()
ORDER BY (symbol, event_ts, trade_id)
TTL toDateTime(intDiv(ingest_ts, 1000000)) + INTERVAL 365 DAY
SETTINGS index_granularity = 8192;

-- 6. Binance Ticker 數據表
CREATE TABLE IF NOT EXISTS raw_ticker_binance (
    event_ts UInt64,                     -- 交易所事件時間戳 (毫秒)
    ingest_ts UInt64,                    -- 本地接收時間戳 (微秒)
    symbol String,                       -- 交易對符號
    price_change Float64,                -- 24小時價格變化
    price_change_percent Float64,        -- 24小時價格變化百分比
    weighted_avg_price Float64,          -- 加權平均價格
    prev_close_price Float64,            -- 前收盤價
    last_price Float64,                  -- 最新價格
    bid_price Float64,                   -- 最佳買價
    ask_price Float64,                   -- 最佳賣價
    open_price Float64,                  -- 開盤價
    high_price Float64,                  -- 最高價
    low_price Float64,                   -- 最低價
    volume Float64,                      -- 成交量
    quote_volume Float64,                -- 成交額
    open_time UInt64,                    -- 開盤時間
    close_time UInt64,                   -- 收盤時間
    count UInt64                         -- 成交筆數
)
ENGINE = MergeTree()
ORDER BY (symbol, event_ts)
TTL toDateTime(intDiv(ingest_ts, 1000000)) + INTERVAL 180 DAY
SETTINGS index_granularity = 8192;

-- 7. Bitget Ticker 數據表
CREATE TABLE IF NOT EXISTS raw_ticker_bitget (
    event_ts UInt64,                     -- 交易所事件時間戳 (毫秒)
    ingest_ts UInt64,                    -- 本地接收時間戳 (微秒)
    symbol String,                       -- 交易對符號
    inst_type String,                    -- 產品類型
    last_price Float64,                  -- 最新價格
    bid_price Float64,                   -- 最佳買價
    ask_price Float64,                   -- 最佳賣價
    open_24h Float64,                    -- 24小時開盤價
    high_24h Float64,                    -- 24小時最高價
    low_24h Float64,                     -- 24小時最低價
    change_24h Float64,                  -- 24小時價格變化
    change_percent_24h Float64,          -- 24小時價格變化百分比
    base_volume Float64,                 -- 基礎資產成交量
    quote_volume Float64,                -- 計價資產成交量
    usd_volume Float64                   -- USD 成交量
)
ENGINE = MergeTree()
ORDER BY (symbol, event_ts)
TTL toDateTime(intDiv(ingest_ts, 1000000)) + INTERVAL 180 DAY
SETTINGS index_granularity = 8192;

-- 8. 快照表（用於重建訂單簿）
CREATE TABLE IF NOT EXISTS snapshot_books (
    ts UInt64,                           -- 快照時間戳 (微秒)
    symbol String,                       -- 交易對符號
    venue String,                        -- 交易所
    last_id UInt64,                      -- 最後更新 ID
    bids_px Array(Float64),              -- 買盤價格數組
    bids_qty Array(Float64),             -- 買盤數量數組
    asks_px Array(Float64),              -- 賣盤價格數組
    asks_qty Array(Float64),             -- 賣盤數量數組
    source String                        -- 數據來源 (REST/WS/ANCHOR)
) 
ENGINE = MergeTree()
ORDER BY (symbol, ts)
TTL toDateTime(intDiv(ts, 1000000)) + INTERVAL 30 DAY
SETTINGS index_granularity = 8192;

-- 9. 間隙日誌表（用於數據質量監控）
CREATE TABLE IF NOT EXISTS gap_log (
    ts UInt64,                           -- 事件時間戳 (微秒)
    venue String,                        -- 交易所
    symbol String,                       -- 交易對符號
    reason String,                       -- 間隙原因
    detail String                        -- 詳細信息
) 
ENGINE = MergeTree()
ORDER BY (symbol, ts)
TTL toDateTime(intDiv(ts, 1000000)) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;

-- 10. 統計視圖：每日數據量統計
CREATE TABLE IF NOT EXISTS daily_stats (
    date Date,                           -- 統計日期
    venue String,                        -- 交易所
    symbol String,                       -- 交易對符號
    channel String,                      -- 數據通道
    event_count UInt64,                  -- 事件總數
    data_size_mb Float64,                -- 數據大小 (MB)
    first_ts UInt64,                     -- 首個事件時間戳
    last_ts UInt64,                      -- 最後事件時間戳
    completeness_pct Float64             -- 數據完整性百分比
)
ENGINE = MergeTree()
ORDER BY (date, venue, symbol, channel)
TTL date + INTERVAL 1 YEAR
SETTINGS index_granularity = 8192;

-- 建立索引以提升查詢性能
-- ALTER TABLE raw_ws_events ADD INDEX idx_timestamp (timestamp) TYPE minmax GRANULARITY 1;
-- ALTER TABLE raw_depth_binance ADD INDEX idx_event_ts (event_ts) TYPE minmax GRANULARITY 1;
-- ALTER TABLE raw_books_bitget ADD INDEX idx_event_ts (event_ts) TYPE minmax GRANULARITY 1;