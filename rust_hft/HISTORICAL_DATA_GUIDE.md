# 📊 HFT 歷史資料系統使用指南

## 🎯 系統概述

本系統實現了完整的歷史資料管理和訓練流程：

```
下載歷史資料 → ClickHouse 存儲 → 預訓練模型 → 實時微調
     ↓              ↓              ↓          ↓
   ZIP 解析      高性能時序庫    歷史資料學習   線上學習
```

## 🚀 快速開始

### 1. 環境準備

```bash
# 啟動 ClickHouse 資料庫
docker-compose up -d clickhouse

# 檢查服務狀態
curl http://localhost:8123/ping
```

### 2. 下載並導入歷史資料

#### 🎯 方法 A: 使用 YAML 配置文件 (推薦)

```bash
# 1. 創建配置文件
cargo run --example download_historical_lob -- --create-config my_config.yaml

# 2. 查看可用的配置 profiles
cargo run --example download_historical_lob -- --list-profiles

# 3. 使用預定義的 profile
cargo run --example download_historical_lob -- --profile production

# 4. 使用自定義配置文件
cargo run --example download_historical_lob -- \
  --config my_config.yaml \
  --profile development

# 5. 命令行參數覆蓋配置
cargo run --example download_historical_lob -- \
  --profile test \
  --symbol SOLUSDT \
  --clickhouse-url http://localhost:8123
```

#### 💼 方法 B: 傳統命令行參數

```bash
# 下載 SOLUSDT 深度資料並直接導入 ClickHouse
cargo run --example download_historical_lob -- \
  --symbol SOLUSDT \
  --start-date 2024-07-01 \
  --end-date 2024-07-07 \
  --data-type depth \
  --clickhouse-url http://localhost:8123 \
  --batch-size 5000 \
  --max-concurrent 5

# 或者僅下載到文件（不導入資料庫）
cargo run --example download_historical_lob -- \
  --symbol SOLUSDT \
  --start-date 2024-07-01 \
  --end-date 2024-07-07 \
  --data-type depth \
  --output-dir ./historical_data
```

### 3. 手動導入已下載的資料（可選）

```bash
# 如果之前只下載了文件，可以手動導入
cargo run --example import_historical_data -- \
  --input-dir ./historical_data \
  --symbol SOLUSDT \
  --clickhouse-url http://localhost:8123
```

### 4. 歷史資料訓練

```bash
# 執行預訓練
cargo run --example train_with_historical -- \
  --symbol SOLUSDT \
  --historical-days 7 \
  --pretrain-epochs 20 \
  --pretrain-batch-size 512 \
  --feature-dim 64 \
  --hidden-dim 128 \
  --model-save-path ./models
```

### 5. 一鍵測試

```bash
# 執行完整工作流程測試
./test_complete_workflow.sh

# YAML 配置示例演示
./examples_with_config.sh

# 自定義參數測試
./test_complete_workflow.sh \
  --symbol BTCUSDT \
  --start-date 2024-06-01 \
  --end-date 2024-06-03
```

## 🎛️ YAML 配置文件詳解

### 配置文件結構
```yaml
# 預設配置
default:
  symbol: "SOLUSDT"
  data_type: "depth"
  date_range:
    start_date: "2024-07-01"
    end_date: "2024-07-03"
  performance:
    max_concurrent: 5
    batch_size: 5000
  clickhouse:
    enabled: true
    url: "http://localhost:8123"

# 開發環境 (繼承 default)
development:
  extends: default
  symbol: "BTCUSDT"
  performance:
    max_concurrent: 2
    batch_size: 1000

# 生產環境 (多交易對)
production:
  extends: default
  symbols: ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
  data_type: "all"
  date_range:
    days_back: 30
```

### 預定義 Profiles

| Profile | 描述 | 用途 |
|---------|------|------|
| `default` | 基礎配置 | 一般用途 |
| `development` | 開發測試 | 小量數據測試 |
| `production` | 生產環境 | 多交易對批量下載 |
| `test` | 快速測試 | CI/CD 測試 |
| `backtest` | 回測數據 | 歷史回測準備 |
| `quick_test` | 極速測試 | 最小數據集驗證 |
| `bulk_download` | 大批量下載 | 10+ 交易對數據 |

