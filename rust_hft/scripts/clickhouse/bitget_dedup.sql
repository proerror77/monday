-- Bitget trades 去重一次性修復腳本
-- 說明：
--  - 將 hft_db.bitget_trades 中的重複成交（按 (symbol, trade_id)）聚合為單條
--  - 使用 argMax(..., local_ts) 選取最新一條（保留最新的 local_ts 對應的欄位）
--  - 可選：將原表改名為 *_raw，並將去重表改名覆蓋原表

-- 1) 構建去重表（臨時表名）
CREATE TABLE IF NOT EXISTS hft_db.bitget_trades_dedup_tmp AS
SELECT
    argMax(exchange_ts, local_ts)  AS exchange_ts,
    max(local_ts)                  AS local_ts,
    symbol,
    trade_id,
    anyLast(side)                  AS side,
    argMax(price, local_ts)        AS price,
    argMax(qty, local_ts)          AS qty,
    anyLast(checksum)              AS checksum
FROM hft_db.bitget_trades
GROUP BY symbol, trade_id
SETTINGS allow_experimental_object_type = 1;

-- 2) 準備目標表（若不存在則創建為去重友好設置）
CREATE TABLE IF NOT EXISTS hft_db.bitget_trades_new (
    exchange_ts UInt64,
    local_ts UInt64,
    symbol String,
    trade_id String,
    side Enum8('buy' = 1, 'sell' = 2),
    price Float64,
    qty Float64,
    checksum UInt32
) ENGINE = ReplacingMergeTree(local_ts)
ORDER BY (symbol, trade_id)
SETTINGS index_granularity = 8192;

-- 3) 將去重數據灌入新表
INSERT INTO hft_db.bitget_trades_new
SELECT * FROM hft_db.bitget_trades_dedup_tmp;

-- 4) 可選：切換表名（請謹慎執行，確保沒有寫入進行中）
-- RENAME TABLE hft_db.bitget_trades TO hft_db.bitget_trades_raw,
--              hft_db.bitget_trades_new TO hft_db.bitget_trades;

-- 5) 清理臨時表（如果不再需要）
-- DROP TABLE IF EXISTS hft_db.bitget_trades_dedup_tmp;

