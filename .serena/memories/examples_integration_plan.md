# Examples 整合計劃

## 當前狀況分析
- 總計34個範例文件，存在大量功能重複
- 已有部分目錄結構（01_data_collection, 05_system_integration等）
- 主要問題：延遲測試、E2E測試、策略演示分散且重複

## 整合目標結構
```
examples/
├── 00_quickstart.rs                    # 保持不變 - 快速入門
├── 01_data_collection/                 # 已存在 - 數據收集
├── 02_trading_strategies/              # 新建 - 交易策略
│   ├── r_breaker_complete.rs          # 整合6個R-Breaker文件
│   └── multi_strategy_demo.rs          # 保持不變
├── 03_performance_testing/             # 新建 - 性能測試
│   ├── latency_benchmark_suite.rs     # 整合8個延遲測試文件
│   ├── stress_test_comprehensive.rs   # 整合壓力測試
│   └── concurrent_performance.rs      # 保持不變
├── 04_exchange_integration/            # 新建 - 交易所集成
│   ├── connection_test_suite.rs       # 整合連接測試文件
│   └── exchange_health_monitor.rs     # 整合健康監控
├── 05_system_integration/              # 已存在 - 系統集成
└── 06_e2e_workflows/                   # 新建 - 端到端流程
    ├── complete_trading_flow.rs       # 整合E2E測試文件
    └── real_data_pipeline.rs          # 整合實時數據流程
```

## 重複文件識別

### 延遲測試類（可整合為 latency_benchmark_suite.rs）
1. core_latency_test.rs
2. simple_latency_test.rs  
3. e2e_latency_test.rs
4. real_e2e_latency_test.rs
5. performance_e2e_test.rs
6. comprehensive_db_stress_test.rs (部分延遲功能)

### R-Breaker策略類（可整合為 r_breaker_complete.rs）
1. r_breaker_bitget_test.rs
2. r_breaker_live_trading.rs
3. r_breaker_real_trading.rs
4. r_breaker_simple_test.rs
5. r_breaker_standalone.rs
6. run_r_breaker.sh (配置腳本)

### E2E測試類（可整合為 complete_trading_flow.rs）
1. e2e_test_demo.rs
2. real_data_e2e_dryrun.rs
3. real_data_e2e_dryrun_fixed.rs

### 連接測試類（可整合為 connection_test_suite.rs）
1. advanced_connection_test.rs
2. test_bitget_connection.rs
3. test_exchange_integration.rs
4. test_all_exchanges.rs

## 整合策略
1. **保留核心功能**：每個整合文件包含所有原文件的核心功能
2. **統一接口**：使用命令行參數選擇測試模式
3. **模組化設計**：每個功能作為獨立模組，便於維護
4. **向後兼容**：保持原有的測試覆蓋範圍