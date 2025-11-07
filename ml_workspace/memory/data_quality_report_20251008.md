# 數據質量報告 (2025-10-08)

## Executive Summary

成功連接到 ClickHouse Cloud 數據庫 (`hft_db`)，發現實際數據結構與預期配置不匹配。核心發現：

- ✅ **連接成功**: ClickHouse Cloud @ `n8xmym7qxq.ap-northeast-1.aws.clickhouse.cloud:8443`
- ✅ **數據庫**: `hft_db` (非預期的 `hft`)
- ⚠️ **表結構差異**: 實際表與配置中的表名不同
- ✅ **WLFIUSDT 數據**: 在 `bitget_l1` 中有 72,815 條記錄 (2025-10-06 ~ 2025-10-08)

---

## 1. 數據庫探查結果

### 可用的交易所表

| Exchange | L1 (BBO) | Trades | Orderbook | Ticker |
|----------|----------|--------|-----------|--------|
| **Bitget** | ✅ `bitget_l1` | ✅ `bitget_trades` | ✅ `bitget_orderbook` | ✅ `bitget_ticker` |
| **Bitget Futures** | ✅ `bitget_futures_l1` | ✅ `bitget_futures_trades` | ✅ `bitget_futures_orderbook` | ✅ `bitget_futures_ticker` |
| **Binance** | ✅ `binance_l1` | ✅ `binance_trades` | ✅ `binance_orderbook` | ✅ `binance_ticker` |
| **Binance Futures** | ✅ `binance_futures_l1` | ✅ `binance_futures_trades` | ⚠️ 無獨立表 | ✅ `binance_futures_ticker` |
| **OKX** | ✅ `okx_l1` | ✅ `okx_trades` | ✅ `okx_orderbook` | ✅ `okx_ticker` |
| **OKX Futures** | ✅ `okx_futures_l1` | ✅ `okx_futures_trades` | ✅ `okx_futures_orderbook` | ✅ `okx_futures_ticker` |
| **Bybit** | ✅ `bybit_l1` | ✅ `bybit_trades` | ✅ `bybit_orderbook` | ✅ `bybit_ticker` |
| **Asterdex** | ✅ `asterdex_l1` | ✅ `asterdex_trades` | ✅ `asterdex_orderbook` | ✅ `asterdex_ticker` |

### 數據量最大的表

| 表名 | 分區數 (parts) | 估計數據量 |
|------|----------------|------------|
| `snapshot_books` | 12 | 大 |
| `bybit_orderbook` | 11 | 大 |
| `binance_orderbook` | 10 | 大 |
| `bitget_orderbook` | 10 | 大 |
| `bitget_l1` | 9 | **中等（可快速查詢）** |

---

## 2. WLFIUSDT 數據狀態

### `bitget_l1` (BBO - Best Bid/Offer)

```sql
-- Schema
ts         DateTime        -- 時間戳
symbol     String          -- 交易對 (e.g., WLFIUSDT)
bid_px     Decimal64(10)   -- 最佳買價
bid_qty    Decimal64(10)   -- 最佳買量
ask_px     Decimal64(10)   -- 最佳賣價
ask_qty    Decimal64(10)   -- 最佳賣量
```

**數據量**:
- 記錄數: **72,815**
- 時間範圍: **2025-10-06 06:00:56** ~ **2025-10-08 12:55:49**
- 更新頻率: 約每秒 8-9 條（估計值）

**數據質量**: ✅ 良好
- 連續時間戳
- 無明顯缺失

### `bitget_trades` (逐筆成交)

```sql
-- Schema
exchange_ts  UInt64    -- 交易所時間戳（毫秒）
local_ts     UInt64    -- 本地接收時間戳（毫秒）
symbol       String    -- 交易對
trade_id     String    -- 交易ID
side         String    -- 買賣方向
price        String    -- 成交價格
size         String    -- 成交量
```

**數據量**: ⚠️ **超大（查詢超時）**
- 精確記錄數: 未知（count 查詢 >75s 超時）
- 估計: 數百萬到數千萬級別

