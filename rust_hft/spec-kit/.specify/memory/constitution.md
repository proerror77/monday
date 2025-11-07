<!--
Sync Impact Report
- Version change: (init) → 1.0.0
- Modified principles: initialized Core Principles for Rust HFT
- Added sections: Core Principles, Governance
- Removed sections: none
- Templates requiring updates: 
  - .specify/templates/plan-template.md (⚠ pending)
  - .specify/templates/spec-template.md (⚠ pending)
  - .specify/templates/tasks-template.md (⚠ pending)
- Follow-up TODOs: propagate quality gates to CI, add explicit metrics names mapping
-->

# Rust HFT Spec‑Kit Constitution

## 核心原則（Core Principles）

### I. 程式風格（Code Style）
- 必須使用 `rustfmt`；若無 `rustfmt.toml` 則使用預設規則。CI/Make 走 `cargo fmt -- --check` 強制檢查。
- 必須啟用 `clippy` 嚴格檢查：工作區採用 `cargo clippy --all-targets --all-features -- -D warnings`（見 `Makefile: check`）。核心 crates（`market-core/*`, `risk-control/*`）應達到零警告；apps/tools 可逐步消除警告。
- 模組分層與責任：
  - `market-core/{core,ports,instrument,snapshot,engine,integration,runtime}`：型別與撮合/事件引擎核心。
  - `data-pipelines/{core,adapters-*}`：行情輸入適配器。
  - `execution-gateway/{execution,adapters-*}`：執行與下單適配器。
  - `strategy-framework/{strategy-dl,infer-onnx,strategies/*}`：策略框架與策略實作。
  - `risk-control/{risk,oms-core,portfolio-core}`：風控與投組。
  - `infra-services/{core/*,testing,recovery}`：基礎設施（Redis/ClickHouse/Metrics/IP C）。
  - `apps/{live,paper,replay,all-in-one}`：應用入口與組裝（Runtime Hub）。
- 公共 API 使用明確 newtype 與命名；禁止以 `String`/`u64` 代表多義實體（以 `market-core/core` 之 `Price`/`Quantity`/`Symbol` 等 newtypes 為準）。

### II. 型別與錯誤處理（Types & Errors）
- Domain newtypes：以 `market-core/core` 為權威來源（`Price(rust_decimal)`, `Quantity(rust_decimal)`, `Symbol`, `OrderId` 等），避免語意不清的裸型別。
- 統一錯誤：使用 `market-core/core::error::{HftError, HftResult}` 作為工作區標準返回；庫層以 `thiserror` 描述，跨 crate 邊界不洩漏 `anyhow`。
- 應用/工具層可使用 `anyhow::Result` 聚合，但在對外 API 轉換為 `HftError`。
- 錯誤分類（實務）：`Network/Timeout/RateLimit`（可重試）、`Parse/Serialization/InvalidOrder`（輸入問題）、`Risk/Exchange`（業務限制）、`Bug`（邏輯瑕疵需 fail-fast）。
- 嚴禁在熱路徑使用 `unwrap/expect`；允許在測試或經不變式證明處以 `debug_assert!`。
- `unsafe` 僅允於經審計的性能熱點，需標註不變式、配套測試與專項審查。

### III. 低延遲與記憶體配置（Low Latency & Memory）
- 熱路徑零配置（zero-alloc）：初始化預分配（`with_capacity`/工作緩衝復用/固定長度容器 `arrayvec/smallvec`）；避免動態字串拼接與成長性 push。
- 編譯設定（以現況為準）：
  - `[profile.release]`：`opt-level=3`, `lto="thin"`, `codegen-units=1`, `panic="abort"`（已在根 `Cargo.toml` 設定）。
  - `[profile.bench]`：`opt-level=3`, `lto="fat"`（基準測試極致優化，已設定）。
  - 可選：以環境變數設定 `RUSTFLAGS="-C target-cpu=native"` 於上線環境（依實際機型）。
- CPU/NUMA：Pin 核心、避免跨 NUMA 訪問；必要結構以 `#[repr(align(64))]` 避免 false sharing。
- 分支與內聯：冷分支 `#[cold]`，熱小函數審慎 `#[inline(always)]`（以 `cargo-criterion` 報告/`perf` 驗證）。
- JSON/序列化：熱路徑優先 `simd-json`/零拷貝解析；IPC 使用 `rmp-serde`。避免在引擎循環中進行 serde 分配。
- 網路 I/O：行情採 `tokio-tungstenite` + 批量處理；對外 TLS 使用 `rustls`（已統一於 workspace）。

