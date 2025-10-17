# Feature Specification: Core Architecture Fix & Performance Measurement

**Feature Branch**: `[004-core-architecture-fix]`
**Created**: 2025-10-09
**Status**: Draft
**Priority**: P0 (Critical - Blocking 25μs latency target)
**Input**: Linus-style architecture deep analysis results: "修復測量系統、消除熱路徑 vtable overhead、建立零成本抽象基礎、拆分 God Object Engine"

## Executive Summary

當前系統存在三大核心問題阻礙達成 25μs p99 延遲目標：

1. **測量系統缺失/不準確**：WS frame arrival 未追蹤、metrics 導出有 1000-tick lag (10秒延遲)、ExecutionWorker 延遲未測量
2. **熱路徑性能損耗**：Box<dyn Strategy> vtable dispatch 浪費 20% 延遲預算 (2-5μs)、Feature gates 缺乏互斥檢查
3. **架構職責混亂**：Engine 結構體是 God Object (1200+ 行)，混雜數據接入、聚合、策略、風控、執行、可觀測性所有職責

本規範定義分三階段修復方案：
- **Phase 1 (48h)**: 緊急修復測量與熱路徑性能
- **Phase 2 (2w)**: 中期重構與缺失 crates 實現
- **Phase 3 (4w)**: 長期架構優化與驗證

---

## User Scenarios & Testing *(mandatory)*

### User Story 1 - 準確的端到端延遲測量 (Priority: P0)

作為性能工程師，我需要準確測量從 WS frame arrival 到 order submit 的完整延遲鏈路，以便識別真正的瓶頸並驗證優化效果。

**Why this priority**: 沒有準確測量就無法優化，這是所有性能工作的基礎。

**Independent Test**:
1. 使用 eBPF `tcp_sendmsg` 測量實際網路發送延遲
2. 對比 `timestamp_received_us` → `timestamp_submitted_us` 與 eBPF 數據
3. 驗證誤差 < 1μs

**Acceptance Scenarios**:
1. Given WS frame 到達，When 記錄 `timestamp_received_us`，Then 與系統 epoll wake 時間誤差 < 100ns
2. Given Engine tick 執行，When 查詢各階段延遲，Then Prometheus `/metrics` 立即反映（無 1000-tick lag）
3. Given ExecutionWorker 提交訂單，When 記錄提交延遲，Then 可在 Grafana 查看完整鏈路

---

### User Story 2 - 零成本策略抽象 (Priority: P0)

作為策略開發者，我需要在不犧牲性能的前提下實現可測試的策略抽象，避免 vtable dispatch 開銷。

**Why this priority**: 當前 Box<dyn Strategy> 浪費 20% 延遲預算，直接影響 25μs 目標達成。

**Independent Test**:
1. Benchmark 對比 `Box<dyn Strategy>` vs `enum StrategyImpl` dispatch
2. 驗證 enum 版本 p99 延遲降低 ≥ 2μs
3. 使用 `cargo asm` 驗證 match 分支被完全 inline

**Acceptance Scenarios**:
1. Given 3 個策略註冊，When 處理 Books15 snapshot (6-10/sec)，Then vtable overhead < 50ns/sec
2. Given 秒級 K 線場景，When 1080 strategy calls/min，Then 總 dispatch 開銷 < 500ns/min
3. Given 新策略添加，When 修改 `StrategyImpl` enum，Then 編譯時捕獲所有遺漏的 match 分支

---

### User Story 3 - 清晰的職責邊界 (Priority: P1)

作為架構師，我需要 Engine 模組只負責事件循環調度，數據接入、聚合、策略、風控、執行各自獨立，避免 God Object。

**Why this priority**: 職責混亂導致測試困難、優化受限、擴展風險高。

**Independent Test**:
1. 單獨測試 `IngestionPipeline` 的 backpressure 邏輯
2. 單獨測試 `StrategyRouter` 的 venue filtering
3. 驗證 `Engine::tick()` 代碼 < 200 行（只做調度）

**Acceptance Scenarios**:
1. Given `IngestionPipeline` 獨立模組，When 測試 consumer backpressure，Then 無需初始化完整 Engine
2. Given `RiskGate` 獨立模組，When 模擬風控拒絕，Then 無需真實 OMS/Portfolio
3. Given Engine 重構，When 編譯，Then `engine/lib.rs` < 400 行

---

### User Story 4 - Feature Gates 互斥保證 (Priority: P1)

作為發佈工程師，我需要編譯時保證 `json-std` 與 `json-simd` 不會同時啟用，避免未定義行為。

**Why this priority**: 當前可能出現「Schrödinger's JSON parser」問題。

