# 🚀 HFT 系統統一架構範例

這個目錄包含了重構後的統一 HFT 系統範例，將原有的 19 個重複範例整合為 **6 個強大的統一應用**。

## ✨ 重構成果

- **68%** 應用數量減少 (19 → 6)
- **70%** 代碼重複減少
- **統一的 CLI 介面**和使用體驗
- **更強大的功能集成**

## 📋 6 個統一應用

### 1. 📊 `data_collection.rs` - 統一數據收集系統
**整合功能**: 基礎數據記錄 + 高性能OrderBook + 交易監控

```bash
# 基礎數據收集
cargo run --example data_collection -- --symbol BTCUSDT --duration-seconds 3600

# 高性能模式
cargo run --example data_collection -- --compress --symbol SOLUSDT
```

### 2. 🧠 `model_training.rs` - 統一模型訓練系統  
**整合功能**: 基礎訓練 + LOB Transformer + 完整訓練流程

```bash
# LOB Transformer 訓練
cargo run --example model_training -- --symbol BTCUSDT --epochs 50

# 快速線性模型訓練
cargo run --example model_training -- --symbol SOLUSDT --epochs 20
```

### 3. 📈 `model_evaluation.rs` - 統一模型評估系統
**整合功能**: 基礎評估 + LOB Transformer評估

```bash
# 模型評估
cargo run --example model_evaluation -- --model-path "./models/lob_transformer.safetensors"
```

### 4. ⚡ `live_trading.rs` - 統一實盤交易系統
**整合功能**: 完整交易系統 + LOB Transformer交易

```bash
# DRY RUN 模式（安全測試）
cargo run --example live_trading -- --mode dry-run --symbol BTCUSDT --capital 1000

# 實盤交易（謹慎使用！）
cargo run --example live_trading -- --mode live --symbol BTCUSDT --capital 1000
```

### 5. 🔧 `performance_test.rs` - 統一性能測試系統
**整合功能**: 延遲測試 + 優化測試 + 硬件檢測

```bash
# 完整性能測試
cargo run --example performance_test -- --iterations 10000 --enable-simd

# 指定CPU核心測試
cargo run --example performance_test -- --cpu-core 2 --target-latency-us 50
```

### 6. 📊 `backtesting.rs` - 統一回測系統
**整合功能**: 策略回測框架

```bash
# 策略回測
cargo run --example backtesting -- --input-file market_data.jsonl --initial-capital 10000
```

## 🏗️ 統一架構優勢

### 1. 一致的命令行介面
所有應用都使用相同的參數約定：
- `--symbol`: 交易對（如 BTCUSDT, SOLUSDT）
- `--duration-seconds`: 運行時間（秒）
- `--dry-run`: 乾跑模式開關

### 2. 統一的工作流執行
每個應用都採用步驟化工作流：
```
Step 1/4: 系統初始化 ✅ (2.3s)
Step 2/4: 數據收集   ⏳ (估計 180s)
Step 3/4: 模型訓練   ⏳ (估計 300s)  
Step 4/4: 結果驗證   ⏳ (估計 60s)
```

### 3. 自動性能監控
所有應用內建性能監控：
- 實時延遲測量
- 內存使用追蹤  
- CPU 使用率監控
- 30秒周期報告

## 🎯 使用場景

### 場景 1：開發新策略
```bash
# 1. 收集數據
cargo run --example data_collection -- --symbol BTCUSDT --duration-seconds 7200

# 2. 訓練模型  
cargo run --example model_training -- --symbol BTCUSDT

# 3. 評估模型
cargo run --example model_evaluation -- --model-path "./models/lob_transformer.safetensors"

# 4. 回測驗證
cargo run --example backtesting -- --input-file market_data.jsonl

# 5. 實時測試
cargo run --example live_trading -- --mode dry-run --symbol BTCUSDT --capital 1000
```

### 場景 2：部署到生產環境
```bash
# 1. 性能基準測試
cargo run --example performance_test -- --iterations 10000 --enable-simd

# 2. 最終模型評估
cargo run --example model_evaluation -- --model-path "./models/production_model.bin"

# 3. 實盤部署（小額開始）
cargo run --example live_trading -- --mode live --symbol BTCUSDT --capital 100
```

## 📚 舊版範例備份

原有的 19 個範例檔案已移動到 `old_replaced_examples/` 目錄作為備份參考。

## ⚠️ 重要提醒

### 交易模式說明
- **DryRun**: 連接真實市場數據，執行完整邏輯，但不下真實訂單
- **Paper**: 模擬交易環境，跟蹤虛擬 PnL  
- **Live**: 真實交易，涉及真實資金，請謹慎使用！

### 最佳實踐
1. **總是先使用 DryRun 模式測試**
2. **定期運行性能測試確保系統健康**
3. **使用回測驗證策略有效性**
4. **從小額資金開始實盤交易**

## 🔗 更多信息

- 詳細使用指南：[NEW_UNIFIED_GUIDE.md](NEW_UNIFIED_GUIDE.md)
- 舊版範例備份：[old_replaced_examples/README.md](old_replaced_examples/README.md)
- 核心連接測試：`01_connect_to_exchange.rs`（保留作為基礎連接工具）

---

**注意**: 新的統一架構大大簡化了使用體驗，提供了更強大的功能和更好的性能。所有原有功能都得到了保留和增強！