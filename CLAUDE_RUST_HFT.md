# Rust HFT 核心引擎开发环境

## 🦀 **专用环境说明**

此 Worktree (`monday-rust-hft`) 专门用于 **Rust HFT核心引擎开发**，专注于超低延迟交易系统的核心组件。

- **分支**: `feature/rust-hft-unified`
- **专注领域**: 高性能OrderBook、策略引擎、风险管理、市场数据处理、PyO3 Python绑定
- **开发语言**: Rust (核心) + PyO3 (Python绑定)
- **核心目标**: <1μs 决策延迟，零分配算法，为Agno Framework提供高性能接口

⸻

## 🏗️ **项目结构**

```
rust_hft/                     # Rust核心代码
├── src/
│   ├── core/                 # 核心组件
│   │   ├── orderbook.rs      # 高性能订单簿
│   │   ├── strategy.rs       # 策略引擎
│   │   ├── risk_manager.rs   # 风险管理
│   │   └── config.rs         # 配置系统
│   ├── integrations/         # 外部集成
│   │   ├── bitget_connector.rs # Bitget WebSocket API
│   │   ├── dynamic_bitget_connector.rs # 动态连接管理
│   │   └── traffic_based_grouping.rs # 流量分组
│   ├── ml/                   # ML推理
│   │   ├── inference.rs      # 模型推理
│   │   ├── features.rs       # 特征提取
│   │   └── model_training_simple.rs # 简单训练
│   ├── python_bindings/      # PyO3绑定 (新增)
│   │   ├── mod.rs            # 模块定义
│   │   ├── orderbook.rs      # OrderBook Python接口
│   │   ├── strategy.rs       # 策略Python接口
│   │   └── risk.rs           # 风险管理Python接口
│   ├── engine/               # 交易引擎
│   │   ├── strategy.rs       # 策略引擎
│   │   ├── execution.rs      # 执行引擎
│   │   └── risk_manager.rs   # 风险管理
│   └── utils/                # 工具函数
│       ├── performance.rs    # 性能监控
│       └── ultra_low_latency.rs # 超低延迟优化
├── benches/                  # 性能基准测试
│   ├── decision_latency.rs   # 决策延迟测试
│   └── orderbook_performance.rs # 订单簿性能测试
├── examples/                 # 使用示例
│   ├── live_trading.rs       # 实时交易示例
│   └── adaptive_collection.rs # 自适应数据收集
├── Cargo.toml               # 依赖配置
└── pyproject.toml           # Python包配置 (新增)
```

⸻

## 🔧 **PyO3 Python绑定开发**

### **核心Python接口设计**

```rust
// src/python_bindings/mod.rs
use pyo3::prelude::*;

/// 创建OrderBook实例
#[pyfunction]
fn create_orderbook(symbol: String) -> PyResult<u64> {
    // 返回OrderBook handle ID
}

/// 更新OrderBook数据
#[pyfunction]
fn update_orderbook(handle: u64, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) -> PyResult<()> {
    // 高性能更新订单簿
}

/// 获取最佳买卖价
#[pyfunction]
fn get_best_bid_ask(handle: u64) -> PyResult<(f64, f64)> {
    // 返回最佳价格对
}

/// 计算交易信号
#[pyfunction]
fn calculate_strategy_signal(handle: u64, features: Vec<f64>) -> PyResult<f64> {
    // 使用Rust计算交易信号
}

/// 风险检查
#[pyfunction]
fn check_risk_limits(position: f64, pnl: f64, max_loss: f64) -> PyResult<bool> {
    // 实时风险检查
}

/// 获取性能指标
#[pyfunction]
fn get_performance_metrics() -> PyResult<HashMap<String, f64>> {
    // 返回延迟、吞吐量等指标
}

/// Python模块定义
#[pymodule]
fn rust_hft(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(create_orderbook, m)?)?;
    m.add_function(wrap_pyfunction!(update_orderbook, m)?)?;
    m.add_function(wrap_pyfunction!(get_best_bid_ask, m)?)?;
    m.add_function(wrap_pyfunction!(calculate_strategy_signal, m)?)?;
    m.add_function(wrap_pyfunction!(check_risk_limits, m)?)?;
    m.add_function(wrap_pyfunction!(get_performance_metrics, m)?)?;
    Ok(())
}
```

