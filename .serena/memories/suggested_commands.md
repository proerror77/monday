# 建議的開發命令

## Rust 開發命令 (`rust_hft/`)

### 測試
```bash
# 運行所有單元測試
cargo test --workspace

# 運行集成測試
cargo test --workspace --test integration

# 運行特定測試
cargo test --package rust_hft --test complete_oms_test

# 運行基準測試
cargo bench
```

### 格式化與檢查
```bash
# 代碼格式化
cargo fmt

# Clippy 靜態分析
cargo clippy --workspace -- -D warnings

# 構建檢查
cargo check --workspace
```

### 構建與運行
```bash
# 開發構建
cargo build

# 發布構建（高性能）
cargo build --release --profile release-max

# 運行主程序
cargo run --bin rust_hft

# 運行範例
cargo run --example 00_quickstart
```

### 性能測試
```bash
# 運行性能基準
cargo bench --bench orderbook_benchmarks
cargo bench --bench ultra_high_perf_benchmarks

# 運行壓力測試
./scripts/run_full_test_suite.sh
```

## Python Agent 開發 (`ml_workspace/`, `ops_workspace/`)

### 測試
```bash
# ML Workspace 測試
cd ml_workspace
python -m pytest tests/ -v

# Ops Workspace 測試  
cd ops_workspace
python -m pytest tests/ -v
```

### 格式化
```bash
# Python 代碼格式化
black . && isort . && flake8 .
```

### Agno Workspace 管理
```bash
# 創建工作流程
ag ws create --name btc_training --config agno.toml

# 執行訓練任務
ag ws run --workflow btc_training

# 監控狀態
ag ws monitor --workspace ml_workspace
```

## 系統級命令

### Docker 部署
```bash
# 啟動基礎設施
docker-compose up -d redis clickhouse prometheus grafana

# 啟動 Rust 引擎
docker-compose up -d rust-hft-engine

# 啟動監控代理
docker-compose up -d ops-agent
```

### 健康檢查
```bash
# 檢查 Rust 引擎指標
curl http://localhost:8000/metrics

# Redis 連接測試
redis-cli ping

# ClickHouse 連接測試
curl http://localhost:8123/ping
```