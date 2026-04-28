# Feature Specification: Order Book & Engine Rework

**Feature Branch**: `[002-orderbook-rework]`  
**Created**: 2025-10-08  
**Status**: Superseded / Frozen as of 2026-04-28

**Input**: Internal review discovered the current TopN order book and engine structure cannot meet latency targets; we must redesign data structures, unify numeric types, and simplify engine layering to restore determinism and maintainability.

> **Superseded by**: `docs/architecture/BINANCE_LOW_LATENCY_MARKET_DATA_PLAN.md`
>
> This specification is no longer the active implementation contract for the
> next phase. Its BTreeMap/Decimal-first design is too generic for the current
> Binance low-latency market-data objective. Use the new plan for active work:
> Binance depth stream correctness, snapshot/diff sequence bridge, fixed-point
> TopN feature path, signal expiry, replay, and remote/CI latency evidence.

## User Scenarios & Testing *(mandatory)*

### User Story 1 – 交易引擎能在高頻行情下維持穩定延遲 (Priority: P1)

作為量化團隊，我希望在 100k 更新/秒 的行情壓測下，訂單簿更新與策略觸發仍維持 p99 ≤ 10 μs，確保策略決策不被資料結構拖慢。

**Why this priority**: 直接影響成交機會；延遲超標導致滑價和撤單失敗。

**Independent Test**: 在壓測環境注入模擬行情，量測 `hft_latency_ingestion_microseconds` 與策略觸發時間，確保 p99 ≤ 10 μs。

**Acceptance Scenarios**:
1. Given 100k updates/s feed，When engine 塞入重構後的訂單簿，Then `hft_latency_ingestion_microseconds` p99 ≤ 10 μs。
2. Given 熱路徑策略觸發，When 收到最新行情，Then 策略輸出 `OrderIntent` 的 p99 ≤ 12 μs。

---

### User Story 2 – 風控仍能正確解析最新持倉與行情 (Priority: P1)

作為風控，我需要在新資料結構下仍能以 Decimal 精度取得持倉與最佳買賣價，以便判斷限額、風險和發出警示。

**Why this priority**: 風控判斷錯誤會造成超限交易或過度拒單。

**Independent Test**: 風控模組輸入含負值和極端小數的行情，驗證 `RiskManager::check_position_limits` 與 `StrategyRiskLimits` 邏輯保持正確。

**Acceptance Scenarios**:
1. Given Decimal 格式的價格/數量，When 風控更新持倉，Then 輸出不再發生 f64 精度損失。
2. Given 多個策略敞口，When 風控計算名義值與持倉限額，Then 不再需要人工轉換或特例處理。

---

### User Story 3 – 開發者能維護清晰的 Engine 分層 (Priority: P2)

作為維運工程師，我需要清楚的資料流分層（資料層/策略層/執行層），讓我可以定位效能瓶頸、替換模組或進行調試而不牽連整個 Engine。

**Why this priority**: 現行單體引擎超過 25 個欄位，跨模組變更風險極高。

**Independent Test**: code review + `cargo check --lib` 確認策略、風控、執行模組的介面僅經由指定 channel 交互；新增單元測試模擬資料流。

**Acceptance Scenarios**:
1. Given 重構後的 Engine，When 我閱讀 `DataEngine/StrategyEngine/ExecutionEngine` 模組，Then 無需追蹤全域鎖或多重相依即可理解資料流。
2. Given 維運需要替換資料來源，When 我在 DataEngine 新增/刪除 consumer，Then 不需要改動策略與執行層程式碼。

### Edge Cases

- 連續多筆在同一價格進出的 orderbook 更新。
- 超過 TopN 的深度資料（插入/刪除超過 N 層）。
- Decimal 取值包含負號、極小刻度（1e-10）或非常大的名目值。
- Engine 任一層停止時必須釋放資源並回報狀態。

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: 全部訂單簿操作 MUST 使用 O(log N) 的資料結構；禁止線性掃描。
- **FR-002**: 價格與數量 MUST 在熱路徑維持 `Decimal`/`Fixed` 型別；嚴禁在核心模組用 `f64`。
- **FR-003**: Engine MUST 分為三層（資料、策略、執行），各層只透過訊息或快照通道互動。
- **FR-004**: 風控與策略 MUST 在新資料結構下保留既有功能，所有限額與風險計算以 Decimal 進行。
- **FR-005**: Prometheus 指標 MUST 在重構後維持既有命名，並新增對應的載入/重構成功指標。
- **FR-006**: 提供壓測與單元測試腳本，驗證 p99 延遲與正確性。

### Key Entities *(include if feature involves data)*

- `OrderBook`：BTreeMap-based 結構，提供 `update`, `best_bid`, `best_ask`, `snapshot_top_n`。
- `DataEngine`/`StrategyEngine`/`ExecutionEngine`：新的 Engine 分層實體。
- `LatencyMetrics`：更新後的延遲統計，支援層間測量。

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 壓測下 `hft_latency_ingestion_microseconds` p99 ≤ 10 μs（較現況下降 ≥ 50%）。
- **SC-002**: 策略輸出延遲 p99 ≤ 12 μs；超過自動警示。
- **SC-003**: Engine 模組 `cargo check --lib` 過程無循環依賴與全域鎖等待。
- **SC-004**: 所有新增/修改程式通過單元測試、整合測試與 Spec Kit `/speckit.analyze` 驗證。
