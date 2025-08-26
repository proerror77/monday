# 快照管理 - 引擎內聚策略

> Engine 內部的快照發布、觸發規則與狀態機，外部不應依賴實現細節

## 快照容器選擇

### ArcSwap 模式 (預設，讀多寫少)
- **適用場景**: 多個策略/風控併發讀取相同快照
- **讀性能**: 幾乎零鎖競爭，wait-free 
- **寫性能**: 原子指標切換，老快照延遲回收
- **記憶體**: 雙快照並存期間 2x 用量

```rust
// Engine 內部實現 (不暴露)
type SnapshotHandle<T> = Arc<ArcSwap<T>>;

// 策略只讀取，不知道內部用 ArcSwap 還是 left-right
let snapshot: Arc<MarketSnapshot> = engine.get_snapshot(&symbol);
```

### left-right 模式 (極高頻寫入場景)
- **適用場景**: 單品種超高頻 L2 更新 (>1k Hz)
- **讀性能**: 兩階段鎖，短暫阻塞
- **寫性能**: 原地修改，無記憶體分配
- **記憶體**: 固定 2x 用量

## 觸發規則 (引擎內聚)

### K線聚合觸發
```
每當:
  - 時間邊界 (例: 60s K線，秒級對齊)
  - 或首筆交易 (冷啟動)
  - 或交易所重連 (邊界重置)

觸發:
  - 發布 BarClose 事件
  - 重置 OHLCV 累積器
  - 推進時間窗基線
```

### 套利機會觸發
```
每當:
  - 雙邊 OrderBook 都更新
  - 且價差 >= 門檻 (如 1 bps)
  - 且雙邊 freshness < stale_threshold_us

觸發:
  - 計算淨價差 (考慮手續費)
  - 發布 ArbitrageOpportunity 事件
  - 記錄到延遲直方圖
```

### 風控觸發
```
每當:
  - 持倉超限 (實時檢查)
  - 回撤超限 (每筆 Fill 後)
  - 延遲異常 (p99 > 2x 歷史)

觸發:
  - 發布 RiskAlert 事件  
  - 可能觸發熔斷 (engine 狀態切換)
  - 記錄到審計日誌
```

## 狀態機 (引擎私有)

```
States: [Startup, Running, Degraded, Halted]

Startup -> Running:
  - 所有配置的 symbols 都有初始快照
  - 策略初始化完成

Running -> Degraded: 
  - 網路異常但未斷線
  - 延遲超標但可恢復

Running/Degraded -> Halted:
  - 觸發風控熔斷
  - 不可恢復的錯誤

Halted -> Startup:
  - 人工介入重啟
```

## 快照欄位定義

### OrderBook 快照
```rust
struct OrderBookSnapshot {
    symbol: Symbol,
    timestamp: Timestamp,
    bids: Vec<BookLevel>,    // Top-N，按價格降序
    asks: Vec<BookLevel>,    // Top-N，按價格升序
    last_trade_price: Price,
    spread_bps: Bps,         // (ask[0] - bid[0]) / mid * 10000
    depth_imbalance: f32,    // bid_vol / (bid_vol + ask_vol)
}
```

### 聚合 Bar
```rust
struct AggregatedBar {
    symbol: Symbol,
    interval_ms: u64,
    open_time: Timestamp,
    close_time: Timestamp,
    open: Price,
    high: Price, 
    low: Price,
    close: Price,
    volume: Quantity,
    trade_count: u32,
    vwap: Price,             // 成交量加權平均價
}
```

## 效能配置

```toml
[engine.snapshot]
mode = "arc_swap"             # arc_swap | left_right
update_interval_us = 1000     # 最小更新間隔 (防抖)
max_snapshot_age_us = 5000    # 超時後觸發重載
top_n_levels = 10             # OrderBook 深度
enable_vwap = true           # 是否計算 VWAP (額外成本)
```

---

**原則**: 外部代碼通過 `engine.get_snapshot()` 獲取，不應假設內部用 ArcSwap 還是其他實現。引擎保留切換快照策略的權利。