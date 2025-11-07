# Data Model: Core Architecture Fix

**Feature**: 004-core-architecture-fix
**Date**: 2025-10-09
**Status**: Design Phase

## Overview

本文檔定義核心架構重構後的主要數據結構。重構的核心目標是將 God Object Engine 拆分為職責清晰的模組，實現零成本抽象，並建立完整的性能測量體系。

## Core Entities

### 1. WsFrameMetrics (hft-integration)

**Purpose**: 追蹤 WebSocket frame 接收與解析時間戳

**Fields**:
```rust
pub struct WsFrameMetrics {
    /// epoll wake 時間（微秒）
    pub received_at_us: u64,

    /// JSON 解析完成時間（微秒）
    pub parsed_at_us: u64,
}
```

**Validation Rules**:
- `parsed_at_us` >= `received_at_us`
- 時間戳使用單調時鐘 (`CLOCK_MONOTONIC`)，避免時鐘回跳問題

**State Transitions**: N/A (immutable after creation)

**Relationships**:
- 被 `LatencyTracker` 消費用於延遲分析
- 發送到 Prometheus 用於 `/metrics` 導出

---

### 2. LatencyStage (hft-core)

**Purpose**: 定義熱路徑各階段枚舉，用於延遲追蹤

**Definition**:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatencyStage {
    /// WebSocket frame 接收階段（新增）
    WsReceive,

    /// JSON 解析階段（新增）
    Parsing,

    /// 數據接入管線處理
    Ingestion,

    /// 訂單簿聚合計算
    Aggregation,

    /// 策略決策計算
    Strategy,

    /// 風控檢查
    Risk,

    /// 執行隊列處理
    Execution,

    /// 訂單提交到交易所（新增）
    Submission,
}
```

**Validation Rules**:
- 用於 histogram 索引，必須保持枚舉順序穩定
- 總共 8 個階段（Phase 3 完成後）

**State Transitions**: Sequential flow from WsReceive → Submission

---

### 3. IngestionPipeline (market-core/engine)

**Purpose**: 數據接入管線，處理市場事件消費與分發

**Fields**:
```rust
pub struct IngestionPipeline {
    /// 事件消費者列表（各交易所 WS 連接）
    consumers: Vec<EventConsumer>,

    /// 延遲追蹤器
    latency_tracker: LatencyTracker,
}
```

**Responsibilities**:
- 管理多個 `EventConsumer`（每個交易所一個）
- 追蹤 Ingestion 階段延遲
- 實現 backpressure 機制（當下游處理過慢時暫停接收）

**Validation Rules**:
- `consumers.len()` > 0
- 每個 consumer 必須有唯一的 venue_id

**Methods**:
- `add_consumer(&mut self, consumer: EventConsumer)`
- `tick(&mut self) -> Vec<MarketEvent>`
- `latency_stats(&self) -> LatencyStats`

---

### 4. StrategyRouter (market-core/engine)

**Purpose**: 策略路由與調度，使用零成本 enum dispatch

**Fields**:
```rust
pub struct StrategyRouter {
    /// 策略列表（enum，非 Box<dyn>）
    strategies: Vec<StrategyImpl>,

    /// Venue 映射表（策略名 → venue filter）
    venue_mapping: HashMap<String, VenueId>,
}
```

**StrategyImpl Enum**:
```rust
pub enum StrategyImpl {
    Trend(TrendStrategy),
    Arbitrage(ArbitrageStrategy),
    LobFlowGrid(LobFlowGridStrategy),
}
```

**Responsibilities**:
- 根據 venue filter 路由市場快照到對應策略
- 執行零成本 enum match dispatch（無 vtable overhead）
- 聚合多個策略的 OrderIntent 輸出

**Validation Rules**:
- `strategies.len()` > 0
- 每個 strategy 的 venue_scope 必須在 venue_mapping 中有對應項

**Performance Characteristics**:
- Match dispatch overhead < 50ns/call（從 2-5μs vtable 降低）
- 編譯時完全 inline（透過 `#[inline(always)]`）

---

### 5. RiskGate (market-core/engine)

**Purpose**: 事前風控閘門，檢查訂單意圖合規性

**Fields**:
```rust
pub struct RiskGate {
    /// 風控管理器
    manager: RiskManager,

    /// Venue 規格配置（tick size, min qty, 等）
    venue_specs: HashMap<VenueId, VenueSpec>,
}
```

**Responsibilities**:
- 驗證 OrderIntent 符合風控規則
- 檢查 tick size、min quantity、max position 等限制
- 過濾不符合要求的訂單意圖

**Validation Rules**:
- 所有 venue_specs 必須在系統初始化時載入
- Risk check 必須在 < 1μs 內完成（不阻塞熱路徑）

**Methods**:
- `review(&self, intents: Vec<OrderIntent>) -> Vec<OrderIntent>`
- `update_venue_spec(&mut self, venue: VenueId, spec: VenueSpec)`

---

### 6. ExecutionDispatcher (market-core/engine)

**Purpose**: 執行分發器，管理訂單提交與確認處理

**Fields**:
```rust
pub struct ExecutionDispatcher {
    /// 執行隊列（各 venue 獨立隊列）
    queues: EngineQueues,

    /// 訂單管理系統核心
    oms: OmsCore,
}
```

**Responsibilities**:
- 將 OrderIntent 轉換為實際訂單並提交
- 追蹤 Submission 階段延遲（新增測量點）
- 處理執行回報（fills, cancels, rejections）

**Validation Rules**:
- 每個 venue 有獨立的 submission queue
- Submit latency 必須 < 5μs（用戶空間部分）

