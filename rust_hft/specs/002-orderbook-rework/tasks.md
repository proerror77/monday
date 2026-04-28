---
description: "Tasks for Order Book & Engine Rework"
---

# Tasks: Order Book & Engine Rework

**Input**: Design documents from `/specs/002-orderbook-rework/`
**Prerequisites**: plan.md、research.md、現有 benchmark

> **2026-04-28 Superseded / Frozen**
>
> Do not execute this task list as the active P0/P1 roadmap. The next active
> task list lives in `docs/architecture/BINANCE_LOW_LATENCY_MARKET_DATA_PLAN.md`.
> In particular, T101/T202/T203 are no longer the preferred first moves for the
> low-latency Binance lane; start with Binance depth sequence correctness,
> fixed-point TopN features, latency trace, replay, and remote/CI verification.

## Format: `[ID] [P?] Description`
- `[P]`: 可平行執行
- 任務依優先度排序，先重構資料結構，再進行分層

---

## Phase 1: 訂單簿資料結構重構 (P1)

- [ ] T101 [P] 建立 `market-core/core/src/orderbook/` 模組並撰寫 BTreeMap-based OrderBook（insert/update/remove/snapshot）
- [ ] T102 [P] 將 `topn_orderbook` 改為薄快取（僅從新 orderbook 抽取前 N 檔）；移除線性搜尋與 `epsilon`
- [ ] T103 更新聚合/行情流程 (`market-core/engine/src/aggregation.rs` 等) 使用新 orderbook API
- [ ] T104 撰寫單元測試/benchmarks 對比原實作性能（至少 10k updates/s）（結果記錄於 benchmarks/）

## Phase 2: 數值型別一致化 (P1)

- [ ] T201 盤點熱路徑 `f64` 使用處（Symbol/BaseSymbol 等），提供 `Symbol::as_str()` helper
- [ ] T202 將 orderbook/行情事件/策略/風控流程換成 Decimal/Fixed 型別
- [ ] T203 調整 JSON parsing（simd_json）直接產出 Decimal 型別
- [ ] T204 更新風控與策略測試，確保數值無精度損失

## Phase 3: Engine 分層與 channel (P2)

- [ ] T301 新增 `DataEngine`（consumers + aggregator + snapshot publisher）
- [ ] T302 新增 `StrategyEngine`（策略列表 + market snapshot subscriber）
- [ ] T303 新增 `ExecutionEngine`（router + OMS + clients）；保持 interface 向後相容
- [ ] T304 重寫 `SystemBuilder`、`Engine` 裝配流程，使用 channel 傳遞資料；所有層移除全域 Mutex

## Phase 4: 壓測與觀測性 (P1)

- [ ] T401 撰寫壓測腳本（100k updates/s）並記錄 `hft_latency_*` 指標
- [ ] T402 驗證 Prometheus 指標與新延遲指標保持一致；新增必要的 layer 指標
- [ ] T403 更新文件/README，說明新架構與操作方式

## Phase 5: 清理與合併 (P1)

- [ ] T501 清理未使用欄位/方法（hyperliquid/grvt adapter 等）；必要時加註 `#[allow(dead_code)]`
- [ ] T502 `/speckit.analyze` 與既有整合測試通過
- [ ] T503 寫壓測報告並附於 benchmarks/

---

## Dependencies & Execution Order

1. Phase 1 & Phase 2 可部分平行，但建議先完成 OrderBook，量測初始效能再推進型別一致化。
2. Phase 3 必須在 Phase 1/2 完成後進行，以免 channel 設計依舊資料結構考量。
3. Phase 4/5 收尾，確保監控與測試通過。
