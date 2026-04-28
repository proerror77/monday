# Implementation Plan: Order Book & Engine Rework

**Branch**: `[002-orderbook-rework]` | **Date**: 2025-10-08 | **Spec**: specs/002-orderbook-rework/spec.md

> **2026-04-28 Superseded / Frozen**
>
> This plan is kept as historical context, but it should not drive the next
> implementation phase. The current execution plan is
> `docs/architecture/BINANCE_LOW_LATENCY_MARKET_DATA_PLAN.md`.
>
> Main correction: the next milestone is Binance market data correctness,
> feature computation, signal generation, latency trace, replay, and paper
> trading. A generic BTreeMap + Decimal order book rework is not the hot-path
> target. It may remain useful for cold-path correctness or compatibility, but
> the Binance lane should use fixed-point integers, explicit sequence handling,
> and a fixed-size TopN fast view.

## Summary

重構訂單簿資料結構、統一 Decimal 型別、拆分 Engine 分層，並提供壓測與指標驗證，確保在高頻行情下仍維持 p99 ≤ 10 μs。

## Technical Context

**Language/Version**: Rust stable 1.81+  
**Primary Dependencies**: `std::collections::BTreeMap`, `rust_decimal`, `tokio`, `simd-json`  
**Testing**: `cargo test`, `cargo bench`, 自訂壓測腳本  
**Target Platform**: Linux (x86_64)，支援 Mac 開發  
**Performance Goals**: ingestion/strategy p99 ≤ 10–12 μs  
**Constraints**: 不破壞現有 API；Prometheus 指標維持相容；需與策略/風控/執行模組兼容。

## Constitution Check

- MUST：熱路徑採 O(log N) 以上效率的容器；拒絕線性掃描。
- MUST：數值型別統一 Decimal；禁止 `f64` 遍佈模組。
- MUST：Engine 模組分層清楚，可獨立測試與部署。

## Project Structure

### Documentation (this feature)

```
specs/002-orderbook-rework/
├── spec.md
├── plan.md
├── tasks.md
├── research.md            # 選填：資料結構比較、基準報告
└── benchmarks/            # 壓測腳本與輸出
```

### Source Code (repository root)

```
market-core/
├── core/
│   ├── src/orderbook/            # 新 BTreeMap-based OrderBook
│   ├── src/types.rs              # Symbol/BaseSymbol 內部化
│   └── src/topn_orderbook.rs     # 薄快取/向後兼容層
├── engine/
│   ├── src/data_engine.rs
│   ├── src/strategy_engine.rs
│   ├── src/execution_engine.rs
│   └── benches/
└── runtime/
    └── src/system_builder/       # 裝配與 channel 定義
```

**Structure Decision**: 在 `market-core/core` 新增 `orderbook` 模組，對外導出；Engine 分層直接放在 `market-core/engine/src`；Runtime 只負責裝配與管道。

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| 引入 BTreeMap | 確保 O(log N) 更新與查找 | 手寫陣列 O(N) 已造成性能崩潰 |
| 引擎三層拆分 | 降低單體複雜度 | 保留單體結構難以調試與擴充 |

## Testing Strategy

- 單元測試：OrderBook 更新/刪除/快照；Symbol/BaseSymbol 轉換；Engine 各層介面。
- 壓測：100k updates/s，量測 ingestion/strategy p99；與舊版比較。
- 整合測試：既有 e2e (`dual_exchange_e2e`, `precision_semantic_test`) 必須全部通過。
- 指標驗證：Prometheus 指標與新增指標對照，確保監控一致。

## Risks & Mitigations

- **策略/風控相容性**：逐步導入 Decimal；提供暫時 adapter；新增 lint 防止 `f64` 回滲。
- **效能回退**：建立 benchmark pipeline；若 BTreeMap 不足，評估 heapless/skiplist 作為替代。
- **重構影響面大**：於 feature branch 完成全部變更，通過 `/speckit.analyze` 後再合併。

## Deliverables

- 新 `orderbook` 模組與完整測試。
- 更新的 Engine 三層實作與 channel 溝通。
- 調整後的策略/風控/執行模組，適配新資料結構。
- 壓測腳本與報告，證明性能目標已達成。
