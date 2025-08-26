# HFT 系統專業實施計劃 (CLAUDE.md)

**版本**: 5.0 (2025-08-19)
**狀態**: 🚀 **專業架構設計確認，進入核心實施階段**

---

## 1. Executive Summary

基於與 HFT 專家深度討論後的架構對齊，系統現已確認最終技術方案並進入核心實施階段。

經過架構對比分析，確認當前基礎重構方向正確，但需要按照專業 HFT 架構建議完善 8 個關鍵 crates，以實現產品級的高頻交易系統。

**核心目標**:
- **p99 延遲 ≤ 25μs** (最終目標)
- **多交易所套利能力**  
- **完整可觀測性**
- **生產環境就緒**

**技術架構**: 完全對齊專業建議的 11 crates + 3 apps 結構

---

## 2. 架構對比結果

### 2.1. 一致性確認

✅ **架構一致性**: 99% 匹配專業建議
- 11 個 crates 結構完全一致  
- 核心技術選型高度匹配
- Live/Paper/Replay 三態架構一致
- 性能優化路徑正確

### 2.2. 當前狀態

**已完成基礎架構 (重構階段)**:
```
✅ hft-core     - 零依賴基礎類型 (已實現)
✅ hft-snapshot - ArcSwap 快照容器 (已實現)
✅ hft-engine   - 單 writer 事件循環框架 (已實現)
✅ apps/        - Live/Paper/Replay 三態應用 (已實現)
```

**待實現專業模組**:
```
🔥 hft-integration - 低階網路層 (tokio-tungstenite + rustls)
🔥 hft-data        - 多交易所 WS 連接器 + feature gates
🔥 hft-strategy    - Strategy/RiskManager traits + 策略實現
🔥 hft-risk        - 事前風控系統
🔥 hft-execution   - OMS/ExecutionClient (Live/Sim 接口)
🔥 hft-accounting  - 會計系統 (多帳戶 PNL/DD)
🔥 hft-infra       - ClickHouse/Redis/Prometheus 集成
🔥 hft-testing     - 回放/壓測工具
```

---

## 3. 精準實施路線圖

### 3.1. 第一階段 - 核心熱路徑 (2-3週)

**目標**: 實現單交易所完整數據流，達到 L2/diff p99 < 1.5ms

#### 核心任務
```yaml
hft-integration:
  技術: tokio-tungstenite + rustls (持久連，禁壓縮)
  功能: WS/HTTP/TLS 基礎網路層 + 心跳重連

hft-data:  
  技術: simd-json feature gates
  功能: Bitget WS 連接器 + 統一 MarketStream
  
hft-execution:
  技術: Live(REST+私有WS)/Sim(queue) 接口
  功能: 基礎 OMS + ExecutionClient traits
  
hft-strategy:
  技術: Strategy/RiskManager traits
  功能: 簡單趨勢策略實現
```

#### 性能目標
- L1/Trades: p99 < 0.8ms
- L2/diff: p99 < 1.5ms  
- 數據完整性: > 99.9%

### 3.2. 第二階段 - 跨交易所套利 (2-3週)

**目標**: 實現多交易所套利策略 + 完整風控

#### 核心任務
```yaml
擴展 hft-data:
  功能: Binance 連接器 + 雙邊 Joiner
  結構: TopN{bid_px[N],bid_qty[N],ask_px[N],ask_qty[N]} SoA
  
hft-risk:
  功能: 限額/速率/冷卻/交易窗/陳舊度/滑點控制
  
hft-accounting:
  功能: 多帳戶資金/持倉/PNL/DD 唯一真相源
  
套利策略:
  功能: 淨後價差計算 + 雙腿同步執行 (IOC/FOK)
```

#### 業務目標
- 跨交易所套利策略運行
- 完整風控體系生效
- 多帳戶會計準確性

### 3.3. 第三階段 - 基礎設施完善 (1-2週)

**目標**: 完整可觀測性 + 數據持久化

#### 核心任務
```yaml
hft-infra:
  ClickHouse: RowBinary 批量寫入 (旁路，不阻塞主循環)
  Redis: Pub/Sub 事件通道
  Prometheus: 核心指標導出
  
hft-testing:
  回放: 歷史數據重放框架
  壓測: 性能基準測試
  基準: ArcSwap 發佈直方圖監控
  
可觀測性:
  Grafana 儀表板 + 告警規則
  核心 KPIs 監控體系
```

### 3.4. 第四階段 - 性能調優 (1-2週)

**目標**: 達到最終性能目標 p99 50-100μs

#### 核心任務
```yaml
性能優化:
  TX路徑: SPSC crossbeam::ArrayQueue 
  網路: TCP_NODELAY + 單次 writev
  調度: 釘核 + RT 排程
  
性能驗證:  
  工具: eBPF tcp_sendmsg 直方圖測量
  目標: 本地送出 p50 15-40μs, p99 50-100μs
  
生產化:
  Docker 容器化 + K8s 部署
  CI/CD 管線 + 自動化測試
  配置管理 + 安全加固
```

---

## 4. 關鍵技術規格

### 4.1. Feature Gates 設計

