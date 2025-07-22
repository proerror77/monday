# 🚀 Rust HFT 快速開始指南

⚡ **10分鐘上手** - 從零開始運行高頻交易系統

## 📋 前置要求

### 系統要求
```bash
# 最低配置
CPU: 4核心 2.0GHz+
RAM: 8GB+
OS: Linux/macOS/Windows

# 推薦配置  
CPU: 8核心 3.0GHz+ (支持AVX2)
RAM: 16GB+
SSD: 100GB+ 可用空間
網路: 低延遲網路連接
```

### 軟體依賴
```bash
# Rust 工具鏈 (必需)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable

# 可選依賴
sudo apt install build-essential pkg-config libssl-dev  # Linux
# 或
brew install pkg-config openssl                        # macOS
```

## 🔧 快速安裝

### 1. 克隆項目
```bash
git clone https://github.com/proerror77/rust_hft.git
cd rust_hft
```

### 2. 編譯項目
```bash
# 基礎編譯 (2-3分鐘)
cargo build --release

# 完整編譯 (啟用所有優化)
cargo build --release --features="gpu,simd,compression"
```

### 3. 驗證安裝
```bash
# 運行項目結構驗證
./scripts/verify_project_structure.sh

# 快速健康檢查
cargo test --lib core::app_runner::tests::test_app_runner_creation
```

## ⚡ 5分鐘示例

### 示例 1: 基礎訂單簿處理
```bash
# 運行最基礎的訂單簿示例
cargo run --release --example 00_quickstart

# 預期輸出:
# ✅ OrderBook initialized for BTCUSDT  
# ✅ WebSocket connected to Bitget
# ✅ Received 100 orderbook updates
# 📊 Average latency: 85μs
```

### 示例 2: 實時數據收集
```bash
# 啟動實時數據收集 (30秒測試)
cargo run --release --example 01_data_collection/multi_symbol_collector

# 實時輸出:
# 🔄 Collecting: BTCUSDT, ETHUSDT, SOLUSDT
# 📈 Updates/sec: 1,250
# 💾 Stored features: 15,000
```

### 示例 3: ML策略執行
```bash
# 運行ML策略示例 (模擬模式)
cargo run --release --example 03_model_inference/strategy_executor

# 輸出:
# 🧠 Model loaded: CPU Linear (17 features)  
# 📊 Generated 50 trading signals
# 💰 P&L: +$123.45 (模擬)
```

## 🎯 核心概念

### 1. 訂單簿 (OrderBook)
```rust
// 創建高性能訂單簿
use rust_hft::core::orderbook::OrderBook;

let mut orderbook = OrderBook::new("BTCUSDT".to_string());

// 更新買賣單 (<100ns延遲)
orderbook.update_bid(price("45000.0"), quantity("1.5"))?;
orderbook.update_ask(price("45001.0"), quantity("1.2"))?;

// 獲取最佳價格
let (best_bid, best_ask) = orderbook.get_best_prices();
```

### 2. 特徵提取 (Features)
```rust
// 實時特徵計算
use rust_hft::ml::FeatureExtractor;

let mut extractor = FeatureExtractor::new(50); // 50個歷史窗口

// 提取技術指標 (<5μs延遲)
let features = extractor.extract_features(
    &orderbook, 
    sequence_id, 
    timestamp
)?;

// 特徵包括: spread, OBI, momentum, volatility等
```

### 3. ML推理 (Inference)
```rust
// 零分配ML推理
use rust_hft::engine::strategy::MLStrategy;

let strategy = MLStrategy::new(config)?;

// 超低延遲預測 (<1μs延遲)  
let signal = strategy.predict_zero_alloc(&features)?;
match signal.action {
    TradingAction::Buy => println!("買入信號: 概率 {:.2}%", signal.probability * 100.0),
    TradingAction::Sell => println!("賣出信號: 概率 {:.2}%", signal.probability * 100.0),
    TradingAction::Hold => println!("持有"),
}
```

## 📊 配置說明

### 基礎配置文件
```yaml
# config/quick_start.yaml
system:
  # 性能設置
  cpu_isolation: false        # 初學者設為 false
  memory_prefaulting: true    # 啟用內存預分配
  simd_acceleration: true     # 啟用SIMD加速

trading:  
  # 交易參數
  symbols: ["BTCUSDT"]        # 交易對
  max_position_size: "100.0"  # 最大倉位
  stop_loss: "0.02"          # 2% 止損
  
data_collection:
  # 數據收集
  websocket_url: "wss://ws.bitget.com/v2/ws/public"
  buffer_size: 1000
  compression: true

ml:
  # ML設置  
  model_type: "linear"        # 簡單線性模型
  feature_window: 20          # 20個歷史點
  inference_batch_size: 1     # 實時推理
```