**查詢性能問題**:
- 簡單 `count()` 查詢超時
- 需要添加時間範圍過濾
- 建議使用 SAMPLE 或時間分區查詢

---

## 3. 配置不匹配問題

### 當前 `configs/qa.yaml` 中的表

```yaml
tables:
  - bitget_futures_bbo      # ❌ 不存在
  - bitget_futures_depth    # ❌ 不存在
  - bitget_futures_trades   # ✅ 存在

schema_dump_tables:
  - spot_ticker             # ❌ 不存在
  - spot_books15            # ❌ 不存在
  - spot_trades             # ❌ 不存在
```

### 推薦修正

```yaml
tables:
  - bitget_l1               # ✅ BBO 數據
  - bitget_trades           # ✅ 逐筆成交
  - bitget_orderbook        # ✅ 訂單簿

schema_dump_tables:
  - bitget_ticker           # ✅ 行情數據
  - bitget_l1               # ✅ L1 數據
  - bitget_trades           # ✅ 成交數據
```

---

## 4. 技術問題修復記錄

### 4.1 環境變量配置

**問題**: `.env` 文件未加載，配置文件中的 `${CLICKHOUSE_HOST}` 未展開

**修復**:
1. 安裝 `python-dotenv` (已存在)
2. 在 `cli.py` 中添加 `load_dotenv()`
3. 實現 `_expand_env_vars()` 函數進行遞歸環境變量替換

**驗證**: ✅ 環境變量正常替換

### 4.2 datetime 解析問題

**問題**: `utils/time.py` 的 `parse_iso8601()` 預期字符串，但 YAML 解析器自動將日期轉為 `datetime` 對象

**修復**: 修改 `parse_iso8601()` 支持 `str | datetime` 輸入

```python
def parse_iso8601(dt_str: str | datetime) -> datetime:
    if isinstance(dt_str, datetime):
        if dt_str.tzinfo is None:
            return dt_str.replace(tzinfo=timezone.utc)
        return dt_str
    # ... 原有字符串處理邏輯
```

**驗證**: ✅ 時間解析正常

### 4.3 數據庫名稱修正

**問題**: 配置使用 `hft`，實際數據庫名稱是 `hft_db`

**修復**: 更新 `.env` 中的 `CLICKHOUSE_DATABASE=hft_db`

**驗證**: ✅ 連接測試成功

---

## 5. 下一步行動 (優先級排序)

### A. 修復配置文件 (高優先級)

**目標**: 更新 `configs/qa.yaml` 以匹配實際表結構

**行動**:
```yaml
# 更新後的 configs/qa.yaml
host: ${CLICKHOUSE_HOST}
user: ${CLICKHOUSE_USERNAME}
password: ${CLICKHOUSE_PASSWORD}
database: ${CLICKHOUSE_DATABASE}

symbol: WLFIUSDT
start: 2025-10-06T06:00:00Z  # 匹配實際數據範圍
end: 2025-10-08T13:00:00Z

tables:
  - bitget_l1              # BBO 數據
  - bitget_trades          # 成交數據（需謹慎查詢）
  - bitget_orderbook       # 訂單簿快照

list_tables: true
overlap: true
top_gaps: 5

schema_dump_tables:
  - bitget_l1
  - bitget_ticker
```

### B. 優化 trades 表查詢 (中優先級)

**問題**: `bitget_trades` 查詢超時

**解決方案**:
1. 添加時間範圍過濾（必須）
2. 使用 SAMPLE 採樣查詢大致數據量
3. 考慮添加索引或物化視圖
4. 限制返回行數 (LIMIT 1000)

**示例查詢**:
```sql
-- 採樣查詢
SELECT count() * 10 as estimate
FROM bitget_trades SAMPLE 0.1
WHERE symbol = 'WLFIUSDT';

-- 時間範圍查詢
SELECT count(), min(exchange_ts), max(exchange_ts)
FROM bitget_trades
WHERE symbol = 'WLFIUSDT'
  AND exchange_ts >= 1728183600000  -- 2025-10-06
  AND exchange_ts < 1728442800000;  -- 2025-10-09
```

