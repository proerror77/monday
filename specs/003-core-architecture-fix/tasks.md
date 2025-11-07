---
description: "Tasks for Core Architecture Fix & Performance Measurement"
---

# Tasks: Core Architecture Fix & Performance Measurement

**Input**: Design documents from `/specs/003-core-architecture-fix/`
**Prerequisites**: plan.md (required), spec.md (required for user stories)

**Organization**: Tasks grouped by phase and user story for independent delivery

## Format: `[ID] [P?] [Story] Description`
- **[P]**: Can run in parallel (no cross-file deps)
- **[Story]**: US1/US2/US3/US4 (User Stories from spec.md)
- **Priority**: P0 (Critical), P1 (High), P2 (Medium)

---

## Phase 1: 緊急修復測量與熱路徑 (48h, P0)

### Setup & Infrastructure

- [ ] T001 [P] 創建 `hft-integration` crate skeleton（基礎結構）
- [ ] T002 [P] 添加 `quanta` 高精度時鐘依賴到 workspace
- [ ] T003 配置 feature gates 互斥檢查框架
- [ ] T004 [P] 設置 Phase 1 benchmark baseline（存儲主分支 p99 數據）

### FR-001: WS Frame Timestamp Tracking (US1)

- [ ] T010 [US1] 定義 `WsFrameMetrics` 結構體（received_at_us, parsed_at_us）
- [ ] T011 [US1] 在 `tokio-tungstenite` 接收層注入 `received_at_us` 追蹤
- [ ] T012 [US1] 在 JSON parser 完成後記錄 `parsed_at_us`
- [ ] T013 [US1] 單元測試：驗證時間戳誤差 < 100ns（使用 `quanta::Clock`）
- [ ] T014 [US1] 集成測試：對比 epoll wake 時間與 `received_at_us`

### FR-002: Metrics Sync Lag Fix (US1)

- [ ] T020 [US1] 修改 `LatencyMonitor::sync_interval`：1000 tick → 100 tick
- [ ] T021 [US1] 添加 `last_sync_ts` 追蹤，確保最大 lag < 1s
- [ ] T022 [US1] Prometheus exporter 使用批量 snapshot 而非增量同步
- [ ] T023 [US1] 單元測試：模擬 100 tick 驗證同步觸發
- [ ] T024 [US1] 集成測試：curl `/metrics` 驗證延遲數據實時性

### FR-003: ExecutionWorker Latency Measurement (US1)

- [ ] T030 [US1] 在 `ExecutionWorker` 添加 `submit_latency_us` 計數器
- [ ] T031 [US1] 記錄從 intent 接收到 REST/WS 發送的延遲
- [ ] T032 [US1] 導出到 Prometheus histogram（buckets: 10μs, 50μs, 100μs, 500μs）
- [ ] T033 [US1] 單元測試：模擬提交流程驗證計時準確性
- [ ] T034 [US1] 文檔更新：在 Grafana dashboard 添加提交延遲面板

### FR-004: Zero-Cost Strategy Abstraction (US2)

- [ ] T040 [US2] 定義 `enum StrategyImpl { Trend(TrendStrategy), Arbitrage(...), ... }`
- [ ] T041 [US2] 實現 `impl StrategyImpl { fn on_market_event(...) -> Vec<OrderIntent> }`（內部 match）
- [ ] T042 [US2] 替換 `Engine::strategies: Vec<Box<dyn Strategy>>` → `Vec<StrategyImpl>`
- [ ] T043 [US2] 使用 `cargo asm` 驗證 match 分支完全 inline
- [ ] T044 [US2] Benchmark：對比 `Box<dyn>` vs `enum` dispatch（目標：降低 ≥ 2μs）
- [ ] T045 [US2] 更新所有策略實現以匹配新 enum 模式
- [ ] T046 [US2] 集成測試：Books15 snapshot 場景（6-10/sec）驗證 overhead < 50ns/sec

### FR-005: Feature Gates Mutual Exclusion (US4)

- [ ] T050 [US4] 在 `Cargo.toml` 添加 `compile_error!` macro
  ```rust
  #[cfg(all(feature = "json-std", feature = "json-simd"))]
  compile_error!("Mutually exclusive features: json-std and json-simd");
  ```
