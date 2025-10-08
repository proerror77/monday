# HFT System Architecture Documentation

**版本**: 5.1 (2025-08-25)  
**狀態**: 🟢 **架構重構完成，生產就緒**

## 概述

本文檔描述了 HFT 系統的最終架構設計、目錄結構、命名規範和開發指南。

## 核心架構原則

### 1. 三層架構分離
```
L1: Rust 核心引擎     - 微秒級交易執行
L2: 運維工作區        - 監控告警處理  
L3: 機器學習工作區    - 模型訓練優化
```

### 2. 特性驅動設計
- **Feature Gates**: 條件編譯控制依賴
- **Runtime Hub**: 統一特性轉發中心
- **Workspace 一致性**: 版本和依賴統一管理

### 3. 職責清晰分離
- **apps/**: 應用層程式入口
- **crates/**: 核心庫和組件  
- **strategies/**: 獨立策略實現
- **ops/**: 運維和基礎設施

## 目錄結構

```
rust_hft/
├── market-core/                 # 行情核心：基礎型別 + 引擎 + Runtime
│   ├── core/
│   ├── ports/
│   ├── instrument/
│   ├── snapshot/
│   ├── engine/
│   ├── integration/
│   └── runtime/
├── data-pipelines/              # 行情輸入渠道
│   ├── core/                    # 統一 MarketStream 實作
│   └── adapters/                # 各交易所行情適配器
├── strategy-framework/          # 策略框架與示例
│   ├── strategies/
│   ├── strategy-dl/
│   └── infer-onnx/
├── risk-control/                # 風控與頭寸管理
│   ├── risk/
│   ├── oms-core/
│   └── portfolio-core/
├── execution-gateway/           # 交易執行與適配
│   ├── execution/
│   └── adapters/
├── infra-services/              # 基礎設施服務
│   ├── core/
│   │   ├── clickhouse/
│   │   ├── redis/
│   │   ├── metrics/
│   │   ├── venue/
│   │   └── ipc/
│   ├── testing/
│   └── recovery/
├── apps/                        # 實際運行入口（Live/Paper/Replay 等）
├── tools/                       # 補助工具（Collector / Dataset / Infer CLI…）
├── docs/                        # 文檔
├── examples/                    # 教學與 Demo
├── tests/                       # 集成與 E2E 測試
├── ops/                         # 運維腳本與 Docker 編排
├── scripts/                     # 開發工具腳本
└── legacy/                      # 歷史/存檔代碼
```

## 命名規範

### 1. Crate 命名規範

#### 核心 Crates
```
hft-core         - 核心類型和錯誤 (零依賴)
hft-ports        - 穩定介面定義
hft-engine       - 事件循環引擎
hft-runtime      - 系統運行時
```

#### 功能 Crates
```
hft-data            - 數據處理核心
hft-execution       - 執行核心
hft-risk            - 風險管理
hft-integration     - 網路集成
```

#### 適配器 Crates
```
hft-data-adapter-{venue}       - 數據適配器
hft-execution-adapter-{venue}  - 執行適配器
```

#### 基礎設施 Crates  
```
hft-infra-{service}  - 基礎設施集成
例: hft-infra-clickhouse, hft-infra-redis
```

#### 策略 Crates
```
hft-strategy-{name}  - 策略實現
例: hft-strategy-trend, hft-strategy-arbitrage
```

### 2. 應用命名規範

```
hft-live    - 真盤交易應用
hft-paper   - 模擬盤交易應用  
hft-replay  - 歷史回放應用
```

### 3. 特性命名規範

#### 交易所特性
```
bitget   - Bitget 交易所支持
binance  - Binance 交易所支持
mock     - 模擬交易所
```

#### 策略特性
```
trend-strategy      - 趨勢策略
arbitrage-strategy  - 套利策略
dl-strategy        - 深度學習策略
```

#### 基礎設施特性
```
clickhouse  - ClickHouse 數據存儲
redis       - Redis 緩存和消息
metrics     - Prometheus 指標
```

#### 便利特性
```
full          - 所有特性
all-exchanges - 所有交易所
all-strategies - 所有策略
full-infra    - 所有基礎設施
```

## 依賴關係規則

### 1. 依賴層級
```
Level 0: core (零依賴)
Level 1: ports, instrument (依賴 core)
Level 2: engine, data, execution (依賴 L0+L1)
Level 3: runtime, strategies (依賴 L0+L1+L2)
Level 4: apps (依賴所有層級)
```

### 2. 循環依賴禁止
- 同級別 crate 之間禁止相互依賴
- 高級別不能依賴更高級別
- 適配器只能依賴其對應的核心 crate

### 3. Workspace 依賴管理
```toml
[workspace.dependencies]
# 所有版本在 workspace 級別統一管理
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1.0", default-features = false }
```

## 特性系統設計

### 1. Runtime Hub 模式
```toml
# apps/live/Cargo.toml
[features]
bitget = ["runtime/adapter-bitget-data", "runtime/adapter-bitget-execution"]
trend-strategy = ["runtime/strategy-trend"]
```

### 2. 條件編譯
```rust
#[cfg(feature = "bitget")]
info!("✓ Bitget 交易所");

#[cfg(feature = "trend-strategy")]
info!("✓ 趨勢策略");
```

### 3. 可選依賴
```toml
# runtime/Cargo.toml
[dependencies]
adapter-bitget-data = { ..., optional = true }

[features]
adapter-bitget-data = ["dep:adapter-bitget-data"]
```

## 測試策略

### 1. 測試分層
```
單元測試     - 各 crate 內部 (src/lib.rs #[cfg(test)])
集成測試     - tests/integration/
E2E測試      - tests/ (頂層)
性能測試     - benches/
```

### 2. 測試命名
```rust
#[test]
fn test_component_functionality() { }   // 基本功能測試

#[test] 
fn test_error_handling_edge_case() { }  // 錯誤處理測試

#[bench]
fn bench_performance_critical_path() { } // 性能基準測試
```

## 配置管理

### 1. 環境分層
```
config/dev/      - 開發環境 (寬鬆限制)
config/staging/  - 測試環境 (接近生產)
config/prod/     - 生產環境 (嚴格限制)
```

### 2. 配置加載順序
1. 默認配置 (代碼中)
2. 環境配置文件
3. 環境變量覆蓋
4. 命令行參數

## 運維和部署

### 1. 基礎設施管理
```bash
make dev        # 啟動開發環境
make build      # 構建所有應用
make test       # 運行測試套件
make health     # 健康檢查
```

### 2. 容器化
```
ops/docker-compose.yml      - 開發環境
ops/deployment/k8s/         - Kubernetes 部署
ops/deployment/docker/      - Docker 生產部署
```

## 性能要求

### 1. 延遲目標
- P99 < 25μs (端到端)
- P50 < 10μs (本地處理)
- WebSocket 延遲 < 1.5ms

### 2. 吞吐量目標  
- 100,000+ ops/sec
- 1000+ symbols 並發
- 10GB+ 日數據處理

## 開發指南

### 1. 添加新特性
1. 在對應 crate 中實現
2. 添加 feature gate
3. 在 runtime 中集成
4. 在 apps 中轉發
5. 添加測試和文檔

### 2. 添加新適配器
```
1. 創建 crates/data/adapters/adapter-{venue}/
2. 實現 MarketDataAdapter trait
3. 添加 feature: adapter-{venue}-data
4. 在 runtime 中註冊
```

### 3. 添加新策略
```
1. 創建 strategies/{name}/
2. 實現 Strategy trait
3. 添加 feature: strategy-{name}
4. 在 runtime 中集成
```

## 版本控制

### 1. 語義版本
- 主版本: 架構重大變更
- 次版本: 新功能添加
- 修訂版本: Bug 修復

### 2. 發布流程
1. 特性分支開發
2. PR 審查和測試
3. 合併到 main
4. 自動 CI/CD 部署

## 質量保證

### 1. 代碼質量
- Clippy 檢查: 0 warnings
- 測試覆蓋率 > 80%
- 文檔完整性檢查

### 2. 性能驗證
- 基準測試回歸檢查
- 內存洩漏檢測
- 併發安全驗證

---

此文檔隨系統演進持續更新，確保架構文檔與實際實現保持一致。

## 架構校驗與後續重構（2025-10-07）

本日對核心代碼（engine/runtime/ports/snapshot/integration）進行逐段校驗，結論如下：

- 熱路徑設計已達專業 HFT 標準：單寫入者引擎、SPSC 佇列、ArcSwap/left-right 快照、工作緩衝復用、FxHashMap，與文檔描述一致。
- async 僅用於 I/O 與後台任務，撮合/路由熱圈保持同步，不存在不必要的調度開銷。
- Runtime Hub 與 feature 轉發符合設計；多帳戶/路由映射與恢復/觀測（Redis/ClickHouse/IPC）按需可選。

為提升維護性與消重，計劃進行小幅「結構性重構」，不改動行為與熱路徑：

1) P0 Adapters-Common（不合併適配器為單一 crate）
- 目標：抽取行情適配器共用邏輯（JSON 解析、訂閱組裝、錯誤映射、WS/REST 輔助）到 `data-pipelines/adapters-common`，各交易所仍保留獨立 crate。
- 效益：顯著降低重複（每個 adapter 約下降 70–85% 重複段），又不犧牲編譯增量快取與邊界。

2) P1 System Builder 模組化
- 現狀：`market-core/runtime/src/system_builder.rs` 體量偏大。
- 拆分：新增 `config_types.rs`（配置結構）、`infra_exporters.rs`（Redis/ClickHouse 任務）、`runtime_management.rs`（運行時管理 API），主檔僅保留 Builder 與 start/stop 核心流程。

3) P2 管理層鎖型優化
- 僅於 runtime/IPC 管理層使用 `parking_lot::Mutex`，不觸碰熱路徑；避免偶發慢鎖，提升可預測性。

4) P3 度量與可觀測性補強
- 在 `EngineStats` 增加 backpressure/丟棄統計，導出至 metrics；補齊 Criterion 基準測試與（環境可用時）flamegraph 腳本。
 - 引擎指標採用可選 feature（`engine/metrics`），避免熱路徑直接依賴基礎設施；由 runtime 於每秒狀態任務將統計以 Gauges 輸出到 `hft-infra-metrics`。

本重構僅為檔案/模組邊界整理與共用碼抽取，不改動運行語義與熱路徑行為。詳見 `docs/REFACTOR_PLAN.md`。
