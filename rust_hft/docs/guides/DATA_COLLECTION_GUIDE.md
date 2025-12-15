# HFT 數據收集系統使用指南 (Pure Rust)

本指南介紹如何使用純 Rust 實現的 HFT 系統收集 Binance 和 Bitget 的前20大交易量合約標的數據，包含 ticker、orderbook (LOB)、books15 和 trades 數據。

## 📋 目錄

- [快速開始](#快速開始)
- [數據類型說明](#數據類型說明)
- [系統架構](#系統架構)
- [環境準備](#環境準備)
- [使用方法](#使用方法)
- [監控和維護](#監控和維護)
- [故障排除](#故障排除)

## 🚀 快速開始

### 1. 自動獲取熱門標的（推薦）

```bash
# 自動獲取並收集 USDT 合約前20大標的
cargo run -p hft-collector -- \
  --auto-top-symbols \
  --market usdt-futures \
  --top-limit 20 \
  --venue both

# 收集現貨前10大標的
cargo run -p hft-collector -- \
  --auto-top-symbols \
  --market spot \
  --top-limit 10 \
  --venue both \
  --channels "depth@100ms,trade"
```

### 2. 快速測試（30秒測試）

```bash
# 測試現貨前5大標的
cargo run -p hft-collector -- \
  --auto-top-symbols \
  --market spot \
  --top-limit 5 \
  --venue both \
  --channels "depth@100ms,trade" \
  --batch-size 100 \
  --flush-ms 2000
```

## 📊 數據類型說明

系統收集以下類型的市場數據：

### Binance 數據流
| 數據類型 | 通道名 | 頻率 | 說明 |
|---------|--------|------|------|
| **Orderbook** | `depth@100ms` | 100ms | L2 深度數據，最多1000檔 |
| **Trades** | `trade` | 實時 | 每筆成交記錄 |
| **Ticker** | `ticker` | 1秒 | 24小時統計數據 |

### Bitget 數據流  
| 數據類型 | 通道名 | 頻率 | 說明 |
|---------|--------|------|------|
| **Orderbook** | `books` | 10ms | 全量深度快照 |
| **Books15** | `books15` | 20ms | 前15檔深度 |
| **Books1** | `books1` | 20ms | 最優買賣價（白名單標的）|
| **Trades** | `trade` | 實時 | 每筆成交記錄 |

## 🏗️ 系統架構

```
┌─────────────────┐    ┌─────────────────┐
│   Binance API   │    │   Bitget API    │
│                 │    │                 │
│ ∙ Depth@100ms   │    │ ∙ Books (10ms)  │
│ ∙ Trades        │    │ ∙ Books15       │ 
│ ∙ Ticker        │    │ ∙ Trades        │
└─────────┬───────┘    └─────────┬───────┘
          │                      │
          ▼                      ▼
┌─────────────────────────────────────────┐
│         hft-collector 應用              │
│                                         │
│ ∙ WebSocket 連接管理                    │
│ ∙ 數據解析和標準化                       │
│ ∙ 批量寫入優化                          │
│ ∙ 連接重試和容錯                        │
└─────────────────┬───────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────┐
│            ClickHouse               │
│                                         │
│ ∙ raw_ws_events (原始數據)              │
│ ∙ raw_depth_binance (Binance深度)      │
│ ∙ raw_books_bitget (Bitget深度)        │
│ ∙ raw_trades_* (交易數據)               │
│ ∙ raw_ticker_* (Ticker數據)            │
│ ∙ snapshot_books (快照重建)             │
└─────────────────────────────────────────┘
```

## 🔧 環境準備

### 系統要求
- **操作系統**: Linux/macOS
- **Rust**: 1.70+
- **ClickHouse**: 22.3+ (可選，數據持久化)
- **內存**: 建議 2GB+
- **磁盤**: 建議 100GB+ (取決於收集時長)
- **網絡**: 穩定的互聯網連接

### 安裝依賴

1. **編譯 hft-collector**
   ```bash
   # 項目根目錄執行
   cargo build -p hft-collector --release
   ```

2. **啟動 ClickHouse（可選）**
   ```bash
   # Docker 方式
   docker run -d --name clickhouse-server \
     -p 8123:8123 -p 9000:9000 \
     --ulimit nofile=262144:262144 \
     clickhouse/clickhouse-server
   
   # 創建數據庫
   echo "CREATE DATABASE IF NOT EXISTS hft" | curl -X POST 'http://localhost:8123' -d @-
   ```

3. **初始化數據表**
   ```bash
   # 使用提供的 SQL 腳本
   clickhouse-client --multiquery < scripts/setup_clickhouse_tables.sql
   ```

## 📖 使用方法

### 方法一：自動獲取熱門標的（推薦）

```bash
# 查看所有可用參數
cargo run -p hft-collector -- --help

# 自動獲取 USDT 合約前20大標的
cargo run -p hft-collector -- \
  --auto-top-symbols \
  --market usdt-futures \
  --top-limit 20 \
  --venue both

# 自動獲取現貨前10大標的
cargo run -p hft-collector -- \
  --auto-top-symbols \
  --market spot \
  --top-limit 10 \
  --venue both
```

**核心參數說明**:
| 參數 | 說明 | 默認值 |
|------|------|--------|
| `--auto-top-symbols` | 啟用自動獲取熱門標的 | false |
| `--market` | 市場類型 (spot/usdt-futures/coin-futures) | usdt-futures |
| `--top-limit` | 每交易所獲取的標的數量 | 20 |
| `--venue` | 交易所 (binance/bitget/both) | both |
| `--channels` | 數據通道（見下表） | depth@100ms,trade |
| `--ch-url` | ClickHouse URL | http://localhost:8123 |
| `--database` | 數據庫名稱 | hft |
| `--batch-size` | 批處理大小 | 5000 |
| `--flush-ms` | 刷新間隔（毫秒）| 1000 |

### 方法二：手動指定標的

```bash
# 手動指定 Binance 標的
cargo run -p hft-collector -- \
  --venue binance \
  --symbols "BTCUSDT,ETHUSDT,SOLUSDT" \
  --channels "depth@100ms,trade,ticker" \
  --binance-market usdt-m

# 手動指定 Bitget 標的
cargo run -p hft-collector -- \
  --venue bitget \
  --symbols "BTCUSDT,ETHUSDT,SOLUSDT" \
  --channels "books,books15,trade" \
  --inst-type USDT-FUTURES
```

### 方法三：混合模式

```bash
# 同時運行多個實例收集不同市場
# 實例1: 現貨前10大
cargo run -p hft-collector -- \
  --auto-top-symbols --market spot --top-limit 10 &

# 實例2: 合約前10大
cargo run -p hft-collector -- \
  --auto-top-symbols --market usdt-futures --top-limit 10
```

## 📈 監控和維護

### 查看數據收集狀態

```bash
# 查看進程狀態
ps aux | grep hft-collector

# 查看應用日誌（應用會直接輸出到終端）
# 或者重定向到日誌文件
cargo run -p hft-collector -- \
  --auto-top-symbols \
  --market spot \
  --top-limit 5 \
  > collector.log 2>&1 &
```

### ClickHouse 數據查詢

```sql
-- 查看數據統計
SELECT 
    venue, 
    channel, 
    symbol, 
    COUNT(*) as events,
    min(timestamp) as first_event,
    max(timestamp) as last_event
FROM hft.raw_ws_events 
WHERE timestamp > now() - INTERVAL 1 HOUR
GROUP BY venue, channel, symbol
ORDER BY events DESC;

-- 查看最新的 orderbook 數據
SELECT 
    symbol,
    bids_px[1] as best_bid,
    asks_px[1] as best_ask,
    event_ts
FROM hft.raw_depth_binance 
WHERE symbol = 'BTCUSDT'
ORDER BY event_ts DESC 
LIMIT 10;

-- 查看交易數據
SELECT 
    symbol,
    COUNT(*) as trade_count,
    SUM(quantity) as total_volume
FROM hft.raw_trades_binance
WHERE event_ts > now() - INTERVAL 1 HOUR
GROUP BY symbol
ORDER BY trade_count DESC;
```

### 數據完整性檢查

```sql
-- 檢查數據間隙
SELECT 
    venue,
    symbol,
    reason,
    count(*) as gap_count
FROM hft.gap_log
WHERE ts > now() - INTERVAL 1 DAY
GROUP BY venue, symbol, reason;

-- 檢查 CRC 校驗失敗
SELECT * FROM hft.gap_log 
WHERE reason = 'checksum_fail' 
AND ts > now() - INTERVAL 1 HOUR;
```

## 🔍 故障排除

### 常見問題

1. **ClickHouse 連接失敗**
   ```bash
   # 檢查服務狀態
   curl http://localhost:8123/ping
   
   # 檢查數據庫
   echo "SHOW DATABASES" | curl -X POST 'http://localhost:8123' -d @-
   ```

2. **WebSocket 連接超時**
   - 檢查網絡連接
   - 確認交易所 API 狀態
   - 查看日誌文件中的錯誤信息

3. **數據寫入失敗**
   ```bash
   # 檢查磁盤空間
   df -h
   
   # 檢查 ClickHouse 表結構
   echo "DESCRIBE hft.raw_ws_events" | curl -X POST 'http://localhost:8123' -d @-
   ```

4. **進程異常退出**
   ```bash
   # 查看日誌
   tail -100 logs/binance_collector.log
   
   # 重新啟動
   ./scripts/start_data_collection.sh stop
   ./scripts/start_data_collection.sh start
   ```

### 性能調優

1. **批處理優化**
   ```bash
   # 增大批處理大小和刷新間隔
   BATCH_SIZE=10000 FLUSH_MS=2000 ./scripts/start_data_collection.sh start
   ```

2. **ClickHouse 調優**
   ```sql
   -- 優化表設置
   ALTER TABLE hft.raw_ws_events MODIFY SETTING index_granularity = 8192;
   
   -- 添加物化列索引
   ALTER TABLE hft.raw_ws_events ADD INDEX idx_timestamp timestamp TYPE minmax GRANULARITY 1;
   ```

3. **系統資源**
   ```bash
   # 調整文件描述符限制
   ulimit -n 65536
   
   # 監控資源使用
   htop
   ```

### 日誌分析

```bash
# 統計錯誤類型
grep -E "(ERROR|WARN)" logs/*.log | sort | uniq -c

# 查看重連情況
grep "重連" logs/*.log | tail -20

# 監控數據吞吐
grep "批量寫入" logs/*.log | tail -10
```

## 📝 數據文件說明

收集的數據將存儲在 ClickHouse 的以下表中：

- `raw_ws_events` - 所有原始 WebSocket 數據
- `raw_depth_binance` - Binance 深度數據（結構化）
- `raw_books_bitget` - Bitget 訂單簿數據（結構化）
- `raw_trades_*` - 交易數據
- `raw_ticker_*` - Ticker 數據
- `snapshot_books` - 訂單簿快照（用於重建）
- `gap_log` - 數據質量日誌

## 📞 支持

如遇到問題，請：

1. 查看日誌文件 `logs/*.log`
2. 檢查 [故障排除](#故障排除) 章節
3. 提交 Issue 到項目倉庫

---

**祝你數據收集愉快！** 🎯