- [ ] T051 [US4] 添加類似檢查：`snapshot-arcswap` vs `snapshot-left-right`
- [ ] T052 [US4] CI: 添加 feature matrix test（所有有效組合）
- [ ] T053 [US4] 手動測試：`cargo build --features json-std,json-simd` 驗證編譯失敗
- [ ] T054 [US4] `cargo tree` 驗證依賴圖正確（只包含一個 JSON parser）

### Phase 1 驗證

- [ ] T060 執行完整測試套件：`cargo test --workspace`
- [ ] T061 Benchmark 對比：確保主分支無回歸（≤2%）
- [ ] T062 `cargo clippy -- -D warnings` 零警告
- [ ] T063 性能驗證：WS receive → parsing p99 < 5μs
- [ ] T064 文檔更新：記錄 Phase 1 性能基準數據

**Checkpoint Phase 1**: 測量系統修復完成，熱路徑性能提升

---

## Phase 2: 架構重構與缺失 Crates (2w, P0-P1)

### Setup

- [ ] T100 [P] 創建 4 個新 crate skeletons（hft-integration, hft-testing, hft-data, hft-execution）
- [ ] T101 [P] 配置 workspace dependencies（tokio-tungstenite, rustls, simd-json）
- [ ] T102 設置 Phase 2 benchmark baseline

### FR-006: Engine God Object Refactoring (US3)

- [ ] T110 [US3] 提取 `IngestionPipeline` 模組
  - [ ] T111 移動 `event_consumers` 字段
  - [ ] T112 移動 `latency_tracker` 字段
  - [ ] T113 實現 `IngestionPipeline::tick()` 方法
  - [ ] T114 單元測試：backpressure 邏輯（無需完整 Engine）

- [ ] T120 [US3] 提取 `StrategyRouter` 模組
  - [ ] T121 移動 `strategies: Vec<StrategyImpl>` 字段
  - [ ] T122 移動 `venue_mapping` 字段
  - [ ] T123 實現 venue filtering 邏輯
  - [ ] T124 單元測試：路由決策（無需完整 Engine）

- [ ] T130 [US3] 提取 `RiskGate` 模組
  - [ ] T131 移動 `risk_manager` 字段
  - [ ] T132 移動 `venue_specs` 字段
  - [ ] T133 實現風控審核邏輯
  - [ ] T134 單元測試：風控拒絕（無需真實 OMS/Portfolio）

- [ ] T140 [US3] 提取 `ExecutionDispatcher` 模組
  - [ ] T141 移動 `execution_queues` 字段
  - [ ] T142 移動 `oms` 字段
  - [ ] T143 實現訂單提交邏輯
  - [ ] T144 單元測試：訂單調度（無需真實交易所）

- [ ] T150 [US3] 重構 `Engine::tick()` 為純調度邏輯
  - [ ] T151 精簡為 < 200 行（只做 ingestion → aggregation → router → risk → execution）
  - [ ] T152 確保 `Engine` struct 只保留 10 個字段
  - [ ] T153 驗證 `lib.rs` 總行數 < 400
  - [ ] T154 集成測試：端到端流程保持功能一致

### FR-007: hft-integration Crate (US1)

- [ ] T200 [P] [US1] 實現 `ws_helpers.rs`（tokio-tungstenite + rustls）
- [ ] T201 [P] [US1] 實現 `latency.rs`（WsFrameMetrics 結構體）
- [ ] T202 [P] [US1] 實現 `heartbeat.rs`（心跳重連邏輯）
- [ ] T203 [US1] 單元測試：WS 連接建立與重連
- [ ] T204 [US1] 集成測試：真實 WS 端點連接（使用 mock server）
- [ ] T205 [US1] 文檔：API 使用範例與最佳實踐

### FR-008: hft-testing Crate (US1/US2)

- [ ] T210 [P] 實現 `replay.rs`（歷史數據回放框架）
- [ ] T211 [P] 實現 `fixtures.rs`（測試數據 fixtures）
- [ ] T212 [P] 實現 benchmark harness（使用 `criterion`）
- [ ] T213 添加回放數據集（ClickHouse 導出）
- [ ] T214 單元測試：回放準確性驗證
- [ ] T215 集成測試：完整回放場景

### FR-009: hft-data Crate (US2)

- [ ] T220 [P] 定義 `MarketStream` trait（統一接口）
- [ ] T221 [P] 實現 Bitget 連接器（WS + REST）
- [ ] T222 [P] 實現 `parser.rs`（simd-json feature gate）
- [ ] T223 Feature gate 配置：`json-std` vs `json-simd`
- [ ] T224 單元測試：JSON 解析正確性
- [ ] T225 Benchmark：simd-json vs serde_json 性能對比
- [ ] T226 集成測試：真實 Bitget WS 連接

