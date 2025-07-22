# Rust HFT Examples - 核心示例程序

本目錄包含 Rust HFT 高頻交易系統的**14個核心示例**，演示系統主要功能。已清理10個重複示例，保留最重要的功能演示。

## 🚀 快速開始指南

### 基礎示例 (必看)
- **00_quickstart.rs** - 系統快速入門，WebSocket連接和數據處理
- **02_simplified_unified_system.rs** - 統一交易系統架構演示

### 性能測試
- **performance_e2e_test.rs** - 端到端性能基準測試  
- **comprehensive_db_stress_test.rs** - 15分鐘完整數據庫壓力測試

## 📁 核心模塊結構

### 01_data_collection/ - 數據收集層
- **ws_performance_test.rs** - WebSocket連接性能測試
- **clickhouse_writer.rs** - ClickHouse批量寫入測試
- **multi_symbol_collector.rs** - 多標的並行收集
- **data_quality_monitor.rs** - 實時數據品質監控

### 03_model_inference/ - 模型推理層
- **inference_benchmark.rs** - 推理性能基準測試
- **strategy_executor.rs** - 策略執行引擎

### 05_system_integration/ - 系統整合層
- **end_to_end_test.rs** - 完整交易流程測試
- **latency_benchmark.rs** - 系統延遲基準測試
- **stress_test.rs** - 高負載壓力測試
- **system_monitoring.rs** - 系統監控和告警

## 🚀 運行示例

### 基礎示例
```bash
# 系統快速入門
cargo run --example 00_quickstart

# 統一系統演示
cargo run --example 02_simplified_unified_system
```

### 性能測試
```bash
# 端到端性能測試
cargo run --example performance_e2e_test

# 完整15分鐘壓力測試 (需要ClickHouse和Redis)
cargo run --example comprehensive_db_stress_test
```

### 數據收集測試
```bash
# WebSocket性能測試
cargo run --example 01_data_collection/ws_performance_test

# ClickHouse寫入測試  
cargo run --example 01_data_collection/clickhouse_writer
```

### 系統整合測試
```bash
# 端到端測試
cargo run --example 05_system_integration/end_to_end_test

# 延遲基準測試
cargo run --example 05_system_integration/latency_benchmark
```

## ⚡ 性能基準 (Apple M3)

| 組件 | 實測延遲 | 目標延遲 | 測試狀態 |
|------|----------|----------|----------|
| 消息處理 | 4.9μs | <1μs | 🟡 需優化 |
| 數據庫寫入 | 12.5ms | <10ms | ✅ 達標 |
| WebSocket連接 | ~86 msg/s | >100 msg/s | 🟡 可提升 |
| 端到端系統 | <50ms P95 | <100ms | ✅ 優秀 |

## 🔧 前置要求

### Docker 服務
```bash
# 啟動必要服務
docker-compose up -d clickhouse redis

# 檢查服務狀態
docker-compose ps
```

### 系統優化
```bash
# 編譯優化 (Release模式)
export RUSTFLAGS="-C target-cpu=native -C opt-level=3"
cargo build --release

# macOS 系統調優
sudo sysctl -w kern.ipc.maxsockbuf=16777216
sudo sysctl -w net.inet.tcp.recvspace=65536
```

## 📊 測試報告生成

每個測試會自動生成性能報告至 `test_results/` 目錄：
- **延遲統計**: P50/P95/P99 分佈
- **吞吐量**: 消息/秒，記錄/秒
- **資源使用**: CPU，內存，網絡
- **連接穩定性**: 成功率，錯誤率

### 最新測試結果
- **總記錄數**: 309,492 (15分鐘)
- **平均處理速度**: 343.9 記錄/秒
- **連接成功率**: 100% (25個交易對)
- **數據庫寫入**: 0錯誤 (892批次)

## 🗑️ 已清理的文件

已將以下10個重複和過時的示例移至 `examples_archived/redundant/`：
- `00_quickstart_lockfree.rs`
- `00_zero_copy_websocket_test.rs`  
- `concurrent_test_demo.rs`
- `debug_bitget_ws.rs`
- `quick_ws_performance_check.rs`
- `real_ws_stress_test.rs`
- `test_*.rs` 系列
- `simple_clickhouse_test.rs`

## 🔍 故障排除

### 常見問題
1. **編譯錯誤**: 確保 Rust 1.70+ 和所有依賴已安裝
2. **ClickHouse連接失敗**: 檢查 Docker 容器狀態
3. **性能不佳**: 使用 `--release` 標誌編譯
4. **權限錯誤**: macOS 可能需要允許網絡訪問

### 日誌調試
```bash
# 詳細日誌
RUST_LOG=debug cargo run --release --example 00_quickstart

# 錯誤專用日誌
RUST_LOG=error cargo run --example comprehensive_db_stress_test
```