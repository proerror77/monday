# Rust HFT 重構計畫

此文件紀錄我們的長期重構方向與待辦事項，目標是將現有高頻交易系統整理為結構清晰、易於擴展與維護的架構。每次改動請同步更新本計畫。

## 2026-04-28 計畫修正：當前主線收窄到 Binance Market Data + Signal Engine

本文件原本描述的是「完整交易平台」的長期重構方向。這個方向可以保留，但不再作為當前 P0/P1 執行主線。下一階段以 [`docs/architecture/BINANCE_LOW_LATENCY_MARKET_DATA_PLAN.md`](architecture/BINANCE_LOW_LATENCY_MARKET_DATA_PLAN.md) 為高層藍圖，具體執行順序以 [`docs/architecture/BINANCE_MD_REFACTOR_EXECUTION_PLAN.md`](architecture/BINANCE_MD_REFACTOR_EXECUTION_PLAN.md) 為準。

當前主線不是先做泛化 HFT 系統，而是先做：

```text
Binance raw market data
  -> local order book correctness
  -> microstructure features
  -> signal generation
  -> latency trace
  -> replay
  -> paper trading
```

因此，以下工作暫時降級為後續平台化任務，不應阻塞 Binance fast lane：

- 泛化 `sdk` crate / CLI。
- 多交易所 adapter 大重構。
- 完整 Strategy -> Risk -> Execution 實盤下單鏈路。
- 跨交易所套利與完整 OMS。
- Generic BTreeMap + Decimal 訂單簿全量替換。

新的優先級：

| 優先級 | 目標 | 驗收 |
|--------|------|------|
| P0 | Binance depth stream + snapshot/diff bridge | update id 連續、gap 可檢測、reconnect 可 rebuild |
| P1 | 本地 order book + TopN fast view | best bid/ask 與 Binance bookTicker sanity check 對齊 |
| P2 | OBI/microprice/spread/staleness + `Signal` | signal 有 timestamp、expiry，策略不直接下單 |
| P3 | per-stage latency trace + replay | live/replay 對同一輸入產生同一 book/features/signals |
| P4 | remote/CI benchmark + paper trading | 不依賴本機重編譯；p50/p95/p99/p999/max 有證據 |

舊里程碑 M2-M7 仍是長期平台治理方向，但需要等 Binance market-data lane 可測、可回放、可穩定運行後再恢復。

## 2026-04-29 計畫補完：Q2 主線

2026 Q2 的可執行主線以
[`docs/architecture/HFT_2026_Q2_EXECUTION_PLAN.md`](architecture/HFT_2026_Q2_EXECUTION_PLAN.md)
為準。該計畫把已完成的 market-data fast lane、Bitget/Linux staging
工具，以及本季可在本機完成的隊列拓撲、Signal/OrderIntent 生命週期、
OMS/risk/reconciliation 測試、paper/shadow evaluation、overload mode、
exchange rules/rate limits/cost model 全部合併成季度里程碑。

判斷口徑：

- 本機負責結構正確性、測試、replay/paper/shadow、隊列與風控邊界。
- Linux staging 負責 p99/p999 真實延遲證據。
- small live、跨交易所實盤套利、FIX/SBE/order-entry、kernel bypass
  延後到 Q2 驗收完成後。

---

## 核心原則

1. **單一入口 (`sdk`)**：對外提供簡潔 API、CLI 與模板；所有使用者操作都經由 `sdk`。
2. **明確分層**：資料、引擎、策略、執行、回測、部署、基礎設施、共享型別各司其職。
3. **宣告式配置**：Instrument / Venue / Strategy / Infra 透過 YAML + 強型別 schema 管理，避免散落常量。
4. **抽象可重用**：行情、下單、風控、觀測、恢復等以 trait/registry/插件方式提供，讓新交易所或新商品能快速掛上。
5. **漸進式遷移**：先修復現有編譯與測試，再逐塊搬移；保持主分支可穩定建置與回測。

---

## 里程碑總覽