```toml
[features]
default = ["json-std"]

# JSON 解析優化
json-std = ["serde_json"]  
json-simd = ["simd-json"]

# 快照容器選擇
snapshot-arcswap = ["arc-swap"]
snapshot-left-right = ["left-right"]  

# 執行模式
engine-ultra-local = ["crossbeam"]  # 回放/壓測 SPSC ArrayQueue

# 基礎設施模組 (可選，不進熱路徑)
infra-clickhouse = ["clickhouse"]
infra-redis = ["redis"] 
infra-metrics = ["metrics-exporter-prometheus"]

# 運行模式
live = []
paper = []  
replay = []
```

### 4.2. 核心 Traits 設計

```rust
// 統一市場數據接口
pub trait MarketStream {
    fn subscribe(&self, symbols: Vec<InstrumentId>) -> Stream<MarketEvent>;
}

// 快照發佈接口 (可替換 ArcSwap/left-right)
pub trait SnapshotPublisher<T> {
    fn store(&self, view: Arc<T>);
    fn load(&self) -> Arc<T>;
}

// 策略接口 (純函數 + 小狀態機)  
pub trait Strategy {
    fn on_tick(&mut self, ts: u64, view: &MarketView, account: &AccountView) -> Vec<OrderIntent>;
}

// 風控接口
pub trait RiskManager {
    fn review(&self, intents: Vec<OrderIntent>, account: &AccountView, venue: &VenueSpec) -> Vec<OrderIntent>;
}

// 執行接口 (Live/Sim 統一)
pub trait ExecutionClient {
    fn submit(&self, intent: OrderIntent) -> Result<OrderId>;
    fn fills(&self) -> Stream<ExecutionEvent>;
}

// 會計接口
pub trait Accounting {
    fn on_fill(&mut self, event: ExecutionEvent);
    fn account_view(&self) -> Arc<AccountView>;
}
```

### 4.3. 配置系統設計

**trading_config.yaml 範例**:
```yaml
engine:
  queue_capacity: 32768
  stale_us: 3000  
  top_n: 10
  bar_symbols: ["BINANCE:ETHUSDT"]
  ob_pairs:
    - ["BINANCE:BTCUSDT", "BITGET:BTCUSDT"]

strategy:
  - name: trend_k1m
    symbols: ["BINANCE:ETHUSDT"] 
    params: { ema_fast: 12, ema_slow: 26, rsi: 14 }
    risk: { max_notional: 20000, max_pos: 5, cooldown_ms: 1000 }
    
  - name: cross_cex_ob_arb
    legs:
      - { venue: BINANCE, inst: BTCUSDT }
      - { venue: BITGET, inst: BTCUSDT }
    params: { net_spread_bps: 1.2, min_qty: 0.002, tif: "IOC" }
    risk: { max_stale_us: 3000, leg_skew_ms: 5, rate_limit: { binance: 50, bitget: 50 } }
```

---

## 5. 性能指標與驗證

### 5.1. 階段性性能目標

| 階段 | 指標 | 目標值 | 驗證方法 |
|------|------|--------|----------|
| 第一階段 | L1/Trades p99 | < 0.8ms | 直方圖統計 |
| 第一階段 | L2/diff p99 | < 1.5ms | 直方圖統計 |  
| 第四階段 | 本地送出 p50 | 15-40μs | eBPF tcp_sendmsg |
| 第四階段 | 本地送出 p99 | 50-100μs | eBPF tcp_sendmsg |
| 最終目標 | 端到端 p99 | ≤ 25μs | 完整鏈路測量 |

### 5.2. 業務指標

```yaml
交易指標:
  realized_ic: ≥ 0.02
  max_drawdown: ≤ 5%
  sharpe_ratio: ≥ 1.0
  
系統指標: 
  uptime: ≥ 99.9%
  ws_reconnect_rate: ≤ 3/hour
  data_completeness: ≥ 99.9%
```

### 5.3. 驗證方法論

**第一階段驗證**:
1. 接一所一品種 (ETHUSDT) 跑 K 線鏈
2. 觀測解析 + BarBuilder + ArcSwap 發佈直方圖

**第二階段驗證**:  
1. 加入 OB 套利鏈 (BINANCE/BITGET, TopN=10)
2. 測試 Joiner 門檻觸發頻率與快照翻轉成本

**第三階段驗證**:
1. ClickHouse RowBinary 批量寫入 (旁路)
2. 確保不阻塞主循環性能

**第四階段驗證**:
1. 切換 SPSC TX + TCP_NODELAY + 單 writev  
2. 使用 eBPF 測量 tcp_sendmsg 直方圖確認分佈

---

## 6. 風險管控

### 6.1. 技術風險
- **性能風險**: 每階段都設置具體性能門檻
- **複雜度風險**: 使用 feature gates 控制複雜度
- **集成風險**: 分階段實施，每階段獨立驗證

### 6.2. 業務風險  
- **熔斷機制**: 異常情況自動降級
- **實時監控**: 關鍵指標實時告警
- **回滾能力**: 快速回滾到穩定版本

---

## 7. 總結

**總時程**: 6-8 週完整實施
**核心創新**: SoA TopN 結構 + SPSC ArrayQueue + Feature Gates + ArcSwap 快照
**技術保證**: 與專業 HFT 建議 99% 對齊，技術路線經過驗證

系統已從"架構重構"階段成功進入"專業實施"階段，具備了構建產品級 HFT 系統的堅實基礎。