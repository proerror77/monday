# 🎯 Rust HFT 功能模块使用指南

## 📋 功能模块概述

这个 HFT 系统采用模块化设计，你可以根据需要选择性编译不同的功能模块，提高编译效率和减少依赖复杂度。

## 🔧 核心功能模块

### 基础模块
```bash
# 仅核心基础设施（配置、错误处理、工具）
cargo build --no-default-features --features core

# 市场数据功能
cargo build --no-default-features --features market-data

# 高性能订单簿引擎
cargo build --no-default-features --features orderbook-engine
```

### 交易功能
```bash
# 基础交易功能
cargo build --no-default-features --features basic-trading

# 标准交易（包含回测和风险管理）
cargo build --no-default-features --features standard-trading

# 高频交易引擎
cargo build --no-default-features --features high-frequency
```

### 机器学习模块
```bash
# 仅特征工程
cargo build --no-default-features --features ml-features

# 纯 Rust ML（Candle）
cargo build --no-default-features --features ml-candle

# PyTorch 集成（需要安装 PyTorch）
cargo build --no-default-features --features ml-pytorch

# ML 研发环境
cargo build --no-default-features --features ml-research
```

### 数据存储
```bash
# 历史数据管理
cargo build --no-default-features --features historical-data

# ClickHouse 数据库
cargo build --no-default-features --features database

# 实时数据记录
cargo build --no-default-features --features data-recording
```

## 🎯 预设组合包

### 1. 最小编译（快速测试）
```bash
cargo build --no-default-features --features core
cargo test --no-default-features --features core
```

### 2. 基础交易开发
```bash
cargo build --features basic-trading
cargo run --features basic-trading --example connect_to_exchange
```

### 3. 量化策略开发
```bash
cargo build --features quant-trading
cargo run --features quant-trading --example model_training
```

### 4. 高频交易优化
```bash
cargo build --features high-frequency --release
cargo bench --features high-frequency
```

### 5. ML 模型研发
```bash
cargo build --features ml-research
python -c "import rust_hft; rust_hft.train_model()"
```

## 🚀 常用开发场景

### 场景1: 修改核心逻辑
如果你在修改 `src/core/` 下的文件：
```bash
# 快速编译检查
cargo check --no-default-features --features core
# 运行核心测试
cargo test --no-default-features --features core core::
```

### 场景2: 开发交易策略
如果你在开发新的交易策略：
```bash
# 编译策略引擎
cargo build --features strategy-engine
# 运行回测
cargo run --features backtest-engine --example backtesting
```

### 场景3: 优化订单簿性能
如果你在优化订单簿性能：
```bash
# 编译高性能版本
cargo build --features ultra-performance --release
# 运行性能测试
cargo bench --features performance-testing
```

### 场景4: 集成新的交易所
如果你在添加新的交易所连接：
```bash
# 编译市场数据功能
cargo build --features market-data
# 测试连接
cargo test --features bitget-connector integration_tests::
```

## 📊 Examples 对应的 Features

| Example | 推荐 Features | 说明 |
|---------|---------------|------|
| `connect_to_exchange` | `bitget-connector` | 基础连接测试 |
| `data_collection` | `market-data, data-recording` | 数据收集 |
| `model_training` | `ml-candle` 或 `ml-pytorch` | 模型训练 |
| `model_evaluation` | `ml-inference, testing-framework` | 模型评估 |
| `live_trading` | `standard-trading` 或 `high-frequency` | 实盘交易 |
| `performance_test` | `ultra-performance, performance-testing` | 性能测试 |
| `backtesting` | `backtest-engine` | 策略回测 |

## ⚡ 性能优化建议

### 开发阶段
```bash
# 快速迭代，只编译需要的模块
cargo check --no-default-features --features core,market-data

# 并行编译加速
cargo build --features basic-trading -j $(nproc)
```

### 测试阶段
```bash
# 分模块测试
cargo test --no-default-features --features core core::
cargo test --no-default-features --features market-data market_data::
cargo test --no-default-features --features strategy-engine engine::
```

### 生产部署
```bash
# 高性能编译
cargo build --features high-frequency --release

# 完整功能编译
cargo build --features full --release
```

## 🛠️ 故障排除

### 编译错误排查
1. **缺少依赖**: 检查是否启用了正确的 feature
2. **版本冲突**: 使用 `cargo tree` 检查依赖树
3. **平台兼容**: 某些优化功能仅支持特定平台

### 常见问题
```bash
# 问题：PyTorch 相关错误
# 解决：使用 Candle 替代
cargo build --no-default-features --features ml-candle

# 问题：ClickHouse 连接错误  
# 解决：暂时禁用数据库功能
cargo build --no-default-features --features basic-trading

# 问题：Barter 依赖错误
# 解决：使用自定义交易引擎
cargo build --no-default-features --features orderbook-engine,strategy-engine
```

## 📈 性能对比

| Feature 组合 | 编译时间 | 二进制大小 | 内存使用 | 适用场景 |
|-------------|----------|------------|----------|----------|
| `core` | ~30s | ~2MB | ~10MB | 快速测试 |
| `basic-trading` | ~2min | ~15MB | ~50MB | 策略开发 |
| `high-frequency` | ~3min | ~8MB | ~30MB | 生产交易 |
| `full` | ~8min | ~50MB | ~200MB | 完整功能 |

## 🎯 下一步建议

1. **从最小功能开始**: 先确保 `core` 功能编译通过
2. **逐步添加模块**: 根据需要逐个启用功能模块  
3. **性能测试**: 使用 `performance-testing` 验证优化效果
4. **持续集成**: 在 CI 中分别测试不同的 feature 组合

---

💡 **提示**: 使用 `cargo check` 替代 `cargo build` 可以大幅提高编译检查速度！