### 配置優先級
1. 命令行參數 (最高優先級)
2. 指定的 profile 配置
3. 基礎配置 (extends)
4. 默認值 (最低優先級)

### 日期配置選項
```yaml
date_range:
  # 方式 1: 絕對日期
  start_date: "2024-07-01"
  end_date: "2024-07-31"
  
  # 方式 2: 相對日期
  days_back: 30  # 最近 30 天
```

## 📁 系統架構

### 核心組件

```
src/database/
├── clickhouse_client.rs    # ClickHouse 客戶端
└── mod.rs                  # 模塊導出

examples/
├── download_historical_lob.rs   # 歷史資料下載器
├── import_historical_data.rs    # 資料導入工具
└── train_with_historical.rs     # 歷史預訓練系統

clickhouse/
├── init.sql               # 資料庫初始化腳本
└── data/                  # 資料存儲目錄
```

### 資料庫表結構

#### 1. LOB 深度表 (`lob_depth`)
```sql
CREATE TABLE lob_depth (
    timestamp DateTime64(6, 'UTC'),     -- 微秒級時間戳
    symbol String,                      -- 交易對
    sequence UInt64,                    -- 序列號
    bid_prices Array(Float64),          -- 買盤價格
    bid_quantities Array(Float64),      -- 買盤數量
    ask_prices Array(Float64),          -- 賣盤價格
    ask_quantities Array(Float64),      -- 賣盤數量
    mid_price Float64,                  -- 中間價
    spread Float64,                     -- 價差
    -- ... 更多欄位
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (symbol, timestamp, sequence);
```

#### 2. 成交表 (`trade_data`)
```sql
CREATE TABLE trade_data (
    timestamp DateTime64(6, 'UTC'),
    symbol String,
    trade_id String,
    price Float64,
    quantity Float64,
    side Enum8('buy' = 1, 'sell' = 2),
    notional Float64,
    -- ... 更多欄位
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (symbol, timestamp, trade_id);
```

#### 3. 機器學習特徵表 (`ml_features`)
包含各種技術指標和預測目標。

## 🔧 配置說明

### ClickHouse 配置
```yaml
# docker-compose.yml
services:
  clickhouse:
    image: clickhouse/clickhouse-server:latest
    ports:
      - "8123:8123"  # HTTP 接口
      - "9000:9000"  # Native 接口
    environment:
      CLICKHOUSE_DB: hft_db
      CLICKHOUSE_USER: hft_user
      CLICKHOUSE_PASSWORD: hft_password
```

### 下載器配置
- **並行下載**: `--max-concurrent 5` (建議 3-8)
- **重試次數**: `--max-retries 3`
- **批量大小**: `--batch-size 10000` (建議 5K-20K)
- **資料驗證**: `--validate-data true`

### 訓練配置
- **特徵維度**: `--feature-dim 64` (建議 32-128)
- **隱藏層**: `--hidden-dim 128` (建議 64-256)
- **學習率**: `--pretrain-lr 0.001` (建議 0.0001-0.01)
- **批次大小**: `--pretrain-batch-size 512` (建議 256-1024)

## 📈 效能優化

### 1. 下載優化
```bash
# 使用較大的並行數
--max-concurrent 8

# 啟用壓縮清理
--cleanup-zip true

# 跳過已存在文件
--skip-existing true
```

### 2. 導入優化
```bash
# 較大的批次大小
--batch-size 20000

# 增加並行處理
--max-concurrent 6

# 關閉資料驗證（如果資料可信）
--validate-data false
```

### 3. 訓練優化
```bash
# 使用 GPU（如果可用）
export CUDA_VISIBLE_DEVICES=0

# 較大的批次大小
--pretrain-batch-size 1024

# 啟用早停
--early-stopping true
```

## 🔍 監控和診斷

### 資料庫查詢
```sql
-- 查看資料量
SELECT 
    table,
    count() as rows,
    formatReadableSize(sum(bytes_on_disk)) as size
FROM system.parts 
WHERE database = 'hft_db' 
GROUP BY table;

-- 查看特定交易對的資料
SELECT 
    symbol,
    count() as total_records,
    min(timestamp) as earliest,
    max(timestamp) as latest
FROM lob_depth 
GROUP BY symbol;

-- 分析資料品質
SELECT 
    symbol,
    avg(spread) as avg_spread,
    avg(mid_price) as avg_mid_price,
    count() as record_count
FROM lob_depth 
WHERE timestamp >= now() - INTERVAL 1 DAY
GROUP BY symbol;
```

