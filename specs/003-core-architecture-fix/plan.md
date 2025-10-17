# Implementation Plan: Core Architecture Fix & Performance Measurement

**Branch**: `[003-core-architecture-fix]` | **Date**: 2025-10-09 | **Spec**: specs/003-core-architecture-fix/spec.md
**Input**: Linus-style architecture deep analysis results + Professional HFT architecture alignment

## Summary

分三階段修復系統核心架構問題：(1) 緊急修復測量系統與熱路徑性能（48h）；(2) 重構 Engine God Object 並實現缺失的專業 crates（2w）；(3) 完整性能優化與驗證（4w）。目標達成 p99 ≤ 25μs 延遲、零成本策略抽象、清晰職責邊界、完整可觀測性。

## Technical Context

**Language/Version**: Rust stable 1.75+ (workspace 一致)
**Primary Dependencies**:
- 熱路徑: `arc-swap` (現有), `tokio-tungstenite`, `rustls`, `simd-json` (feature gate)
- 測量: `metrics-exporter-prometheus`, `quanta` (高精度時鐘)
- 測試: `criterion`, `cargo-tarpaulin`, eBPF tools (`bpftrace`)

**Storage**:
- ClickHouse (歷史數據，RowBinary 批量寫入)
- Redis (Pub/Sub 事件通道)

**Testing**:
- Unit: `cargo test --workspace`
- Benchmark: `criterion` 基準測試套件
- Integration: 端到端延遲鏈路測試
- Performance: eBPF `tcp_sendmsg` 驗證

**Target Platform**: Linux (低延遲專用節點，CPU pinning + RT scheduling)

**Performance Goals**:
- Phase 1: L1/Trades p99 < 0.8ms, L2/diff p99 < 1.5ms
- Phase 2: 維持性能無回歸
- Phase 3: 端到端 p99 ≤ 25μs (eBPF 驗證)

**Constraints**:
- 熱路徑零 vtable dispatch
- 重構期間所有現有測試必須保持通過
- 增量實施，避免大規模停機重構

**Scale/Scope**:
- 適用於 live/paper/replay 三種應用
- 支持多交易所套利場景
- 核心 crates 從 48 個精簡為 13 個專業模組

## Constitution Check

### Phase 1 (48h) - 緊急修復
- **MUST**: WS frame arrival 時間追蹤誤差 < 100ns
- **MUST**: Prometheus metrics 同步 lag < 1s（從 10s 降低）
- **MUST**: `enum StrategyImpl` 替換 `Box<dyn Strategy>`，vtable overhead < 50ns/call
- **MUST**: Feature gates 互斥檢查編譯時生效
- **MUST**: `cargo clippy -- -D warnings` 零警告
- **MUST**: Benchmark 對比主分支無回歸（≤2%）

### Phase 2 (2w) - 架構重構
- **MUST**: Engine 結構體字段數 < 10（從 27 降低）
- **MUST**: `market-core/engine/src/lib.rs` < 400 行（從 1200+ 降低）
- **MUST**: 四個新 crates (`hft-integration`, `hft-testing`, `hft-data`, `hft-execution`) 實現完成且有單元測試
- **MUST**: L1/Trades p99 < 0.8ms, L2/diff p99 < 1.5ms（可測量，有 benchmark 證據）
- **MUST**: 所有現有測試保持通過（零回歸）
- **MUST**: `cargo-tarpaulin` 測試覆蓋率 ≥ 80%

### Phase 3 (4w) - 性能調優
- **MUST**: 端到端 p99 ≤ 25μs（eBPF `tcp_sendmsg` 驗證）
- **MUST**: Benchmark suite 覆蓋所有 8 個熱路徑階段
- **MUST**: 所有核心 crates (13個) 實現完成，測試覆蓋率 ≥ 80%
- **SHOULD**: 生產環境運行 7 天無穩定性回歸

## Project Structure

### Documentation (this feature)

```
specs/003-core-architecture-fix/
├── spec.md              # 主規範文檔（已完成）
├── plan.md              # 實施計劃（本文件）
├── tasks.md             # 具體任務清單
├── data-model.md        # 可選：重構後數據結構詳細設計
└── performance.md       # 可選：性能測量與基準報告
```

### Source Code (repository root)

#### Phase 1 新增/修改文件