### **开发工作流程**

```bash
# 构建Rust核心
cargo build --release

# 构建Python绑定
maturin develop --release

# 测试Python集成
python -c "
import rust_hft
handle = rust_hft.create_orderbook('BTCUSDT')
print(f'OrderBook created: {handle}')
"

# 性能基准测试
cargo bench --bench decision_latency
```

⸻

## 🎯 **性能优化开发**

### **关键性能指标**

- **决策延迟**: P99 < 1μs
- **订单簿更新**: > 100,000 updates/s
- **内存使用**: 零分配热路径
- **CPU效率**: < 80% 单核使用率

### **优化工具**

```bash
# 延迟基准测试
cargo bench --bench decision_latency

# 内存分析
cargo build --release
valgrind --tool=memcheck ./target/release/rust_hft

# SIMD优化检查
objdump -d target/release/rust_hft | grep -i "simd\|avx\|sse"

# 性能分析
perf record -g ./target/release/rust_hft
perf report
```

⸻

## 📊 **集成测试**

### **与Agno Framework集成验证**

```python
# 测试脚本示例
import rust_hft
import time

# 创建OrderBook
handle = rust_hft.create_orderbook("BTCUSDT")

# 模拟市场数据
bids = [(50000.0, 0.1), (49999.0, 0.2)]
asks = [(50001.0, 0.15), (50002.0, 0.25)]

# 测试延迟
start = time.time_ns()
rust_hft.update_orderbook(handle, bids, asks)
best_bid, best_ask = rust_hft.get_best_bid_ask(handle)
end = time.time_ns()

latency_us = (end - start) / 1000
print(f"Latency: {latency_us:.2f}μs")

# 测试交易信号
features = [0.5, 0.3, 0.8, 0.2]
signal = rust_hft.calculate_strategy_signal(handle, features)
print(f"Signal: {signal}")
```

⸻

## 🔄 **开发最佳实践**

### **代码质量**

```bash
# 格式化代码
cargo fmt

# 静态分析
cargo clippy --all-targets -- -D warnings

# 测试覆盖率
cargo tarpaulin --out html
```

### **性能测试**

```bash
# 运行所有基准测试
cargo bench

# 特定性能测试
cargo bench decision_latency
cargo bench orderbook_performance

# 压力测试
cargo test --release test_high_frequency_updates
```

⸻

## 🚀 **部署配置**

### **构建优化**

```toml
# Cargo.toml 优化设置
[profile.release]
debug = false
lto = true
codegen-units = 1
panic = "abort"
overflow-checks = false

[dependencies]
pyo3 = { version = "0.20", features = ["extension-module"] }
tokio = { version = "1.0", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
```

### **Python包配置**

```toml
# pyproject.toml
[build-system]
requires = ["maturin>=1.0,<2.0"]
build-backend = "maturin"

[project]
name = "rust-hft"
version = "2.0.0"
description = "Ultra-low latency HFT engine for Agno Framework"
authors = ["HFT Team"]
dependencies = [
    "numpy>=1.21.0",
    "pandas>=1.3.0",
]
```

⸻

## 🎯 **开发目标**

### **短期目标 (1-2周)**
- [ ] 完成PyO3绑定的核心接口
- [ ] 实现OrderBook的Python接口
- [ ] 优化决策延迟至<1μs
- [ ] 完成基本的集成测试

### **中期目标 (1月)**
- [ ] 完整的Python API文档
- [ ] 自动化性能回归测试
- [ ] 与Agno Framework的深度集成
- [ ] 生产级错误处理

### **长期目标 (3月)**
- [ ] 多交易所支持
- [ ] 动态策略热加载
- [ ] 分布式架构支持
- [ ] 完整的监控体系

⸻

## 🔗 **相关文档**

- **主项目**: `/Users/shihsonic/Documents/monday/`
- **Agno Framework**: `/Users/shihsonic/Documents/monday-agno-framework/`
- **集成测试**: `/Users/shihsonic/Documents/monday-integration/`
- **性能优化报告**: `rust_hft/PERFORMANCE_OPTIMIZATION_PLAN.md`

---

**开发重点**: 为Agno Framework提供极致性能的Rust后端，通过PyO3实现无缝集成，确保<1μs延迟目标。