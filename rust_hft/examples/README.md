# HFT System Examples Guide

本目錄包含 HFT 高頻交易系統的**核心示例程序**，演示系統主要功能和最佳實踐。

## 示例放置原則

### 1. 示例類型分類

#### 🚀 快速開始示例 (頂級目錄)
- **目標**: 5分鐘快速理解系統
- **位置**: `examples/00_quickstart.rs`
- **內容**: 基本 WebSocket 連接和數據處理

#### 📚 教學示例 (按模組分組)
- **目標**: 深入理解特定模組
- **位置**: `examples/{module}/`
- **內容**: 詳細功能演示和配置選項

#### ⚡ 性能測試示例 (專門目錄)
- **目標**: 性能基準和壓力測試
- **位置**: `examples/03_performance_testing/`
- **內容**: 基準測試和系統限制測試

#### 🔧 完整應用示例 (應用目錄)
- **目標**: 完整的端到端應用
- **位置**: `examples/app_layer_demo/`
- **內容**: 可部署的示例應用

### 2. 命名規範

#### 示例文件命名
```
{priority}_{module}_{description}.rs

例子:
00_quickstart.rs              - 快速開始
01_data_collection_demo.rs    - 數據收集示例  
02_strategy_basic_trend.rs    - 基礎趨勢策略
03_performance_benchmark.rs   - 性能基準測試
```

#### 示例目錄命名
```
{priority}_{purpose}/

例子:
01_data_collection/           - 數據收集相關
02_strategy_development/      - 策略開發相關
03_performance_testing/       - 性能測試相關
```

### 3. 示例內容指南

#### 必須包含的元素
```rust
//! # 示例標題
//! 
//! ## 功能描述
//! 簡潔描述示例的主要功能
//!
//! ## 運行方法
//! ```bash
//! cargo run --example example_name
//! ```
//!
//! ## 依賴要求
//! - Redis: 實時緩存
//! - ClickHouse: 數據存儲 (可選)

use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌
    tracing_subscriber::fmt::init();
    
    info!("🚀 啟動 {} 示例", env!("CARGO_BIN_NAME"));
    
    // 示例邏輯
    
    Ok(())
}
```

#### 錯誤處理標準
- 使用 `Result<(), Box<dyn std::error::Error>>` 作為返回類型
- 提供清晰的錯誤信息和恢復建議
- 包含優雅的資源清理

#### 配置管理
- 支持命令行參數覆蓋
- 提供合理的默認值
- 使用環境變量進行敏感配置

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

### Feature 啟用說明

許多示例需要特定的 feature 才能編譯和運行。使用 `--features` 參數啟用所需功能：

#### 交易所適配器
```bash
# Bitget 相關示例
cargo run --example test_bitget_connection --features bitget

# Binance 相關示例  
cargo run --example test_binance_adapter --features binance

# Bybit 相關示例
cargo run --example test_exchange_integration --features bybit

# 多交易所示例
cargo run --example test_all_exchanges --features "bitget,binance,bybit"
```

#### 策略和基礎設施
```bash
# 趨勢策略演示
cargo run --example multi_strategy_demo --features trend-strategy

# 套利策略演示  
cargo run --example arbitrage_strategy_demo --features arbitrage-strategy

# 監控和指標
cargo run --example observability_demo --features metrics

# ClickHouse 數據存儲
cargo run --example comprehensive_db_stress_test --features clickhouse

# Redis 緩存
cargo run --example redis_streams_demo --features redis
```

#### 完整功能集
```bash
# 啟用所有功能的完整演示
cargo run --example s1_adapter_engine_demo --features full

# Live 交易應用 (生產環境)
cargo run -p hft-live --features "bitget,trend-strategy,metrics"
```

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