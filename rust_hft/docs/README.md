# Rust HFT × Agno AI Trading Platform - 文檔中心

🚀 **統一文檔系統 v3.0** - 完整的開發者和用戶指南

## 📚 文檔結構

### 🏗️ [架構文檔](architecture/)
- [**系統架構總覽**](architecture/SYSTEM_ARCHITECTURE.md) - 雙平面架構設計
- [**多層訂單簿架構**](architecture/MULTI_LEVEL_ORDERBOOK_ARCHITECTURE.md) - 高性能OrderBook設計
- [**性能優化架構**](architecture/PERFORMANCE_ARCHITECTURE.md) - SIMD、CPU親和性、內存優化
- [**Binance 低延遲 Market Data 藍圖**](architecture/BINANCE_LOW_LATENCY_MARKET_DATA_PLAN.md) - 行情接收、本地訂單簿、特徵與信號路線圖
- [**Binance MD 執行計畫**](architecture/BINANCE_MD_REFACTOR_EXECUTION_PLAN.md) - fast-lane 實作、驗證命令、剩餘風險
- [**HFT Production Readiness Checklist**](architecture/HFT_PRODUCTION_READINESS_CHECKLIST.md) - low latency、交易正確性、安全觀測三條主線的上線檢查表

### 📖 [用戶指南](guides/)
- [**快速開始**](guides/QUICK_START.md) - 10分鐘上手指南
- [**安裝配置**](guides/INSTALLATION.md) - 詳細安裝說明
- [**Rust 構建與發布**](guides/RUST_BUILD_RELEASE.md) - 快速迭代、CI 快取、精簡 release 產物
- [**交易策略開發**](guides/TRADING_STRATEGIES.md) - ML策略開發教程
- [**回測與評估**](guides/BACKTESTING.md) - 策略回測框架
- [**部署指南**](guides/DEPLOYMENT.md) - 生產環境部署

### 🔧 [API文檔](api/)
- [**Rust API**](api/RUST_API.md) - 完整Rust接口文檔
- [**Python API**](api/PYTHON_API.md) - PyO3綁定接口
- [**配置選項**](api/CONFIGURATION.md) - 所有配置參數
- [**CLI命令**](api/CLI_REFERENCE.md) - 命令行工具使用
- [**Schema v2 配置指南**](config/SCHEMA_V2.md) - 宣告式系統設定與 Instrument Catalog
- [**Catalog 管理指南**](config/CATALOG_GUIDE.md) - Instrument/Venue catalog 的維護與載入流程
- [**Validation Modes**](config/VALIDATION_MODES.md) - 寬鬆 / 嚴格模式的行為與建議

### 📊 [測試報告](reports/)
- [**性能基準**](reports/PERFORMANCE_BENCHMARKS.md) - 延遲和吞吐量測試
- [**系統測試**](reports/SYSTEM_TEST_RESULTS.md) - 集成測試結果
- [**風險優化**](reports/RISK_OPTIMIZATION.md) - 風控系統評估
- [**壓力測試**](reports/STRESS_TEST_RESULTS.md) - 高負載測試報告

## 🎯 核心特性概述

### ⚡ 超低延遲交易 (<1μs)
```rust
// 零分配決策路徑
let signal = strategy.predict_zero_alloc(&orderbook)?;
let order = executor.execute_immediate(&signal)?;
```

### 🧠 智能ML引擎 (7層模型)
```python
# 多層模型架構
agents = [
    SupervisorAgent(),    # 全局調度
    TrainAgent(),        # 模型訓練  
    TradeAgent(),        # 策略執行
    MonitorAgent(),      # 系統監控
    RiskAgent(),         # 風險控制
    ConfigAgent(),       # 配置管理
    ChatAgent()          # 用戶交互
]
```

### 📈 實時特徵存儲 (redb)
```rust
// 超低延遲存儲
let store = FeatureStore::new(config)?;
store.store_features_batch(&features)?;  // <10μs
let historical = store.query_range(start, end)?;  // <50μs
```

## 📦 項目組件

| 組件 | 延遲目標 | 實際性能 | 說明 |
|------|----------|----------|------|
| **訂單簿更新** | <100ns | ~80ns | Lock-free結構 |
| **ML推理** | <1μs | ~800ns | SIMD優化 |
| **特徵提取** | <5μs | ~3μs | 向量化計算 |
| **風險檢查** | <500ns | ~300ns | 預計算規則 |
| **訂單執行** | <2μs | ~1.5μs | 直接API調用 |

## 🚀 快速開始

### 1. 環境設置
```bash
git clone https://github.com/your-repo/rust_hft.git
cd rust_hft

# 安裝依賴
cargo build --release --features="gpu,simd"

# 驗證安裝
./scripts/verify_project_structure.sh
```

### 2. 運行示例
```bash
# 快速示例 (5分鐘)
cargo run --example 00_quickstart

# 完整系統測試
cargo run --example comprehensive_db_stress_test
```