### FR-010: hft-execution Crate (US2/US3)

- [ ] T230 [P] 定義 `ExecutionClient` trait
- [ ] T231 [P] 實現 Live 版本（REST + 私有 WS）
- [ ] T232 [P] 實現 Sim 版本（queue-based）
- [ ] T233 實現 OMS 核心邏輯（訂單狀態機）
- [ ] T234 單元測試：訂單生命週期
- [ ] T235 集成測試：Live 模式真實下單（測試網）
- [ ] T236 集成測試：Sim 模式模擬執行

### Phase 2 驗證

- [ ] T250 執行完整測試套件：`cargo test --workspace`
- [ ] T251 測試覆蓋率：`cargo-tarpaulin` ≥ 80%
- [ ] T252 Benchmark 驗證：L1/Trades p99 < 0.8ms
- [ ] T253 Benchmark 驗證：L2/diff p99 < 1.5ms
- [ ] T254 `cargo clippy -- -D warnings` 零警告
- [ ] T255 文檔更新：架構圖反映新模組結構

**Checkpoint Phase 2**: Engine 重構完成，4 個基礎 crates 實現

---

## Phase 3: 性能調優與完整可觀測性 (4w, P1-P2)

### Setup

- [ ] T300 [P] 創建剩餘 crate skeletons（hft-strategy, hft-risk, hft-accounting, hft-infra）
- [ ] T301 [P] 配置 eBPF 工具鏈（bpftrace, bcc-tools）
- [ ] T302 設置 Phase 3 benchmark baseline

### FR-011: 剩餘專業 Crates

- [ ] T310 [P] **hft-strategy** crate
  - [ ] T311 定義 Strategy trait（最終版本）
  - [ ] T312 實現趨勢策略（TrendStrategy）
  - [ ] T313 實現套利策略（ArbitrageStrategy）
  - [ ] T314 單元測試：策略邏輯驗證
  - [ ] T315 集成測試：回測場景

- [ ] T320 [P] **hft-risk** crate
  - [ ] T321 定義 RiskManager trait
  - [ ] T322 實現默認風控（限額/速率/冷卻）
  - [ ] T323 實現滑點控制
  - [ ] T324 單元測試：風控規則驗證
  - [ ] T325 集成測試：風控拒絕場景

- [ ] T330 [P] **hft-accounting** crate
  - [ ] T331 實現 Portfolio 會計系統
  - [ ] T332 實現 PNL/DD 計算
  - [ ] T333 實現多帳戶支持
  - [ ] T334 單元測試：會計準確性
  - [ ] T335 集成測試：多帳戶場景

- [ ] T340 [P] **hft-infra** crate
  - [ ] T341 實現 ClickHouse RowBinary 批量寫入
  - [ ] T342 實現 Redis Pub/Sub 封裝
  - [ ] T343 實現 Prometheus metrics export
  - [ ] T344 單元測試：基礎設施連接
  - [ ] T345 集成測試：端到端數據持久化

### FR-012: Benchmark Suite (US1/US2)

- [ ] T400 [US2] 定義 8 個熱路徑階段 benchmark
  - [ ] T401 WsReceive benchmark
  - [ ] T402 Parsing benchmark
  - [ ] T403 Ingestion benchmark
  - [ ] T404 Aggregation benchmark
  - [ ] T405 Strategy benchmark
  - [ ] T406 Risk benchmark
  - [ ] T407 Execution benchmark
  - [ ] T408 Submission benchmark

- [ ] T410 [US2] 實現 ArcSwap 發佈直方圖監控
- [ ] T411 [US2] 實現端到端延遲鏈路測試
- [ ] T412 [US2] 集成測試：所有 benchmark 覆蓋
- [ ] T413 [US2] 文檔：benchmark 使用指南

### FR-013: eBPF Network Profiling (US1)

- [ ] T420 [US1] 編寫 eBPF `tcp_sendmsg` 追蹤腳本
- [ ] T421 [US1] 實現直方圖數據收集（p50, p99, p999）
- [ ] T422 [US1] 對比用戶空間計時與 eBPF 數據
- [ ] T423 [US1] 驗證端到端 p99 ≤ 25μs
- [ ] T424 [US1] 文檔：eBPF 測量方法論