```
hft-core/src/
├── latency.rs           # 擴展 LatencyStage enum（新增 WsReceive, Parsing, Submission）
└── types.rs             # 基礎類型保持不變

market-core/engine/src/
├── lib.rs               # 修改：替換 Box<dyn Strategy> 為 enum StrategyImpl
├── latency_monitor.rs   # 修改：減少 metrics sync lag（1000 tick → 100 tick）
└── execution_worker.rs  # 新增：submit_latency_us 測量

Cargo.toml               # 新增：Feature gates 互斥檢查
└── [features] section
```

#### Phase 2 新增 Crates

```
hft-integration/
├── src/
│   ├── lib.rs           # WS/HTTP/TLS 基礎網路層
│   ├── ws_helpers.rs    # tokio-tungstenite + rustls 封裝
│   ├── latency.rs       # WsFrameMetrics 結構體
│   └── heartbeat.rs     # 心跳重連邏輯
└── Cargo.toml

hft-testing/
├── src/
│   ├── lib.rs           # Benchmark harness
│   ├── replay.rs        # 回放框架
│   └── fixtures.rs      # 測試數據 fixtures
└── Cargo.toml

hft-data/
├── src/
│   ├── lib.rs           # 統一 MarketStream trait
│   ├── bitget.rs        # Bitget 連接器
│   └── parser.rs        # JSON 解析（simd-json feature gate）
└── Cargo.toml

hft-execution/
├── src/
│   ├── lib.rs           # ExecutionClient trait
│   ├── live.rs          # Live 實現（REST + 私有 WS）
│   └── sim.rs           # Sim 實現（queue）
└── Cargo.toml

market-core/engine/src/
├── lib.rs               # 重構：僅保留事件循環調度（< 400 行）
├── dataflow/
│   ├── ingestion.rs     # IngestionPipeline 模組
│   └── aggregation.rs   # 保持不變
├── strategy_router.rs   # StrategyRouter 模組（新）
├── risk_gate.rs         # RiskGate 模組（新）
└── execution_dispatch.rs # ExecutionDispatcher 模組（新）
```

#### Phase 3 完整 Crates 結構

```
hft-strategy/
├── src/
│   ├── lib.rs           # Strategy trait 定義
│   ├── trend.rs         # 趨勢策略實現
│   └── arbitrage.rs     # 套利策略實現
└── Cargo.toml

hft-risk/
├── src/
│   ├── lib.rs           # RiskManager trait
│   └── default.rs       # 默認風控實現
└── Cargo.toml

hft-accounting/
├── src/
│   ├── lib.rs           # Portfolio/Account 會計系統
│   └── pnl.rs           # PNL/DD 計算
└── Cargo.toml

hft-infra/
├── src/
│   ├── clickhouse.rs    # ClickHouse RowBinary 寫入
│   ├── redis.rs         # Redis Pub/Sub
│   └── prometheus.rs    # Prometheus metrics export
└── Cargo.toml

benches/
├── hotpath_latency_p99.rs  # 新增：熱路徑基準測試
└── strategy_dispatch.rs    # 新增：enum vs Box<dyn> 對比
```

**Structure Decision**:
- Phase 1 先修復現有代碼的測量與性能問題
- Phase 2 實現 4 個基礎 crates 並重構 Engine
- Phase 3 完成剩餘 crates 並建立完整 benchmark suite

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| `enum StrategyImpl` 需手動維護 match 分支 | 零成本抽象，消除 vtable overhead (2-5μs) | `Box<dyn Strategy>` 浪費 20% 延遲預算，無法接受 |
| Feature gates 互斥檢查需 `compile_error!` macro | 防止 `json-std` + `json-simd` 同時啟用導致未定義行為 | Runtime 檢查會引入性能開銷，且無法在編譯時發現問題 |
| Engine 拆分為 4 個模組 | 清晰職責邊界，單獨測試，降低複雜度 | 保持 God Object 會導致測試困難、優化受限、擴展風險高 |
| eBPF `tcp_sendmsg` 測量 | 唯一準確測量網路發送延遲的方法 | 用戶空間計時無法準確反映內核網路棧延遲 |
| 使用 `quanta` 高精度時鐘 | 納秒級時間戳精度需求 | `std::time::Instant` 精度不足（微秒級） |
| 13 crates 架構 | 專業 HFT 系統標準，職責清晰、可測試性高 | 單體結構無法支持多交易所套利、測試困難、擴展受限 |

## Implementation Phases

### Phase 1 (48h) - 緊急修復

**目標**: 修復測量系統、消除熱路徑 vtable overhead、建立基礎測量能力

**優先級**: P0（阻礙 25μs 延遲目標達成）