### 日誌分析
```bash
# 查看下載日誌
RUST_LOG=info cargo run --example download_historical_lob -- --help

# 查看導入日誌
RUST_LOG=debug cargo run --example import_historical_data -- --help

# 查看訓練日誌
RUST_LOG=info cargo run --example train_with_historical -- --help
```

## ⚠️ 注意事項

### 1. 資料來源限制
- Bitget 歷史資料下載**需要有效的資料日期**
- 某些日期可能沒有資料（週末、節假日）
- 建議先小範圍測試再大批量下載

### 2. 存儲空間
- 深度資料約 **1-2GB/天/交易對**
- 成交資料約 **500MB-1GB/天/交易對**
- 建議預留充足的磁盤空間

### 3. 記憶體使用
- 訓練時會載入大量歷史資料到記憶體
- 建議至少 **8GB RAM**
- 可調整 `batch_size` 控制記憶體使用

### 4. 網絡穩定性
- 歷史資料下載需要穩定的網絡連接
- 建議在網絡穩定時進行大批量下載
- 支援斷點續傳和重試機制

## 🚨 故障排除

### 常見問題

#### 1. ClickHouse 連接失敗
```bash
# 檢查服務狀態
docker-compose ps

# 重啟服務
docker-compose restart clickhouse

# 檢查日誌
docker-compose logs clickhouse
```

#### 2. 下載失敗 (404)
```bash
# 檢查日期範圍
# Bitget 可能沒有某些日期的資料

# 嘗試不同的交易對
--symbol BTCUSDT

# 嘗試不同的日期
--start-date 2024-01-01 --end-date 2024-01-03
```

#### 3. 記憶體不足
```bash
# 減少批次大小
--batch-size 1000
--pretrain-batch-size 128

# 減少特徵維度
--feature-dim 32
--hidden-dim 64
```

#### 4. 編譯錯誤
```bash
# 更新依賴
cargo update

# 清理重建
cargo clean && cargo build --examples

# 檢查 Rust 版本
rustc --version  # 建議 1.70+
```

## 📚 進階用法

### 1. 多交易對批量處理
```bash
#!/bin/bash
symbols=("BTCUSDT" "ETHUSDT" "SOLUSDT" "ADAUSDT")

for symbol in "${symbols[@]}"; do
    echo "Processing $symbol..."
    
    # 下載
    cargo run --example download_historical_lob -- \
        --symbol "$symbol" \
        --start-date 2024-07-01 \
        --end-date 2024-07-07 \
        --data-type depth
    
    # 導入
    cargo run --example import_historical_data -- \
        --input-dir ./historical_data \
        --symbol "$symbol"
    
    # 訓練
    cargo run --example train_with_historical -- \
        --symbol "$symbol" \
        --historical-days 7
done
```

### 2. 定時資料更新
```bash
# crontab 設定 - 每日凌晨 2 點下載前一天資料
0 2 * * * /path/to/update_historical_data.sh
```

### 3. 資料品質檢查
```sql
-- 檢查資料完整性
SELECT 
    symbol,
    toDate(timestamp) as date,
    count() as records_per_day,
    avg(array_length(bid_prices)) as avg_bid_levels,
    avg(array_length(ask_prices)) as avg_ask_levels
FROM lob_depth 
GROUP BY symbol, date
ORDER BY symbol, date;

-- 檢查異常資料
SELECT *
FROM lob_depth 
WHERE spread < 0 OR spread > mid_price * 0.1
LIMIT 10;
```

## 🎉 總結

本系統提供了完整的歷史資料管理解決方案，支援：

✅ **自動化下載** - 批量下載 Bitget 歷史資料  
✅ **高效存儲** - ClickHouse 時序資料庫  
✅ **智能訓練** - 歷史預訓練 + 實時微調  
✅ **性能監控** - 完整的監控和診斷工具  
✅ **容錯設計** - 重試、恢復、驗證機制  

開始使用：
```bash
./test_complete_workflow.sh
```

完整文檔請參考：`examples/` 目錄中的各個範例程式。