### Phase 3 驗證

- [ ] T450 執行完整測試套件：`cargo test --workspace`
- [ ] T451 測試覆蓋率：`cargo-tarpaulin` ≥ 80%（所有核心 crates）
- [ ] T452 Benchmark 驗證：所有 8 個階段覆蓋
- [ ] T453 eBPF 驗證：端到端 p99 ≤ 25μs
- [ ] T454 `cargo clippy -- -D warnings` 零警告
- [ ] T455 生產環境：運行 7 天無穩定性回歸
- [ ] T456 文檔更新：完整性能報告

**Checkpoint Phase 3**: 最終性能目標達成，完整可觀測性建立

---

## Phase N: Polish & Cross-Cutting

### 文檔與可觀測性

- [ ] TX01 更新 CLAUDE.md v6.0（反映最終架構）
- [ ] TX02 創建 Grafana 儀表板（8 個熱路徑階段）
- [ ] TX03 配置 Prometheus 告警規則
- [ ] TX04 編寫運維手冊（troubleshooting guide）
- [ ] TX05 創建架構圖（反映 13 crates 結構）

### 生產化

- [ ] TX10 Docker 容器化（三個應用）
- [ ] TX11 K8s 部署配置（低延遲節點）
- [ ] TX12 CI/CD 管線更新
- [ ] TX13 配置管理（trading_config.yaml）
- [ ] TX14 安全加固（TLS, 訪問控制）

### 代碼質量

- [ ] TX20 代碼審查（所有核心 crates）
- [ ] TX21 性能審查（所有熱路徑）
- [ ] TX22 安全審查（`cargo audit`）
- [ ] TX23 依賴審查（license compliance）
- [ ] TX24 技術債務清理

---

## Dependencies & Execution Order

**Critical Path**:
```
Phase 1 Setup (T001-T004)
  ↓
Phase 1 Parallel Track:
  - US1: FR-001, FR-002, FR-003 (T010-T034)
  - US2: FR-004 (T040-T046)
  - US4: FR-005 (T050-T054)
  ↓
Phase 1 驗證 (T060-T064)
  ↓
Phase 2 Setup (T100-T102)
  ↓
Phase 2 Parallel Track:
  - US3: FR-006 (T110-T154)
  - US1: FR-007 (T200-T205)
  - US1/US2: FR-008 (T210-T215)
  - US2: FR-009 (T220-T226)
  - US2/US3: FR-010 (T230-T236)
  ↓
Phase 2 驗證 (T250-T255)
  ↓
Phase 3 Setup (T300-T302)
  ↓
Phase 3 Parallel Track:
  - FR-011: T310-T345 (4 crates)
  - US1/US2: FR-012 (T400-T413)
  - US1: FR-013 (T420-T424)
  ↓
Phase 3 驗證 (T450-T456)
  ↓
Polish & Cross-Cutting (TX01-TX24)
```

**Parallelization Strategy**:
- Tasks marked `[P]` can run in parallel (no cross-file dependencies)
- Each User Story track can be developed independently within same Phase
- Phase N tasks can start once Phase 3 core functionality is stable

**Risk Mitigation**:
- Each Phase ends with comprehensive verification checkpoint
- Rollback to previous Phase if critical issues detected
- Incremental deployment (live → paper → replay)

---

## Success Criteria Summary

### Phase 1 (48h)
- ✅ All T001-T064 completed
- ✅ WS frame → parsing p99 < 5μs
- ✅ Metrics lag < 1s
- ✅ Strategy dispatch < 50ns/call
- ✅ Feature gates mutual exclusion working

### Phase 2 (2w)
- ✅ All T100-T255 completed
- ✅ Engine < 10 fields, lib.rs < 400 lines
- ✅ 4 crates implemented with 80% test coverage
- ✅ L1/Trades p99 < 0.8ms
- ✅ L2/diff p99 < 1.5ms

### Phase 3 (4w)
- ✅ All T300-T456 completed
- ✅ 13 crates implemented with 80% test coverage
- ✅ All 8 hotpath stages benchmarked
- ✅ End-to-end p99 ≤ 25μs (eBPF verified)
- ✅ 7-day production stability

---

## Notes

- **Estimation**: Task granularity optimized for 2-4 hour work units
- **Ownership**: Each task assignable to single developer
- **Testing**: Unit tests required for all new code
- **Documentation**: API docs required for all public interfaces
- **Review**: Code review required before merging to main branch