**Methods**:
- `submit(&mut self, intents: Vec<OrderIntent>) -> Result<Vec<OrderId>>`
- `on_execution_event(&mut self, event: ExecutionEvent)`
- `submit_latency_stats(&self) -> LatencyStats`

---

### 7. Engine (market-core/engine, refactored)

**Purpose**: 核心引擎，事件循環調度器（重構後僅保留調度職責）

**Fields**:
```rust
pub struct Engine {
    /// 引擎配置
    config: EngineConfig,

    /// 數據接入管線
    ingestion: IngestionPipeline,

    /// 聚合引擎
    aggregation: AggregationEngine,

    /// 快照發佈器（ArcSwap 或 left-right）
    snapshots: SnapshotPublisher,

    /// 策略路由器
    router: StrategyRouter,

    /// 風控閘門
    risk: RiskGate,

    /// 執行分發器
    execution: ExecutionDispatcher,

    /// 投資組合狀態
    portfolio: Portfolio,

    /// 引擎統計
    stats: EngineStats,

    /// 延遲監控器
    latency_monitor: LatencyMonitor,
}
```

**Responsibilities** (重構後):
- 僅負責調度各模組的 `tick()` 方法
- 不包含任何業務邏輯
- `lib.rs` < 400 行（從 1200+ 降低）

**Validation Rules**:
- 結構體字段數 < 10（當前 10 個字段，符合要求）
- `tick()` 方法 < 200 行代碼

**Performance Characteristics**:
- 單次 tick 延遲 < 15μs（理想情況）
- L1/Trades p99 < 0.8ms（Phase 1 目標）
- L2/diff p99 < 1.5ms（Phase 2 目標）

---

## Feature Gates Configuration

**Purpose**: 編譯時互斥檢查，避免衝突特性同時啟用

**Definition**:
```toml
[features]
default = ["json-std", "snapshot-arcswap"]

# Hot-path features (mutually exclusive)
json-std = ["serde_json"]
json-simd = ["simd-json"]

# Snapshot strategies (mutually exclusive)
snapshot-arcswap = ["arc-swap"]
snapshot-left-right = ["left-right"]

# Cold-path features (composable)
clickhouse = ["clickhouse"]
redis = ["redis"]
metrics = ["metrics-exporter-prometheus"]
```

**Mutual Exclusion Rules**:
- `json-std` ⊕ `json-simd` (XOR)
- `snapshot-arcswap` ⊕ `snapshot-left-right` (XOR)

**Validation**:
- Workspace `Cargo.toml` 加入編譯時檢查：
```rust
#[cfg(all(feature = "json-std", feature = "json-simd"))]
compile_error!("Features 'json-std' and 'json-simd' are mutually exclusive");

#[cfg(all(feature = "snapshot-arcswap", feature = "snapshot-left-right"))]
compile_error!("Features 'snapshot-arcswap' and 'snapshot-left-right' are mutually exclusive");
```

---

## Trait Definitions

### MarketStream (hft-data)

**Purpose**: 統一市場數據流接口

```rust
pub trait MarketStream {
    /// 訂閱指定交易對
    fn subscribe(&mut self, symbols: Vec<InstrumentId>) -> Result<()>;

    /// 獲取下一批市場事件（非阻塞）
    fn poll_events(&mut self) -> Result<Vec<MarketEvent>>;

    /// 獲取連接狀態
    fn connection_state(&self) -> ConnectionState;
}
```

**Implementations**:
- `BitgetStream` (Phase 1)
- `BinanceStream` (Phase 2)

---

### ExecutionClient (hft-execution)

**Purpose**: 統一執行客戶端接口（Live/Sim）

```rust
pub trait ExecutionClient {
    /// 提交訂單意圖
    fn submit(&mut self, intent: OrderIntent) -> Result<OrderId>;

    /// 取消訂單
    fn cancel(&mut self, order_id: OrderId) -> Result<()>;

    /// 獲取執行事件（fills, cancels, rejections）
    fn poll_execution_events(&mut self) -> Result<Vec<ExecutionEvent>>;
}
```

**Implementations**:
- `LiveExecutionClient`: REST + 私有 WS
- `SimExecutionClient`: 內存隊列模擬

---

## Performance Targets

| Entity | Operation | Target | Measurement |
|--------|-----------|--------|-------------|
| WsFrameMetrics | timestamp recording | < 100ns | `quanta` 高精度時鐘 |
| StrategyRouter | enum dispatch | < 50ns/call | `criterion` benchmark |
| RiskGate | review() | < 1μs | `criterion` benchmark |
| ExecutionDispatcher | submit() | < 5μs | `criterion` benchmark |
| Engine | tick() | < 15μs | `criterion` benchmark |
| LatencyMonitor | metrics sync | < 1s lag | Prometheus `/metrics` |

---

## Migration Path

### Phase 1 (48h)
- 新增 `WsFrameMetrics` 結構體
- 擴展 `LatencyStage` enum (3 → 8 階段)
- 修改 `StrategyRouter` 使用 `enum StrategyImpl`

### Phase 2 (2w)
- 實現 `IngestionPipeline`、`RiskGate`、`ExecutionDispatcher` 模組
- 重構 `Engine` 結構體（拆分職責）
- 實現 `MarketStream` 和 `ExecutionClient` traits

### Phase 3 (4w)
- 完善所有 trait 實現
- 添加完整測試覆蓋
- 達成最終性能目標

---

## References

- **Spec Document**: specs/004-core-architecture-fix/spec.md
- **Implementation Plan**: specs/004-core-architecture-fix/plan.md
- **Rust Ownership Model**: https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html
- **Arc-Swap Documentation**: https://docs.rs/arc-swap/latest/arc_swap/
