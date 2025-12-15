# HFT 测试执行手册

**目标**: 提供可直接执行的测试命令与工作流

---

## 快速开始

```bash
# 1. 快速检查 (提交前必跑)
cargo test --lib

# 2. 完整测试套件
cargo test --workspace

# 3. 性能基准
cargo bench --bench hotpath_latency_p99
```

---

## 本地测试工作流

### 开发新功能时

```bash
# Step 1: 写失败测试
vim market-core/snapshot/src/lib.rs  # 添加 #[test]

# Step 2: 验证测试失败
cargo test test_new_feature -- --exact

# Step 3: 实现功能
# 编辑代码...

# Step 4: 验证测试通过
cargo test test_new_feature -- --exact

# Step 5: 运行相关模块所有测试
cargo test --package hft-snapshot

# Step 6: 运行完整测试套件
cargo test --workspace
```

### 修改并发代码时

```bash
# Step 1: 运行现有 Loom 测试
RUSTFLAGS="--cfg loom" cargo test --features loom

# Step 2: 添加新的 Loom 测试
# 编辑 ring_buffer.rs...

# Step 3: 验证新测试
LOOM_MAX_PREEMPTIONS=3 cargo test --features loom \
  test_your_new_loom_test -- --exact

# Step 4: 完整 Loom 测试
LOOM_MAX_PREEMPTIONS=5 cargo test --features loom
```

---

## CI/CD 模拟

### 模拟 PR 检查

```bash
# 1. 运行所有测试
cargo test --workspace

# 2. Clippy 检查
cargo clippy -- -D warnings

# 3. 格式化检查
cargo fmt -- --check

# 4. 性能基准
cargo bench --no-run

# 5. 覆盖率检查
cargo tarpaulin --out Html --exclude-files 'target/*' 'tests/*'
```

### 模拟发布前检查

```bash
# 1. Release 构建测试
cargo test --workspace --release

# 2. 长时间压力测试 (24h)
cargo test --release --test stability -- \
  --ignored test_24h_continuous_operation

# 3. Fuzzing (6h)
cargo fuzz run ring_buffer_fuzz -- \
  -max_total_time=21600 -workers=8

# 4. 内存泄漏检测
valgrind --leak-check=full \
  target/release/hft-engine-test
```

---

## 特定场景测试

### Ring Buffer 并发测试

```bash
# 基础测试
cargo test --package hft-engine ring_buffer

# Loom 并发验证
RUSTFLAGS="--cfg loom" cargo test --features loom \
  loom_spsc_invariant loom_release_acquire

# Property 测试
cargo test --package hft-engine -- \
  --include-ignored test_push_pop_preserves_order

# Fuzzing
cargo fuzz run ring_buffer_fuzz -- -max_total_time=3600
```

### Snapshot 性能测试

```bash
# 单元测试
cargo test --package hft-snapshot

# 并发压力测试
cargo test test_concurrent_reads_1000_threads -- \
  --nocapture --test-threads=1

# 性能基准
cargo bench --bench snapshot_benchmarks
```

### 网络层集成测试

```bash
# WebSocket 重连测试
cargo test --test ws_reconnect -- \
  --test-threads=1 --nocapture

# 心跳超时测试
cargo test test_heartbeat_timeout -- \
  --exact --nocapture
```

---

## 性能基准测试

### 运行基准

```bash
# 所有基准
cargo bench

# 特定基准组
cargo bench --bench hotpath_latency_p99

# 保存基线
cargo bench -- --save-baseline main

# 对比基线
cargo bench -- --baseline main
```

### 生成性能报告

```bash
# 生成 HTML 报告
cargo bench --bench hotpath_latency_p99
open target/criterion/hotpath/report/index.html

# 使用 critcmp 对比
cargo install critcmp
critcmp main pr
```

---

## Fuzzing 工作流

### 初次设置

```bash
# 安装工具
cargo install cargo-fuzz
rustup default nightly

# 创建 fuzz target
cargo fuzz add ring_buffer_fuzz
```

### 运行 Fuzzing

