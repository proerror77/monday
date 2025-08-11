# HFT System Test Suite

全面的高頻交易系統測試套件，確保系統在生產環境中的可靠性、性能和正確性。

## 測試架構

```
tests/
├── common/              # 共用測試工具和輔助函數
│   └── mod.rs          # 測試助手、模擬對象、性能追蹤器
├── unit/               # 單元測試
│   ├── multi_exchange_manager_test.rs  # 多交易所管理器測試
│   ├── orderbook_comprehensive_test.rs # 訂單簿全面測試
│   ├── trading_strategies_test.rs      # 交易策略測試
│   └── unified_engine_test.rs         # 統一引擎測試
├── integration/        # 集成測試
│   ├── end_to_end_system_test.rs      # 端到端系統測試
│   └── performance_benchmarks.rs      # 性能基準測試
└── lib.rs             # 測試套件主入口
```

## 測試類型

### 1. 單元測試 (Unit Tests)

針對個別模組的詳細功能測試：

- **MultiExchangeManager**: 連接器初始化、配置管理、錯誤處理
- **OrderBook**: 訂單簿更新、計算邏輯、數據完整性
- **Trading Strategies**: 信號生成、狀態管理、性能評估  
- **UnifiedEngine**: 引擎生命週期、風險管理集成、執行管理

### 2. 集成測試 (Integration Tests)

跨模組交互和系統級功能測試：

- **端到端數據流**: WebSocket → OrderBook → Strategy → Execution
- **多交易所協調**: 並發數據處理、消息同步
- **錯誤恢復**: 故障注入、自動重連、系統恢復能力

### 3. 性能基準測試 (Performance Benchmarks)

關鍵性能指標驗證（符合 CLAUDE.md 要求）：

- **延遲要求**: P99 ≤ 25μs（訂單簿更新到交易執行）
- **吞吐量**: ≥100,000 操作/秒
- **內存效率**: ≤10MB 每交易對
- **並發處理**: 支持100+交易對同時處理

## 執行測試

### 快速開始

```bash
# 執行全套測試
./scripts/run_test_suite.sh

# 只執行單元測試
./scripts/run_test_suite.sh --unit-only

# 只執行性能測試（發布模式）
./scripts/run_test_suite.sh --performance-only --release

# 包含壓力測試（資源密集）
./scripts/run_test_suite.sh --stress
```

### 使用 Cargo 直接執行

```bash
# 執行所有測試
cargo test

# 執行特定測試模組
cargo test multi_exchange_manager_test

# 執行性能測試（發布模式）
cargo test --release performance_benchmarks

# 詳細輸出
cargo test -- --nocapture
```

## 性能要求驗證

測試套件會自動驗證以下 CLAUDE.md 中定義的性能要求：

| 指標 | 要求 | 測試驗證 |
|------|------|----------|
| P99 延遲 | ≤ 25μs | ✅ `test_orderbook_update_latency_benchmark` |
| 吞吐量 | ≥ 100k ops/sec | ✅ `test_concurrent_orderbook_performance` |
| 錯誤率 | < 1% | ✅ `test_error_recovery_and_resilience` |
| 內存使用 | ≤ 10MB/symbol | ✅ `test_memory_usage_efficiency` |

## 測試環境設置

### 必需依賴

```bash
# Redis (用於集成測試)
brew install redis
redis-server

# ClickHouse (用於數據存儲測試) - 可選
docker run -d --name clickhouse-server -p 8123:8123 clickhouse/clickhouse-server
```

### 環境變量

```bash
export RUST_LOG=info           # 日誌級別
export RUST_BACKTRACE=1        # 錯誤堆棧追蹤
export REDIS_URL=redis://localhost:6379
```

## 測試數據和模擬

### 模擬交易所數據

測試套件包含真實的市場數據模擬：

- **Binance**: 5檔深度，每100ms更新
- **Bitget**: 15檔深度，每100-150ms更新  
- **Bybit**: 1檔深度，即時交易數據

### 性能測試場景

- **高頻更新**: 100,000+ 訂單簿更新
- **並發處理**: 100個交易對同時處理
- **極端負載**: 5,000 消息/秒持續處理
- **錯誤注入**: 網路斷線、格式錯誤、序列跳躍

## 測試報告和分析

### 自動生成報告

```bash
# 生成詳細性能報告
./scripts/run_test_suite.sh --performance-only --verbose > performance_report.txt

# 基準測試比較
cargo bench
```

### 關鍵指標監控

測試執行時會自動追蹤：

- ✅ **延遲分佈**: 平均、P50、P95、P99
- ✅ **吞吐量**: 操作數/秒、消息數/秒
- ✅ **錯誤率**: 成功率、失敗類型分佈
- ✅ **資源使用**: 內存佔用、CPU利用率

## 持續集成 (CI)

測試套件已配置為 GitHub Actions 工作流程：

```yaml
# .github/workflows/test.yml
name: Test Suite
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: ./scripts/run_test_suite.sh
```

## 故障排除

### 常見問題

1. **Redis連接失敗**
   ```bash
   # 確保Redis運行
   redis-cli ping
   # 應返回: PONG
   ```

2. **性能測試失敗**
   ```bash
   # 使用發布模式
   ./scripts/run_test_suite.sh --performance-only --release
   ```

3. **並發測試不穩定**
   ```bash
   # 限制測試並行度
   RUST_TEST_THREADS=1 cargo test
   ```

### 調試技巧

- 使用 `--verbose` 獲取詳細輸出
- 檢查 `RUST_LOG=debug` 環境變量
- 使用 `--nocapture` 查看打印輸出
- 運行單個測試: `cargo test test_name -- --exact`

## 貢獻指南

### 添加新測試

1. 確定測試類型（單元/集成/性能）
2. 使用適當的測試助手 (`tests/common/`)
3. 遵循命名約定：`test_component_functionality`
4. 包含性能斷言（如適用）
5. 更新此README文檔

### 測試最佳實踐

- ✅ 測試應該快速執行（除性能測試外）
- ✅ 使用有意義的斷言消息
- ✅ 清理測試資源（避免內存洩漏）
- ✅ 模擬外部依賴（不依賴真實交易所）
- ✅ 包含邊界條件和錯誤情況測試

## 參考資料

- [CLAUDE.md](../CLAUDE.md) - 系統架構和性能要求
- [Rust Testing Guide](https://doc.rust-lang.org/book/ch11-00-testing.html)
- [Criterion.rs](https://github.com/bheisler/criterion.rs) - 基準測試框架