**Independent Test**:
1. CI 矩陣測試所有有效 feature combinations
2. 嘗試同時啟用互斥 features，驗證編譯失敗
3. `cargo tree` 驗證依賴圖正確

**Acceptance Scenarios**:
1. Given `--features json-std,json-simd`，When `cargo build`，Then 編譯錯誤顯示 "Mutually exclusive features"
2. Given 預設 features，When 檢查鏈接庫，Then 只包含 `serde_json` 或 `simd-json` 之一
3. Given CI pipeline，When 執行 feature matrix test，Then 覆蓋所有有效組合

---

### Edge Cases

- **時鐘回跳**: 系統時間調整時，延遲計算不應出現負值 → 使用單調時鐘 (`CLOCK_MONOTONIC`)
- **快照發佈失敗**: ArcSwap store 失敗時不應 panic → 降級為 log + metrics increment
- **策略 panic**: 某策略 panic 時不應 crash 整個 Engine → 使用 `catch_unwind` 隔離
- **高頻策略調用**: 微秒級 K 線場景下，1M calls/min 仍需保持低延遲 → benchmark 驗證
- **Metrics 爆炸**: 100+ 交易對場景下，指標數量不應導致 OOM → histogram buckets 限制

---

## Requirements *(mandatory)*

### Functional Requirements

#### Phase 1: 測量與熱路徑修復 (48h)

- **FR-001**: 系統 MUST 在 WS frame 接收時記錄 `timestamp_received_us`（hft-integration 層）
- **FR-002**: 系統 MUST 每 100 tick 或有事件時同步 latency metrics 到 Prometheus（當前 1000-tick lag）
- **FR-003**: ExecutionWorker MUST 測量並導出 `submit_latency_us`（從 intent 接收到 REST/WS 發送）
- **FR-004**: Engine MUST 使用 `enum StrategyImpl` 替代 `Box<dyn Strategy>`（熱路徑零 vtable）
- **FR-005**: Workspace MUST 加入編譯時 feature 互斥檢查（json-std/json-simd, snapshot-arcswap/left-right）

#### Phase 2: 架構重構與缺失 crates (2w)

- **FR-006**: Engine MUST 拆分為 `IngestionPipeline`、`StrategyRouter`、`RiskGate`、`ExecutionDispatcher` 四個獨立模組
- **FR-007**: 系統 MUST 實現 `hft-integration` crate（低階網路層 + latency tracking）
- **FR-008**: 系統 MUST 實現 `hft-testing` crate（benchmark harness + replay fixtures）
- **FR-009**: 系統 MUST 實現 `hft-data` crate（統一 MarketStream trait + simd-json feature gate）
- **FR-010**: 系統 MUST 實現 `hft-execution` crate（Live/Sim ExecutionClient 分離）

#### Phase 3: 長期優化 (4w)

- **FR-011**: 系統 SHOULD 實現 `hft-strategy`、`hft-risk`、`hft-accounting`、`hft-infra` crates
- **FR-012**: Benchmark suite MUST 覆蓋所有熱路徑操作（ingestion, aggregation, strategy, risk, execution）
- **FR-013**: 系統 SHOULD 支持 eBPF-based network stack profiling（tcp_sendmsg histogram）

### Non-Functional Requirements

- **NFR-001**: Phase 1 完成後，L1/Trades p99 MUST < 0.8ms（可測量）
- **NFR-002**: Phase 2 完成後，L2/diff p99 MUST < 1.5ms（可測量）
- **NFR-003**: Phase 3 完成後，端到端 p99 MUST ≤ 25μs（eBPF 驗證）
- **NFR-004**: 重構期間，所有現有測試 MUST 保持通過（零回歸）
- **NFR-005**: 新增代碼 MUST 達到 80% 測試覆蓋率（cargo-tarpaulin）

### Key Entities *(include if feature involves data)*

#### 新增數據結構

```rust
// hft-integration/src/lib.rs
pub struct WsFrameMetrics {
    pub received_at_us: u64,  // epoll wake 時間
    pub parsed_at_us: u64,    // JSON 解析完成
}

// hft-core/src/latency.rs (擴展)
pub enum LatencyStage {
    WsReceive,      // 新增
    Parsing,        // 新增
    Ingestion,      // 現有
    Aggregation,    // 現有
    Strategy,       // 現有
    Risk,           // 現有
    Execution,      // 現有
    Submission,     // 新增
}

// market-core/engine/src/lib.rs (重構後)
pub struct IngestionPipeline {
    consumers: Vec<EventConsumer>,
    latency_tracker: LatencyTracker,
}

pub struct StrategyRouter {
    strategies: Vec<StrategyImpl>,  // enum, not Box<dyn>
    venue_mapping: HashMap<String, VenueId>,
}

pub struct RiskGate {
    manager: RiskManager,
    venue_specs: HashMap<VenueId, VenueSpec>,
}

pub struct ExecutionDispatcher {
    queues: EngineQueues,
    oms: OmsCore,
}

pub struct Engine {
    config: EngineConfig,
    ingestion: IngestionPipeline,
    aggregation: AggregationEngine,
    snapshots: SnapshotPublisher,
    router: StrategyRouter,
    risk: RiskGate,
    execution: ExecutionDispatcher,
    portfolio: Portfolio,
    stats: EngineStats,
    latency_monitor: LatencyMonitor,
}
```