### 高級配置 (生產環境)
```yaml
# config/production.yaml  
system:
  cpu_isolation: true         # 核心隔離
  isolated_cores: [2, 3, 4, 5]  # 專用核心
  memory_prefaulting: true
  simd_acceleration: true
  cache_optimization: true

trading:
  symbols: ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
  max_position_size: "10000.0"
  risk_limit: "1000.0"
  latency_target_us: 1        # 1μs延遲目標

ml:
  model_path: "models/hft_v2.onnx"
  model_type: "lstm"          # 高級LSTM模型  
  feature_window: 50
  gpu_acceleration: true
```

## 🧪 測試運行

### 1. 單元測試
```bash
# 運行所有測試 (75個測試)
cargo test --lib

# 預期結果:
# test result: ok. 73 passed; 0 failed; 0 ignored
```

### 2. 性能測試
```bash  
# 延遲基準測試
cargo bench latency_benchmarks

# 預期結果:
# orderbook_update     time: [82.5 ns 85.2 ns 88.1 ns]
# feature_extraction   time: [2.8 μs 3.1 μs 3.4 μs]  
# ml_inference         time: [780 ns 820 ns 860 ns]
```

### 3. 集成測試
```bash
# 數據庫壓力測試 (5分鐘)
cargo run --release --example comprehensive_db_stress_test

# WebSocket連接測試
cargo run --release --example 00_zero_copy_websocket_test
```

## 📈 監控面板

### 啟動監控系統
```bash
# 啟動Prometheus + Grafana
docker-compose up -d

# 訪問面板
open http://localhost:3000    # Grafana
open http://localhost:9090    # Prometheus
```

### 關鍵指標
- **延遲分佈**: P50/P95/P99 響應時間
- **吞吐量**: 每秒處理的訂單數
- **錯誤率**: 失敗請求百分比  
- **CPU使用**: 各核心負載情況
- **內存使用**: 堆分配和池使用情況

## 🎯 下一步學習

### 初級用戶 (1-2週)
1. [**系統架構**](../architecture/SYSTEM_ARCHITECTURE.md) - 了解整體設計
2. [**API文檔**](../api/RUST_API.md) - 學習核心接口
3. [**配置選項**](../api/CONFIGURATION.md) - 掌握配置技巧

### 中級用戶 (2-4週)  
1. [**交易策略開發**](TRADING_STRATEGIES.md) - 開發自定義策略
2. [**回測框架**](BACKTESTING.md) - 策略回測評估
3. [**性能優化**](../architecture/PERFORMANCE_ARCHITECTURE.md) - 深度性能調優

### 高級用戶 (1-2月)
1. [**部署指南**](DEPLOYMENT.md) - 生產環境部署
2. [**多交易所支持**](MULTI_EXCHANGE.md) - 擴展交易所
3. [**系統監控**](MONITORING.md) - 完整監控體系

## ❓ 常見問題

### Q: 編譯失敗怎麼辦？
```bash
# 更新Rust版本
rustup update stable

# 清理重建
cargo clean && cargo build --release

# 檢查依賴
cargo tree | grep -i error
```

### Q: 延遲過高如何優化？  
```bash
# 啟用CPU隔離
sudo isolcpus=2,3,4,5

# 運行性能測試
cargo run --release --example latency_benchmark

# 查看系統負載
htop
```

### Q: ML模型不準確？
```bash
# 使用更多歷史數據訓練
cargo run --release --bin train_model -- --epochs 200

# 調整特徵窗口大小
# config.yaml: feature_window: 100

# 切換到更高級模型
# model_type: "lstm" # 替換 "linear"
```

## 🆘 獲取幫助

- **GitHub Issues**: [報告問題](https://github.com/proerror77/rust_hft/issues)
- **討論區**: [技術討論](https://github.com/proerror77/rust_hft/discussions)  
- **文檔**: [完整文檔](../README.md)
- **示例代碼**: [examples/目錄](../../examples/)

---

**🎉 恭喜！** 您已經完成快速開始指南。現在可以開始構建您的高頻交易策略了！

*更新時間: 2025-07-22*