```bash
# 短时间测试 (1h)
cargo fuzz run ring_buffer_fuzz -- -max_total_time=3600

# 长时间测试 (24h, 多核)
cargo fuzz run ring_buffer_fuzz -- \
  -max_total_time=86400 -workers=16

# 从崩溃输入调试
cargo fuzz run ring_buffer_fuzz \
  fuzz/artifacts/ring_buffer_fuzz/crash-xxx
```

### Fuzzing 结果分析

```bash
# 查看崩溃
ls fuzz/artifacts/ring_buffer_fuzz/

# 最小化崩溃输入
cargo fuzz cmin ring_buffer_fuzz

# 覆盖率报告
cargo fuzz coverage ring_buffer_fuzz
```

---

## 覆盖率报告

### 生成报告

```bash
# 安装工具
cargo install cargo-tarpaulin

# HTML 报告
cargo tarpaulin --out Html --output-dir coverage
open coverage/index.html

# XML 报告 (Codecov)
cargo tarpaulin --out Xml
bash <(curl -s https://codecov.io/bash)

# 仅测试特定包
cargo tarpaulin --package hft-engine --out Html
```

### 覆盖率目标

```bash
# 验证覆盖率阈值
coverage=$(grep -oP 'line-rate="\K[0-9.]+' cobertura.xml)
threshold=0.70

if (( $(echo "$coverage < $threshold" | bc -l) )); then
  echo "Coverage $coverage below threshold $threshold"
  exit 1
fi
```

---

## 调试技巧

### 详细测试输出

```bash
# 显示 println! 输出
cargo test test_name -- --nocapture

# 显示所有测试 (包括通过的)
cargo test -- --nocapture --test-threads=1

# 仅运行失败的测试
cargo test -- --test-threads=1 \
  test_that_failed_before
```

### Loom 调试

```bash
# 显示探索进度
LOOM_LOG=trace cargo test --features loom \
  loom_test -- --nocapture

# 增加探索深度
LOOM_MAX_PREEMPTIONS=7 \
LOOM_MAX_BRANCHES=100000 \
  cargo test --features loom

# 保存失败的执行路径
LOOM_CHECKPOINT_FILE=/tmp/loom.checkpoint \
  cargo test --features loom loom_test
```

### 性能分析

```bash
# 使用 perf
cargo build --release --package hft-engine
perf record -g target/release/hft-engine-bench
perf report

# 使用 flamegraph
cargo flamegraph --bench hotpath_latency_p99
```

---

## 测试数据管理

### 使用 Fixture

```rust
use crate::common::fixtures::load_market_data;

#[test]
fn test_with_real_data() {
    let events = load_market_data("binance", "btcusdt");
    // 使用真实数据测试
}
```

### 生成 Mock 数据

```rust
use crate::common::generators::MarketEventGenerator;

#[test]
fn test_with_generated_data() {
    let mut gen = MarketEventGenerator::new(100.0);
    let trades = gen.generate_trades(1000);
    // 使用生成的数据测试
}
```

---

## 常见问题排查

### 测试超时

```bash
# 增加超时时间
cargo test -- --test-threads=1 --timeout=300
```

### 测试不稳定 (Flaky)

```bash
# 重复运行 100 次
for i in {1..100}; do
  cargo test test_flaky || break
done
```

### Loom 测试太慢

```bash
# 减少探索深度
LOOM_MAX_PREEMPTIONS=2 cargo test --features loom

# 仅测试特定场景
cargo test --features loom loom_specific_test -- --exact
```

---

## 测试报告清单

提交 PR 前检查:

- [ ] `cargo test --workspace` 通过
- [ ] `cargo clippy -- -D warnings` 无警告
- [ ] `cargo fmt -- --check` 通过
- [ ] `cargo bench --no-run` 编译成功
- [ ] 覆盖率 ≥70%
- [ ] 无新增热路径 unwrap

发布前检查:

- [ ] `cargo test --release --workspace` 通过
- [ ] 24h 稳定性测试通过
- [ ] Fuzzing 6h 无崩溃
- [ ] 性能基准无回归
- [ ] 内存泄漏检测通过

---

**参考完整报告**: `TEST_EVALUATION_REPORT.md`
