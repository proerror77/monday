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
├── apps/                    # 應用層 (Feature Gates 入口)
│   ├── live/               # 真盤交易應用
│   ├── paper/              # 模擬盤交易應用
│   └── replay/             # 歷史回放應用
├── crates/                 # 核心庫和組件
│   ├── core/              # 零依賴基礎類型
│   ├── ports/             # 穩定事件和traits
│   ├── engine/            # 單writer事件循環
│   ├── data/              # 市場數據處理
│   │   └── adapters/      # 交易所適配器
│   ├── execution/         # 交易執行組件
│   │   └── adapters/      # 執行適配器
│   ├── risk/              # 風險管理
│   ├── runtime/           # 系統運行時和建構器
│   └── infra/             # 基礎設施集成
├── strategies/            # 策略實現
│   ├── trend/            # 趨勢策略
│   └── arbitrage/        # 套利策略
├── config/               # 配置管理
│   ├── dev/              # 開發環境配置
│   ├── staging/          # 測試環境配置
│   └── prod/             # 生產環境配置
├── ops/                  # 運維和基礎設施
│   ├── clickhouse/       # ClickHouse 配置
│   ├── monitoring/       # Prometheus + Grafana
│   ├── deployment/       # 部署腳本
│   └── docker-compose.yml
├── tests/                # 集成和E2E測試
│   └── integration/      # 集成測試
├── examples/             # 示例和教程
├── scripts/              # 開發腳本
└── docs/                 # 項目文檔
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