### 3. Binance Market Data Fast Lane
```bash
# 接 Binance BTCUSDT depth/bookTicker，橋接 REST snapshot，寫入 replay NDJSON
cargo run -p hft-binance-md --locked -- live \
  --symbol BTCUSDT \
  --max-messages 100 \
  --replay-out /tmp/binance-md.ndjson

# 用同一份 replay 文件重放並檢查 feature/signal parity
cargo run -p hft-binance-md --locked -- replay \
  --replay-in /tmp/binance-md.ndjson

# 基於 replay 文件做 paper evaluation，不啟用真實下單
cargo run -p hft-binance-md --locked -- paper \
  --replay-in /tmp/binance-md.ndjson
```

這條 fast lane 的當前邊界是：Binance WebSocket / REST snapshot -> 本地訂單簿 -> OBI/microprice/spread/staleness -> expiring signal -> replay/paper。真實下單、OMS、風控與跨交易所套利仍是後續階段。

### 4. Bitget Market Data Adapter
```bash
# 驗證 Bitget adapter 的 books/trade parser、mock WS、重連與心跳
cargo test -p hft-data-adapter-bitget --locked -- --test-threads=1

# 跑 Bitget borrowed parser hot-path benchmark
cargo bench -p hft-engine --bench bitget_md_hotpath --locked

# 真實連 Bitget public WS，測本地 receive->parse->event conversion p99
cargo run -p hft-data-adapter-bitget --example live_p99 --release -- \
  --symbol BTCUSDT \
  --depth-channel books1 \
  --max-messages 500 \
  --max-runtime-secs 30

# 真實連 Bitget public WS，拆出 ws gap / queue wait / parse / convert / engine p99
cargo run -p hft-data-adapter-bitget --example latency_audit --release -- \
  --symbol BTCUSDT \
  --depth-channel books1 \
  --queue-capacity 1024 \
  --max-messages 500 \
  --max-runtime-secs 30

# 低延遲模式：engine thread busy-poll raw queue，會占用一個核心
cargo run -p hft-data-adapter-bitget --example latency_audit --release -- \
  --symbol BTCUSDT \
  --depth-channel books1 \
  --queue-capacity 1024 \
  --max-messages 500 \
  --max-runtime-secs 60 \
  --busy-poll
```

Bitget adapter 的行情接口按官方 v2 WebSocket 行為處理：公共端點使用 `wss://ws.bitget.com/v2/ws/public`，深度 channel 使用 `books/books1/books5/books15`，增量模式使用 `books`；心跳使用文本 `"ping"`/`"pong"`，不是只依賴 WebSocket ping frame。books/trade 熱路徑使用 borrowed typed JSON parser，非標準格式才回退 legacy `serde_json::Value` path。

### 5. 開始交易
```bash
# 啟動實時交易
cargo run --release --bin hft_trader -- \
  --symbol BTCUSDT \
  --strategy ml_lstm \
  --dry-run false
```

## 🔧 配置示例

### 高性能配置
```yaml
# config/production.yaml
system:
  cpu_isolation: true
  memory_prefaulting: true
  simd_acceleration: true

trading:
  max_position_size: "10000.0"
  risk_limit: "1000.0"
  latency_target_us: 1

ml:
  model_path: "models/hft_v2.onnx"
  inference_batch_size: 1
  feature_window: 50
```

## 📊 性能監控

### Prometheus 指標啟用（新版）
```bash
# 僅收集指標（不開 HTTP）
cargo run -p hft-paper --features "metrics"

# 收集指標 + 啟用 HTTP 暴露（Axum）
cargo run -p hft-paper --features "metrics,hft-infra-metrics/http-server"
```

### 實時指標
- **延遲監控**: P50/P95/P99延遲分佈
- **吞吐量**: 每秒處理訂單數
- **準確率**: ML模型預測精度
- **風險指標**: 實時VaR、最大回撤

### 監控面板
```bash
# 啟動Grafana監控
docker-compose up -d grafana prometheus
# 訪問: http://localhost:3000
```

## 🧪 測試框架

### 單元測試 (75個測試)
```bash
cargo test --lib         # 運行單元測試
cargo test --test '*'    # 運行集成測試
cargo bench             # 性能基準測試
```

### 壓力測試
```bash
./scripts/run_full_test_suite.sh    # 完整測試套件
./scripts/run_db_test_compatible.sh # 數據庫壓力測試
```

## 🔒 安全審計

### 自動化安全檢查
```bash
cargo audit              # 依賴漏洞掃描
./scripts/security_check.sh  # 完整安全檢查
```

### GitHub Actions CI/CD
- ✅ 自動測試
- ✅ 安全掃描  
- ✅ 性能回歸檢測
- ✅ 多平台構建

## 🤝 貢獻指南

1. **Fork** 項目並創建功能分支
2. **開發** 並確保測試通過
3. **文檔** 更新相關文檔
4. **提交** Pull Request

```bash
git checkout -b feature/amazing-feature
git commit -m 'Add amazing feature'
git push origin feature/amazing-feature
```

## 📞 支援與幫助

- **文檔問題**: 查看 [Issues](https://github.com/your-repo/rust_hft/issues)
- **技術支援**: 聯繫開發團隊
- **社區討論**: [Discussions](https://github.com/your-repo/rust_hft/discussions)

---

## 📄 許可證

MIT License - 詳見 [LICENSE](../LICENSE) 文件

**免責聲明**: 本軟體僅供學習和研究使用，交易有風險，投資需謹慎。

---

*最後更新: 2026-04-28 | 版本: v3.0*
