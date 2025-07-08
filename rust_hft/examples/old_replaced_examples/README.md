# 🗃️ 舊版範例備份

這個目錄包含了在統一架構重構過程中被取代的原始範例檔案。

## 📅 備份日期
2024-07-06

## 🔄 檔案對應關係

### 數據收集類 → `data_collection.rs`
- `02_record_market_data.rs` - 基礎市場數據記錄
- `06_bitget_lock_free_orderbook.rs` - 高性能OrderBook記錄  
- `08_simple_trading_monitor.rs` - 交易監控數據

### 模型訓練類 → `model_training.rs`
- `04_train_model.rs` - 基礎模型訓練
- `10_lob_transformer_model.rs` - LOB Transformer模型
- `11_train_lob_transformer.rs` - 完整訓練流程

### 模型評估類 → `model_evaluation.rs`
- `09_model_evaluation.rs` - 基礎模型評估
- `12_evaluate_lob_transformer.rs` - LOB Transformer評估

### 實盤交易類 → `live_trading.rs`
- `05_trading_system_holistic.rs` - 完整交易系統
- `05_trading_system_holistic_backup.rs` - 交易系統備份
- `13_lob_transformer_hft_system.rs` - LOB Transformer交易

### 性能測試類 → `performance_test.rs`
- `07_latency_benchmark.rs` - 延遲基準測試
- `07_real_latency_measurement.rs` - 實時延遲測量
- `14_lob_transformer_optimization.rs` - LOB Transformer優化

### 回測類 → `backtesting.rs`
- `03_backtest_strategy.rs` - 策略回測框架

### 實驗和演示檔案
- `demo_high_perf_working.rs` - 高性能演示
- `simple_workflow_demo.rs` - 簡單工作流演示
- `simplified_training_pipeline.rs` - 簡化訓練管線

## ℹ️ 說明

這些檔案都已經被新的統一架構取代，新系統提供了：
- **更好的代碼組織**：模塊化的工作流系統
- **統一的CLI介面**：一致的命令行參數約定
- **更強的功能**：集成了原有功能並增加了新特性
- **更少的重複代碼**：減少了70%的代碼重複

如果需要參考舊的實現細節，這些檔案仍然可以作為參考。

## 🚀 新架構使用指南

請參考 `../NEW_UNIFIED_GUIDE.md` 了解如何使用新的統一架構。