**交付物**:
1. WS frame arrival 時間追蹤（hft-integration 層）
2. Prometheus metrics 同步 lag 修復（100 tick）
3. ExecutionWorker submit_latency_us 測量
4. `enum StrategyImpl` 替換 `Box<dyn Strategy>`
5. Feature gates 互斥檢查（編譯時）

**驗證方法**:
- 單元測試覆蓋所有新增測量點
- Benchmark 對比確認 vtable overhead 消除（< 50ns/call）
- 嘗試 `--features json-std,json-simd` 編譯失敗

### Phase 2 (2w) - 架構重構

**目標**: 拆分 Engine God Object、實現 4 個基礎 crates

**優先級**: P0-P1

**交付物**:
1. Engine 重構為 4 個模組（IngestionPipeline, StrategyRouter, RiskGate, ExecutionDispatcher）
2. `hft-integration` crate（低階網路層）
3. `hft-testing` crate（benchmark harness + replay）
4. `hft-data` crate（MarketStream trait + simd-json feature gate）
5. `hft-execution` crate（ExecutionClient trait + Live/Sim 實現）

**驗證方法**:
- Engine `lib.rs` < 400 行
- 所有現有測試保持通過
- L1/Trades p99 < 0.8ms（benchmark 證據）
- L2/diff p99 < 1.5ms（benchmark 證據）
- 測試覆蓋率 ≥ 80%

### Phase 3 (4w) - 性能調優

**目標**: 達成最終性能目標、完整可觀測性

**優先級**: P1-P2

**交付物**:
1. 剩餘 crates 實現（hft-strategy, hft-risk, hft-accounting, hft-infra）
2. 完整 benchmark suite（8 個熱路徑階段）
3. eBPF tcp_sendmsg 驗證（端到端 p99 ≤ 25μs）
4. 生產環境穩定性驗證（7 天運行）

**驗證方法**:
- eBPF 直方圖確認 p99 ≤ 25μs
- Benchmark 覆蓋所有熱路徑
- 測試覆蓋率 ≥ 80%
- 生產環境無穩定性回歸

## Risk Mitigation

### 技術風險

| 風險 | 影響 | 緩解措施 |
|------|------|---------|
| 重構期間破壞現有功能 | High | 每個 Phase 結束時運行完整測試套件，增量驗證 |
| 測量系統引入性能開銷 | Medium | 使用 `#[cfg(feature = "metrics")]` 條件編譯，benchmark 驗證開銷 < 2% |
| enum StrategyImpl 增加新策略複雜度 | Low | HFT 系統策略變更頻率低，可接受的 trade-off |
| Feature gates 配置錯誤 | Medium | CI 矩陣測試所有有效 feature combinations |
| eBPF 工具依賴 | Low | 提供用戶空間 fallback 測量方案 |

### 業務風險

| 風險 | 影響 | 緩解措施 |
|------|------|---------|
| 延遲目標無法達成 | High | 分階段設置中間目標，及時調整方案 |
| 生產環境穩定性回歸 | High | 充分測試，灰度發布，保留快速回滾能力 |
| 開發時程延誤 | Medium | 優先實現 P0 功能，P1/P2 功能可延後 |

## Success Metrics

### Phase 1 Success Criteria
- ✅ WS frame arrival 到 event parsing 延遲可測量，p99 < 5μs
- ✅ Prometheus `/metrics` 延遲數據 lag < 1s
- ✅ Strategy dispatch 開銷 < 50ns/call
- ✅ Feature gates 互斥檢查生效

### Phase 2 Success Criteria
- ✅ Engine 結構體字段數 < 10
- ✅ `lib.rs` < 400 行
- ✅ L1/Trades p99 < 0.8ms
- ✅ L2/diff p99 < 1.5ms
- ✅ 4 個 crate 實現完成且有單元測試

### Phase 3 Success Criteria
- ✅ 端到端 p99 ≤ 25μs（eBPF 驗證）
- ✅ Benchmark suite 覆蓋所有 8 個熱路徑階段
- ✅ 所有核心 crates (13個) 實現完成
- ✅ 測試覆蓋率 ≥ 80%
- ✅ 生產環境運行 7 天無穩定性回歸

## References

- **CLAUDE.md v5.0**: HFT 系統專業實施計劃
- **Agent_Driven_HFT_System_PRD.md**: 產品需求文件（延遲預算分解）
- **Linus-style Analysis**: 深度架構解剖報告
- **Professional HFT Architecture Alignment**: 架構對比報告（99% 一致性確認）
- **Spec Document**: specs/003-core-architecture-fix/spec.md
