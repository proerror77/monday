# Rust HFT System - 項目總覽

## 項目目的
高頻交易（HFT）系統，採用三層架構：
- **L1 Rust核心引擎**: < 25μs 延遲的交易執行
- **L2 Ops監控Agent**: 7x24 常駐監控服務  
- **L3 ML訓練Agent**: GPU 批次訓練任務

## 技術棧
### Rust 核心部分 (`rust_hft/`)
- **異步運行時**: Tokio 1.40
- **WebSocket**: tokio-tungstenite + futures-util
- **JSON解析**: simd-json (比 serde_json 更快)
- **無鎖數據結構**: crossbeam-channel, dashmap, lockfree
- **高精度數值**: rust_decimal, ordered-float
- **性能監控**: metrics, tracing, prometheus
- **Redis客戶端**: redis 0.27 with async support
- **數據庫**: ClickHouse 客戶端
- **系統優化**: core_affinity, libc

### Python Agent 部分
- **Framework**: Agno v2.0 (Workflows 管理)
- **ML**: PyTorch, scikit-learn  
- **監控**: Prometheus, Grafana

## 核心目標
- **延遲**: P99 執行延遲 < 25μs
- **吞吐**: > 100k ops/sec 訂單處理
- **可用性**: 99.99% 核心引擎運行時間
- **自動化**: 每日模型訓練和部署