### IV. 併發與鎖（Concurrency & Locks）
- 熱路徑禁用 `std::sync::Mutex`；首選無鎖/原子資料結構（`crossbeam`, `left-right`, `arc-swap`, `concurrent-queue`）。
- 管理層若必鎖：優先 `parking_lot::{Mutex,RwLock}`；採 `try_lock` + backoff；禁止遞迴鎖（與 `poison` 依賴）。
- 拓撲：單寫入者引擎 + SPSC/MPSC 佇列串接各 stage；快照讀多寫少使用 `left-right` 或 `ArcSwap` flip。
- 任務模型：引擎/撮合同步執行；I/O 與後台任務（恢復、度量、存儲）使用 `tokio`，以無鎖通道與核心交互。
- 減少 `Arc<Mutex<...>>`：以不可變快照或專屬工作執行緒取代跨任務共享可變狀態。

### V. 測試與基準（Testing & Benchmarking）
- 測試分層：單元（各 crate 內）、整合（`tests/integration/`）、E2E（`tests/` 頂層）、基準（`benches/` 與 `market-core/engine/benches/`）。
- 基準：使用 `criterion 0.5`（engine 已啟用 `html_reports`）；建議在固定 CPU 親和/關閉渦輪/固定時脈下執行；輸出 `median/p99` 延遲與吞吐，與主分支比對，預設回歸閾值 2%。
- 目標：端到端 p99 ≤ 25µs、單事件熱路徑 < 5µs（依硬體修正）；吞吐 ≥ 100k ops/s；見 `tests/integration/performance_benchmarks.rs`。
- 命令：`cargo test`、`cargo bench -p hft-engine`（或在 `market-core/engine` 執行 `cargo bench`）。

### VI. 觀測性（Tracing & Metrics）
- Tracing：應用於 `apps/*` 初始化 `tracing_subscriber::fmt().with_env_filter("info")`；熱路徑 span 嚴格控量，必要欄位含 `order_id`、`symbol`、`venue`、`seq`、`latency_ns`。
- Metrics：以 `infra-services/core/metrics` 為唯一來源，暴露（部分）：
  - 延遲：`hft_latency_ingestion_microseconds`、`hft_latency_engine_ns`、`hft_latency_egress_microseconds`
  - 吞吐：`hft_throughput_events_per_sec`
  - 穩定性：`hft_backpressure_events`、`hft_drops_total`
- Logs：僅在調試與非熱路徑使用 `info!`；熱路徑以 counter/histogram 替代。

### VII. 安全與法遵（Security & Compliance）
- 憑證：採用 `aws-sm`/`aliyun-kms` 之機密管理，不將憑證放入 repo；本 repo 已忽略 `.pem`/敏感檔案。
- 輸入驗證：Exchange 連接與策略輸入需 schema 驗證；拒絕不合法訂單與超限參數。
- 風險：下單路徑前置 `risk-control/*` 的限制檢查（頭寸、限價、速率、風險曲線）；異常即 fail-fast 與事件告警。
- 法遵：依地區做 KYC/AML/交易限制，提供審計日誌（append-only）。

### VIII. 部署與回溯（Deployment & Rollback）
- 打包：`cargo build --release`；容器映像基於 `distroless` 或 `alpine`（依需求）；調試版保留 `perf` / `gdb` 權限。
- 配置：`config/*` 下以環境覆寫（12-factor）；熱路徑配置於啟動時加載並固定，不在熱路徑重讀。
- 健康檢查：`/healthz`、`/metrics`；集成 Prometheus/Grafana（已有 `grafana-dashboard.json`）。
- 回溯：灰度發布 + 一鍵回滾腳本；保留舊版本二進位與配置。

### IX. 變更控制與版本規則（Change Control & Versioning）
- SemVer：工作區以語意化版本（核心 crate 同步小版本，破壞性變更時 MAJOR+1）。
- 變更流程：重大改動以 ADR（`docs/ADR-YYYYMMDD-*.md`）或 `docs/REFACTOR_PLAN.md`；需求由 `/speckit.specify` → `/speckit.plan` → `/speckit.tasks` 衍生；PR 需鏈接對應文檔。
- Gate：PR 合併必須滿足：
  - 單元/整合測試全綠；`criterion` 與主分支比對無顯著回歸（預設 2%）。
  - Clippy 零警告（核心 crates），fmt 檢查通過。
  - Tracing/metrics 變更對應更新文檔與告警規則。

## 治理（Governance）
- 合規性：本憲章優先於其他風格指南；與業務目標衝突時以風險與透明度為準，需 CTO/技術委員會裁決。
- 修訂：每次修訂需在開頭新增 Sync Impact Report 並 bump 版本；
  - PATCH：措辭/非語意澄清。
  - MINOR：新增原則或實質擴充。
  - MAJOR：破壞性治理/原則調整。
- 審查：每季進行一次原則回顧（含基準目標/告警規則適配最新硬體與市場）。

**Version**: 1.0.0 | **Ratified**: 2025-10-08 | **Last Amended**: 2025-10-08