| 階段 | 目標 | 產出 | 狀態 |
|------|------|------|------|
| M1 | 修復當前編譯錯誤與測試失敗 | 恢復 `cargo check/test` 通過 | ☐ 未開始 |
| M2 | 建立 Instrument/Venue catalog 與強型別 config | `shared::instrument`, `shared::config` | ☐ 未開始 |
| M3 | 建立 `sdk` crate 與 CLI | `sdk::HftSystem`, `sdk::cli` | ☐ 未開始 |
| M4 | 重排 workspace 目錄，導入新分層 | `data/`, `engine/`, `strategy/`... | ☐ 未開始 |
| M5 | 策略／風控／執行註冊插件化 | 新版 `SystemBuilder`、Risk plugins | ☐ 未開始 |
| M6 | Backtest/Deploy/Infra 統整 | `backtest::Runner`, CLI 指令 | ☐ 未開始 |
| M7 | 文檔與模板更新 | `docs/ARCHITECTURE.md` 等 | ☐ 未開始 |

---

## 2025-10-07 更新：精準重構補充（行為不變）

本更新基於對核心代碼的逐段校驗，聚焦「降低重複、提升可維護性、補強度量」，不改動熱路徑行為。

### P0：Adapters-Common 抽取（最高優先級）

- 新增 crate：`data-pipelines/adapters-common`
  - 提供共用解析與支援：`converter`（SIMD JSON/數值解析/型別轉換）、`subscriptions`（訂閱組裝器）、`errors`（錯誤映射）、`ws_helpers`/`rest_helpers`（依賴現有 integration/ws/http）。
  - 各交易所 adapter 保持獨立 crate，改為依賴 adapters-common，避免大一統單 crate 帶來的重編譯與回歸面。
- 風險控制：在 adapters-common 建立最小整合測試（解析 + 訂閱 + 轉換），確保改動一次覆蓋所有 adapter。
- 產出：顯著降低 adapter 之間的重複（預估 70–85%），保持現有 Feature gates 與編譯邊界。

### P1：System Builder 模組化（高優先級）

- 拆分 `market-core/runtime/src/system_builder.rs`：
  - `system_builder/config_types.rs`：僅放置 runtime 專屬配置型別；能 `pub use shared_config` 的盡量重用，避免雙份結構漂移。
  - `system_builder/infra_exporters.rs`：封裝 Redis/ClickHouse 相關任務與資料列結構（以 `#[cfg(feature=…)]` 條件編譯）。
  - `system_builder/runtime_management.rs`：運行時管理 API（更新策略參數、風控、取消訂單、測試下單、快照讀取）。
  - 主檔 `system_builder.rs` 僅保留 Builder 與 start/stop 核心流程。
- 產出：主檔行數預計下降 ~60–70%，職責與邊界更清晰。

### P2：管理層鎖型優化（中優先級）

- 僅在 runtime/IPC 管理層由 `std::sync::Mutex` 換為 `parking_lot::Mutex`；熱路徑不變；不依賴 poisoning 行為。

### P3：度量與測試補強（中優先級）

- `EngineStats` 增加背壓/丟棄/策略意圖計數等指標，導出至 metrics。
- `benches/` 增補：`Engine::tick`、聚合、adapter decode microbench（Criterion）。
- （環境允許）補 `cargo bloat`/`flamegraph` 指令腳本，定位前 10 熱點符號。

---

## 分階任務清單（可逐步勾選）

- [ ] P0 新增 `adapters-common` crate 骨架與最小測試
- [ ] P0 改造首個 adapter（建議 asterdex 或 mock）切換到共用庫
- [ ] P0 批次遷移其他 adapters（bitget/binance/bybit…）
- [ ] P1 抽取 `config_types.rs` 並通過編譯
- [ ] P1 抽取 `infra_exporters.rs`（Redis/ClickHouse）並通過編譯（含 feature 測試）
- [ ] P1 抽取 `runtime_management.rs` 並通過編譯
- [ ] P2 將 runtime/IPC 管理層鎖改為 `parking_lot`
- [x] P3 增補 `EngineStats` 指標並導出到 metrics（runtime 每秒導出 Gauges；engine 熱路徑採 feature gate）
- [x] P3 補齊 Criterion microbench（Engine::tick、TopN 聚合）；flamegraph/bloat 腳本待後續補充

備註：以上改動均屬結構與可觀測性整理，對熱路徑行為與延遲指標無語義變更。

---

## 詳細待辦 (To-Do List)

### M1：修復與清理（短期必做）

- [x] 修正 `ExecutionClient` trait 變更造成的 mock 編譯錯誤（recovery module）。
- [x] 補齊 `PortfolioState` 新欄位初始化，移除 `E0063`。
- [x] 確認 `SystemBuilder` 正確套用 YAML 策略參數（Trend/Arbitrage/Imbalance...）。
- [x] 移除或修正 `rust_hft::` 過時引用，暫時改為直接使用實際 crate。
  - 2025-02-14: 先將 legacy 範例、測試、工具以 `legacy-sdk` feature gate 暫停編譯，待新 sdk 上線後重寫。
