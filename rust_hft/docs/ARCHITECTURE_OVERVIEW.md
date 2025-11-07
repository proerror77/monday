# HFT 系統架構概覽

本文件提供當前工作區的高層架構地圖、核心模組職責與建置流程。詳盡的設計決策可參考 `ARCHITECTURE.md`，此處著重於 Phase 1.5 後的模組互動與編譯建議。

## 模組分層

| 層級 | Crate / 目錄 | 職責 |
| ---- | ------------- | ---- |
| 核心 (Core Primitives) | `crates/core` | 基礎型別 (`Price`, `Quantity`, `Side`)、錯誤、時間戳與常用工具。保持零依賴，供所有上層模組共用。 |
| 穩定介面 (Ports) | `crates/ports` | 宣告 `MarketStream`, `ExecutionClient`, `Strategy` 等穩定 trait 與事件模型，作為策略與外部適配器間的契約。 |
| 引擎 (Engine) | `crates/engine` | 事件處理循環、行情接收與執行佇列管理。聚焦單 writer、高效資料路徑。 |
| 執行層 (Execution) | `crates/execution` + `crates/execution/adapters/*` | 封裝各交易所 REST/WS 與模擬執行。每個適配器應實作 `ports::ExecutionClient`。 |
| 行情層 (Data) | `crates/data` + `crates/data/adapters/*` | 整合各交易所行情流，轉換成統一的 `MarketEvent`。 |
| 策略層 (Strategies) | `crates/strategy-*`, `strategies/*` | 策略實作。範例：`strategy-dl` 提供同步推理策略；`strategies/trend` 單純示範技術指標策略。 |
| Runtime | `crates/runtime` | `SystemBuilder`/`SystemConfig` 用於宣告式裝配市場資料、執行客戶端、策略與風控，並提供分片、降級、適配器自動註冊能力。 |
| 應用 (Apps) | `apps/live`, `apps/paper`, `apps/replay`, `apps/collector`… | 對外二進位入口。透過 feature flag 選擇需要的適配器與策略，並載入 YAML/CLI 設定。 |
| 工具與測試 | `apps/infer-cli`, `tests/*`, `examples/*` | 工具程式與 E2E 範例。沉重或已淘汰的範例集中於 `examples/_archive`。 |

## 多交易所資料流

```text
MarketStream (Data Adapter) --> Engine::dataflow --> Strategy --> ExecutionClient --> Venue
                                         |                                |
                                         +-- Runtime metrics/logging      +-- Risk Manager / Portfolio
```

1. **行情接入**：各交易所 adapter 解析原生 WS 資料，轉換成 `MarketEvent`。
2. **引擎處理**：`engine` 統一緩衝與節流事件，並交由策略評估。
3. **策略決策**：策略根據 `MarketEvent`、`AccountView` 產生 `OrderIntent`。
4. **執行層**：`ports::ExecutionClient` 實作（或模擬客戶端）負責送單與錯誤降級。
5. **風控 / Portfolio**：`runtime` 的 `RiskManager` 及 `portfolio-core` 更新帳戶狀態。

分片 (`crates/runtime/src/sharding.rs`) 允許在多進程/多任務間按符號或交易所切割工作負載。

## 編譯建議

1. 預設 `cargo build` / `cargo check` 僅會編譯核心模組與主要 app (`default-members` 已設定)。
2. 如需特定適配器或策略，使用 feature flag：
   ```bash
   cargo run -p apps/live --features "bitget,binance,strategy-dl"
   ```
3. 若需舊範例 (`examples/03_execution`)，請顯示啟用對應 feature（詳見 `examples/Cargo.toml`）。
4. 大量 adapter 可採用 `cargo build -p <crate>` 單獨建置，避免每次編譯整個 workspace。

## 後續整理建議

- 將 OMS 相關程式碼從 `examples/03_execution` 拆出成獨立 library crate，並以新 app (`apps/all-in-one`) 關聯。
- 擴充 `runtime` 的 `RiskConfig` 以支援 per-account/per-venue 限額，並導出監控指標。
- 針對 `crates/execution` 與 `crates/data` 撰寫 README / 模組說明，協助 adapter 開發者快速上手。
