# 項目結構

## 根目錄結構
```
monday/
├── rust_hft/                 # L1: Rust核心引擎
├── ml_workspace/              # L3: ML訓練Agent (GPU批次任務)
├── ops_workspace/             # L2: 運營監控Agent (7x24常駐)
├── protos/                    # 統一gRPC契約
├── deployment/                # Docker + K8s部署
├── docs/                      # 完整文檔
└── shared_config/             # 共享配置
```

## Rust 核心引擎結構 (`rust_hft/`)
```
rust_hft/
├── src/
│   ├── core/                  # 核心數據結構和訂單簿
│   │   ├── types.rs          # 基礎類型定義
│   │   ├── orderbook.rs      # 標準訂單簿
│   │   ├── lockfree_orderbook.rs  # 無鎖訂單簿
│   │   └── ultra_fast_orderbook.rs # 超高性能訂單簿
│   ├── exchanges/             # 交易所抽象層
│   │   ├── mod.rs            # ExchangeManager 和 HealthMetrics
│   │   ├── event_hub.rs      # 事件分發中心
│   │   ├── exchange_trait.rs # 交易所統一接口
│   │   ├── bitget.rs         # Bitget 實現
│   │   └── binance.rs        # Binance 實現
│   ├── engine/                # 交易引擎
│   │   ├── complete_oms.rs   # 完整的訂單管理系統
│   │   ├── execution.rs      # 訂單執行引擎
│   │   ├── risk_manager.rs   # 風險管理
│   │   └── strategies/       # 交易策略
│   ├── integrations/          # 外部集成
│   │   ├── redis_bridge.rs   # Redis 數據橋接
│   │   └── unified_bitget_connector.rs # Bitget 連接器
│   ├── ml/                    # 機器學習推理
│   │   ├── inference_engine.rs
│   │   └── features/         # 特徵工程
│   └── utils/                 # 工具函數
├── tests/                     # 測試
│   ├── unit/                 # 單元測試
│   └── integration/          # 集成測試
├── benches/                   # 基準測試
├── examples/                  # 範例程序
└── Cargo.toml                # 依賴配置
```

## 關鍵模塊功能

### `exchanges/mod.rs`
- **ExchangeManager**: 管理多交易所實例和路由
- **HealthMetrics**: 交易所健康度監控
- **RoutingStrategy**: 路由策略實現

### `exchanges/event_hub.rs`
- **ExchangeEventHub**: 事件分發中心
- **ConsumerStats**: 消費者統計（存在型別錯誤）
- 支持背壓控制和慢消費者處理

### `integrations/redis_bridge.rs`
- **RedisBridge**: Redis 數據發布（存在連線重複建立問題）
- 支持訂單簿、交易、Ticker 數據發布

### `engine/complete_oms.rs`
- 完整的訂單管理系統
- 支持多帳戶、風險管理
- 使用 RwLock<HashMap> 結構（存在鎖競爭問題）