- [x] 讓 `cargo check --workspace`、`cargo test --workspace` 在現有結構下重新通過。
  - 2025-02-14: 加入 legacy feature gate + 必要依賴修復後，`cargo check/test --workspace --no-run` 均可執行（仍有既有 warnings 待後續整理）。

### M2：共享模型與配置

- [x] 建立 `shared::instrument` crate：`InstrumentId`, `InstrumentMeta`, `InstrumentCatalog`。
  - 2025-02-14: 新增 `shared/instrument` crate，內含 Catalog API 與 YAML 範例。
- [x] 建立 `shared::venue`/`CapabilityFlags`，讀取 `config/venues/*.yaml`。
  - 2025-02-14: `VenueMeta`/`VenueCapabilities` 已整合到 shared/instrument。
- [x] 將 `SystemConfig`, `StrategyConfig` 移到 `shared::config`，使用 `serde` 強型別驗證。
  - 2025-02-14: `shared/config` 支援 schema_version `v2`，Runtime Loader 可自動轉換。
- [x] 新增 `config/instruments.yaml` 範本與載入流程文件化。
  - 2025-02-17: 建立 YAML 範本、支援 `HFT_INSTRUMENT_CATALOG` 路徑覆寫並於 loader 驗證，`SystemBuilder` 可依 catalog 自動訂閱行情。
- [x] 新增 `config/venues.yaml` 模板與說明。
  - 2025-02-17: 新增 venue catalog 範本，記錄 REST/WS 端點與能力旗標，Runtime loader 已支援 `HFT_VENUE_CATALOG` 合併資訊，並可自動填入缺失的 `symbol_catalog`。

### M3：`sdk` 入口

- [ ] 建立 `sdk` crate，re-export 核心模組（engine、strategy、execution、shared 等）。
- [ ] 實作 `HftSystem::from_config`、`HftSystem::start/stop` API。
- [ ] 實作 `sdk::cli`：`hft run`, `hft backtest`, `hft deploy`, `hft init-config`。
- [ ] 更新 `examples/` 與 `tests/` 只透過 `sdk` 使用系統。

### M4：Workspace 重新分層

- [ ] 將現有 `market-core/*` 拆分為 `shared/` + `engine/`，保留相容 re-export 過渡。
- [ ] `data-pipelines` → `data/`; `execution-gateway` → `execution/`; `infra-services` → `infra/`。
- [ ] 為新的目錄補充 `Cargo.toml`、`mod.rs`、`README`。

### M5：策略、風控、執行插件化

- [ ] `SystemBuilder` 改寫：以 catalog 為基礎註冊市場/執行/策略。
- [ ] 提供 `StrategyRegistry`，支援模板與自定策略。
- [ ] 風控介面模組化：`RiskManagerFactory` 支援不同策略限額、合規規則。
- [ ] Execution adapters 透過 `AdapterRegistry` 管理，新增交易所只需註冊一次。

### M6：Backtest / Deploy / Infra 一致化

- [ ] `backtest::Runner` 使用真實 engine + strategy + data replay。
- [ ] `deploy/` 整合 ECS/K8s/Terraform，`sdk CLI` 可呼叫。
- [ ] `infra` 提供 Redis、ClickHouse、Recovery、Observability 統一初始化 API。
- [ ] 建立合規/審計擴充點（log export, report generator）。

### M7：文檔與模板

- [ ] 更新 `docs/ARCHITECTURE.md` 描述新分層與資料流程。
- [ ] 撰寫 `docs/CONFIG_REFERENCE.md`、`docs/GETTING_STARTED.md`。
- [ ] 提供策略模板 (`sdk/templates/`)，並建立 `cargo generate` 腳手架。
- [ ] 在 repo root 補上 `Makefile` 或 `cargo xtask` 流程（fmt/lint/test/backtest/demo）。

---

## 維護規則

1. 每次完成/新增工作，請更新本檔案勾選狀態並新增備註、日期與責任人（必要時）。
2. 若需新增里程碑或待辦，請依序補在對應章節，避免遺漏。
3. 如計畫有重大調整，先於 PR/Issue 討論後再更新本檔案，確保團隊一致。
