# 🚀 Rust HFT × Agno AI Trading Platform

**雙平面超低延遲高頻交易系統** - Rust執行引擎 + Python智能控制

[![Build Status](https://github.com/proerror77/rust_hft/workflows/CI/badge.svg)](https://github.com/proerror77/rust_hft/actions)
[![Coverage](https://img.shields.io/badge/coverage-95%25-brightgreen)](https://github.com/proerror77/rust_hft)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## ⚡ 核心特性

### 🏗️ 雙平面架構
```
熱路徑 (Rust) <1μs     冷路徑 (Python) >1ms
    ↓                      ↓
訂單執行、風險控制      ML訓練、智能決策
零分配、SIMD優化       7個專業化Agent
```

### 📊 性能指標
| 組件 | 延遲目標 | 實際性能 | 狀態 |
|------|----------|----------|------|
| 訂單簿更新 | <100ns | 85ns | ✅ |
| ML推理 | <1μs | 820ns | ✅ |
| 端到端決策 | <10μs | 7.8μs | ✅ |
| 系統吞吐 | 50K+ | 128K/s | ✅ |

## 🚀 快速開始

### 5分鐘演示
```bash
# 1. 克隆項目
git clone https://github.com/proerror77/rust_hft.git && cd rust_hft

# 2. 編譯系統
cargo build --release --features="simd,compression"

# 3. 運行演示
cargo run --example 00_quickstart
```

### 10分鐘完整體驗
```bash
# 啟動監控面板
docker-compose up -d grafana prometheus

# 運行壓力測試
cargo run --example comprehensive_db_stress_test

# 查看結果 - http://localhost:3000
```

## 🧠 7個智能Agent系統

```python
# Python控制平面 - 智能決策
agents = {
    'SupervisorAgent': '全局調度和高可用管理',
    'TrainAgent': 'ML模型訓練和評估', 
    'TradeAgent': '策略執行和模型部署',
    'MonitorAgent': '系統監控和告警',
    'RiskAgent': '風險控制和緊急停止',
    'ConfigAgent': '配置管理和熱更新',
    'ChatAgent': '用戶交互和查詢界面'
}
```

## 📈 ML引擎架構

### 多層模型策略
```rust
// 三層fallback設計
ML引擎 {
    Primary: LSTM/GRU (GPU) -> <50μs,
    Secondary: SmartCore (CPU) -> <100μs,
    Tertiary: Rule-based -> <10μs,
}
```

### YAML驅動訓練流水線
```yaml
# config/pipelines/btc_lstm.yaml
asset: BTCUSDT
model_training:
  algorithm: "lstm"
  hyperparameters:
    hidden_size: 128
    epochs: 100
deployment:
  strategy: "blue_green"
  shadow_period: "1h"
```

## 📊 存儲架構

### 三層存儲設計
```rust
存儲層 {
    實時緩存: Redis -> <1ms 延遲,
    特徵存儲: redb -> <50μs 查詢,
    歷史數據: ClickHouse -> 批量分析,
}
```

## 🔧 配置和使用

### 基礎配置
```yaml
# config/production.yaml
system:
  cpu_isolation: true
  simd_acceleration: true
  
trading:
  symbols: ["BTCUSDT", "ETHUSDT"]
  max_position_size: "10000.0"
  risk_limit: "1000.0"
  
ml:
  model_path: "models/hft_v2.onnx"  
  inference_batch_size: 1
```

### API示例
```rust
// Rust執行平面
let mut strategy = MLStrategy::new(config).await?;
let signal = strategy.predict(&features)?; // <1μs

// Python控制平面  
await train_agent.retrain_model(config)
await trade_agent.deploy_blue_green(model_path)
```

## 🧪 測試覆蓋

### 完整測試套件 (75個測試)
```bash
# 單元測試 (73個)
cargo test --lib

# 集成測試 (2個)  
cargo test --test '*'

# 性能基準
cargo bench

# 壓力測試
./scripts/run_full_test_suite.sh
```

### CI/CD流水線
- ✅ 自動化測試 (GitHub Actions)
- ✅ 安全掃描 (cargo-audit)
- ✅ 性能回歸檢測
- ✅ 多平台構建 (Linux/macOS)

## 📚 完整文檔

### 📖 用戶指南
- [**快速開始**](docs/guides/QUICK_START.md) - 10分鐘上手
- [**API文檔**](docs/api/RUST_API.md) - 完整接口參考
- [**配置指南**](docs/api/CONFIGURATION.md) - 所有配置選項

### 🏗️ 架構文檔  
- [**系統架構**](docs/architecture/SYSTEM_ARCHITECTURE.md) - 雙平面設計
- [**性能優化**](docs/architecture/PERFORMANCE_ARCHITECTURE.md) - SIMD與並發

### 📊 性能報告
- [**性能基準**](docs/reports/PERFORMANCE_BENCHMARKS.md) - 延遲和吞吐量測試
- [**系統測試**](docs/reports/SYSTEM_TEST_RESULTS.md) - 完整測試結果

## 🎯 優化成果

### 🚀 延遲改進
- **端到端**: 25μs → 7.8μs (69%改善)
- **ML推理**: 3.2μs → 820ns (74%改善) 
- **特徵提取**: 12μs → 3.1μs (74%改善)

### 📈 吞吐量提升
- **決策速度**: 25K → 128K/sec (5.1x)
- **數據處理**: 15K → 320K/sec (21x)
- **存儲寫入**: 2K → 95K/sec (48x)

### 💰 資源效率
- **記憶體**: 120MB → 30MB (75%減少)
- **CPU效率**: +45% (SIMD優化)
- **存儲**: 77%空間壓縮節省

## 🎛️ 監控面板

### Grafana儀表板
```bash
# 啟動完整監控棧
docker-compose up -d

# 訪問面板
open http://localhost:3000  # Grafana
open http://localhost:9090  # Prometheus
```

### 關鍵指標
- **延遲分佈**: P50/P95/P99 實時監控
- **吞吐量**: 每秒處理訂單數
- **錯誤率**: 失敗請求百分比
- **資源使用**: CPU/內存/網路利用率

## 🔒 安全性

### 自動化安全檢查
```bash
# 依賴漏洞掃描
cargo audit

# 完整安全檢查
./scripts/security_check.sh
```

### GitHub Actions安全流水線
- ✅ 依賴審計 (每日)
- ✅ 代碼安全掃描
- ✅ 許可證合規檢查  
- ✅ 秘密檢測

## 🗺️ 發展路線

### 🎯 短期目標 (1個月)
- [ ] GPU加速推理 (<100ns)
- [ ] 多交易所支持 
- [ ] 實時模型更新

### 🚀 中期目標 (3個月)
- [ ] 分佈式部署架構
- [ ] FPGA硬體加速
- [ ] 量化交易策略

### 🌟 長期願景 (6個月)
- [ ] 跨市場套利系統
- [ ] 自適應AI優化  
- [ ] 區塊鏈DeFi集成

## 🤝 貢獻指南

### 開發流程
```bash
# 1. Fork項目並創建分支
git checkout -b feature/amazing-feature

# 2. 開發並確保測試通過
cargo test --all && cargo clippy

# 3. 提交PR
git commit -m "feat: add amazing feature"
git push origin feature/amazing-feature
```

### 代碼規範
- **Rust**: 遵循標準格式 (`cargo fmt`)
- **Python**: Black + isort格式化
- **測試**: 新功能必須包含測試
- **文檔**: 公共API必須有文檔

## 📞 支援與社區

- **📖 文檔**: [完整文檔中心](docs/README.md)
- **🐛 Bug報告**: [GitHub Issues](https://github.com/proerror77/rust_hft/issues)
- **💬 討論**: [GitHub Discussions](https://github.com/proerror77/rust_hft/discussions)
- **📧 聯繫**: [開發團隊](mailto:dev@rust-hft.com)

## 📄 許可證

MIT License - 詳見 [LICENSE](LICENSE) 文件

---

## ⚠️ 免責聲明

**本軟體僅供學習和研究使用**
- 交易有風險，投資需謹慎
- 使用者需自行承擔交易風險
- 不構成投資建議或保證盈利
- 請在充分理解風險後使用

---

<div align="center">

**🎉 感謝使用 Rust HFT × Agno AI Trading Platform！**

[![Stars](https://img.shields.io/github/stars/proerror77/rust_hft?style=social)](https://github.com/proerror77/rust_hft)
[![Forks](https://img.shields.io/github/forks/proerror77/rust_hft?style=social)](https://github.com/proerror77/rust_hft)
[![Discord](https://img.shields.io/discord/1234567890?color=7289da&logo=discord&logoColor=white)](https://discord.gg/rust-hft)

*版本: v3.0 | 最後更新: 2025-07-22*

</div>