#### Feature Gates 定義

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

---

## Success Criteria *(mandatory)*

### Measurable Outcomes

#### Phase 1 Success Criteria (48h)

- **SC-001**: WS frame arrival 到 event parsing 延遲可測量，p99 < 5μs
- **SC-002**: Prometheus `/metrics` 延遲數據 lag < 1s（從 10s 降低）
- **SC-003**: Strategy dispatch 開銷 < 50ns/call（從 2-5ns vtable → 0ns inline）
- **SC-004**: Feature gates 互斥檢查生效，`cargo build --features json-std,json-simd` 失敗

#### Phase 2 Success Criteria (2w)

- **SC-005**: Engine 結構體字段數 < 10（從 27 降低），`lib.rs` < 400 行（從 1200+ 降低）
- **SC-006**: L1/Trades p99 < 0.8ms（可測量，有 benchmark 證據）
- **SC-007**: L2/diff p99 < 1.5ms（可測量，有 benchmark 證據）
- **SC-008**: `hft-integration`、`hft-testing`、`hft-data`、`hft-execution` 四個 crate 實現完成且有單元測試

#### Phase 3 Success Criteria (4w)

- **SC-009**: 端到端 p99 ≤ 25μs（eBPF tcp_sendmsg 驗證）
- **SC-010**: Benchmark suite 覆蓋所有 8 個熱路徑階段
- **SC-011**: 所有核心 crates (13個) 實現完成，測試覆蓋率 ≥ 80%
- **SC-012**: 生產環境運行 7 天無穩定性回歸

### Quality Gates

- **QG-001**: 所有 Phase 結束時，`cargo clippy -- -D warnings` 零警告
- **QG-002**: 所有 Phase 結束時，`cargo test --workspace` 全通過
- **QG-003**: 所有 Phase 結束時，benchmark 對比主分支無回歸（≤2%）
- **QG-004**: Phase 2/3 結束時，`cargo-tarpaulin` 測試覆蓋率 ≥ 80%

---

## Out of Scope

以下內容**不在**本規範範圍內，將在後續規範中處理：

- CPU pinning 與 RT scheduling 優化（需要先完成測量基礎）
- 跨交易所套利策略實現（需要先完成單交易所穩定運行）
- Grafana Tempo 分佈式 tracing 集成（需要先完成基礎 metrics）
- 完整的 48-crate 重組（需要先驗證 13-crate 架構可行性）
- 生產環境部署配置（Kubernetes/Docker）更新

---

## Dependencies & Blockers

### Dependencies

- **需要**: 現有 `hft-core`、`snapshot`、`ports` crates（已存在）
- **需要**: `arc-swap` 或 `left-right` crate（已在 workspace deps）
- **需要**: `metrics-exporter-prometheus` crate（已在 workspace deps）
- **需要**: `criterion` for benchmarking（已在 dev-deps）

### Blockers

- **無 blocking dependencies** - 可立即開始

### Risks

- **風險 1**: 重構期間可能破壞現有功能
  - **緩解**: 每個 Phase 結束時運行完整測試套件，增量驗證

- **風險 2**: 測量系統本身引入性能開銷
  - **緩解**: 使用 `#[cfg(feature = "metrics")]` 條件編譯，benchmark 驗證開銷 < 2%

- **風險 3**: enum StrategyImpl 增加新策略時需修改核心代碼
  - **緩解**: 這是 HFT 系統的正常特性（策略變更頻率低），可接受的 trade-off

---

## References

- **CLAUDE.md v5.0**: HFT 系統專業實施計劃
- **Agent_Driven_HFT_System_PRD.md**: 產品需求文件（延遲預算分解）
- **Architecture Debate Report**: Architect vs Performance 專家辯論結果
- **Linus-style Analysis**: 深度架構解剖報告（本規範的輸入）

---

## Changelog

- **2025-10-09**: Initial draft based on Linus-style architecture analysis