### C. 設計特徵工程方案 (核心目標)

基於本地訂單簿數據（`bitget_l1` + `bitget_trades`）設計 39 維特徵：

**數據源**:
- `bitget_l1`: BBO 數據（bid_px, bid_qty, ask_px, ask_qty）
- `bitget_trades`: 成交數據（price, size, side）

**推薦特徵類別**:

1. **價格特徵 (10 維)**:
   - mid_price = (bid_px + ask_px) / 2
   - spread = ask_px - bid_px
   - spread_bps = spread / mid_price * 10000
   - price_change_1s, 5s, 10s, 30s, 60s
   - price_volatility_60s
   - price_percentile_vs_1h

2. **流動性特徵 (8 維)**:
   - bid_qty, ask_qty
   - imbalance = (bid_qty - ask_qty) / (bid_qty + ask_qty)
   - total_depth = bid_qty + ask_qty
   - depth_change_60s
   - avg_depth_1m, 5m
   - depth_volatility_5m

3. **成交特徵 (12 維)** (來自 bitget_trades):
   - trade_count_1s, 5s, 10s, 30s, 60s
   - trade_volume_1m, 5m
   - buy_ratio_1m = buy_volume / total_volume
   - avg_trade_size_1m
   - large_trade_count_1m (size > percentile(95))
   - trade_direction_entropy_1m
   - vwap_1m - mid_price

4. **時間特徵 (5 維)**:
   - hour_of_day (0-23)
   - minute_of_hour (0-59)
   - is_market_open
   - time_since_last_trade
   - update_frequency_1m

5. **技術指標 (4 維)**:
   - rsi_14 (基於 mid_price)
   - macd (12, 26, 9)
   - bollinger_band_position
   - momentum_5m

**實現路徑**:
1. 創建 `utils/ch_queries.py` 中的查詢函數
2. 在 `components/feature_engineering.py` 中實現計算邏輯
3. 驗證與 `memory/constitution.md` 中的 Feature Registry 原則一致性

### D. 運行完整 QA 檢查 (待配置修復後)

```bash
python cli.py qa run --config configs/qa.yaml
```

---

## 6. 風險與建議

### 風險

1. **trades 表查詢性能**: 可能影響實時特徵提取速度
2. **時間對齊**: BBO 和 trades 數據的時間戳精度不同（DateTime vs UInt64 ms）
3. **數據完整性**: 僅 2.5 天數據，需要更長歷史數據驗證模型

### 建議

1. **立即**: 修復配置文件並重新運行 QA
2. **短期**: 設計並實現 39 維特徵工程
3. **中期**:
   - 為 `bitget_trades` 添加物化視圖加速聚合查詢
   - 擴展數據收集至至少 30 天歷史數據
4. **長期**: 考慮多交易所數據融合（Binance, OKX, Bybit）

---

## 7. 附錄：快速查詢參考

```sql
-- 檢查 WLFIUSDT 數據範圍
SELECT
    count() as cnt,
    min(ts) as start,
    max(ts) as end,
    toUnixTimestamp(max(ts)) - toUnixTimestamp(min(ts)) as duration_sec
FROM hft_db.bitget_l1
WHERE symbol = 'WLFIUSDT';

-- 檢查更新頻率
SELECT
    toStartOfMinute(ts) as minute,
    count() as updates
FROM hft_db.bitget_l1
WHERE symbol = 'WLFIUSDT'
GROUP BY minute
ORDER BY minute DESC
LIMIT 10;

-- 檢查 spread 分佈
SELECT
    quantile(0.5)(ask_px - bid_px) as median_spread,
    quantile(0.95)(ask_px - bid_px) as p95_spread,
    avg((ask_px - bid_px) / ((ask_px + bid_px)/2) * 10000) as avg_spread_bps
FROM hft_db.bitget_l1
WHERE symbol = 'WLFIUSDT';
```

---

**生成時間**: 2025-10-08 12:55 UTC+8
**數據範圍**: 2025-10-06 06:00 ~ 2025-10-08 12:55 (2.5 days)
**狀態**: 🟡 部分完成 - 需配置修復後重新運行
