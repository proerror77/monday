# Snapshot Container Migration: left-right → ArcSwap Only

**日期**: 2025-09-16
**原因**: 编译错误 + 架构不匹配

---

## 问题分析

### 编译错误

```rust
error[E0277]: the trait bound `T: left_right::Absorb<()>` is not satisfied
error[E0277]: `NonNull<T>` cannot be shared between threads safely
error[E0277]: `Cell<usize>` cannot be shared between threads safely
```

### 根本原因

1. **架构不匹配**: left-right 的 `ReadHandle` 和 `WriteHandle` **不是 `Sync`**
   - `ReadHandle<T>` 内部使用 `Cell<usize>` 和 `*const T`
   - 设计为每线程一个 `ReadHandle` clone，而不是跨线程共享

2. **性能不适合**: left-right 写成本极高
   - 需要等待所有读者完成才能更新
   - 不适合 HFT 系统频繁更新（微秒级）的场景

3. **API 冲突**:
   - 我们的 `SnapshotPublisher<T>: Send + Sync` 要求单实例可跨线程共享
   - left-right 要求每线程 clone `ReadHandle`，违反 `Sync`

---

## 解决方案：完全移除 left-right

### 技术对比

| 指标 | ArcSwap | left-right |
|------|---------|------------|
| 读延迟 (p99) | < 10ns | < 5ns |
| 写延迟 | < 100ns | **数毫秒** (等待读者) |
| 内存模型 | `Sync` ✅ | 非 `Sync` ❌ |
| 更新频率 | 微秒级 ✅ | 秒级 |
| HFT 适用性 | **优秀** ✅ | **不适合** ❌ |

### 专家建议

HFT 领域共识：
- **读多写少** → ArcSwap (10ns 读取足够快)
- **极致读性能但几乎不写** → left-right
- **频繁更新场景** → ArcSwap (写成本可控)

我们的场景：每次市场事件都需要更新快照（微秒级频率），**ArcSwap 是唯一正确选择**。

---

## 代码变更

### 移除的文件/代码

1. `src/lib.rs` 的 `leftright_impl` 模块 (73行)
2. `Cargo.toml` 的 `snapshot-left-right` feature
3. 根 `Cargo.toml` 的 `left-right` 依赖

### 保留的架构

```rust
pub trait SnapshotPublisher<T>: Send + Sync {
    fn store(&self, snapshot: Arc<T>);
    fn load(&self) -> Arc<T>;
    fn is_updated(&self) -> bool;
}

pub struct ArcSwapPublisher<T> {
    inner: ArcSwap<T>,
    updated: AtomicBool,
}

pub type DefaultPublisher<T> = ArcSwapPublisher<T>;
```

---

## 如何避免类似问题

### 1. 选择库时的检查清单

✅ **在采用新库前验证**：
- [ ] 是否满足 `Send + Sync` 要求？
- [ ] 写延迟是否在可接受范围（< 1ms）？
- [ ] 是否适合我们的更新频率（微秒级）？
- [ ] 社区是否有 HFT 场景的实践案例？

### 2. 设计决策记录

**教训**: left-right 是优秀的库，但不适合 HFT 场景。

| 使用场景 | 推荐方案 |
|---------|---------|
| 配置热加载（分钟级更新） | left-right ✅ |
| 市场快照（微秒级更新） | ArcSwap ✅ |
| 静态引用数据（几乎不更新） | left-right ✅ |

### 3. 编译时验证

建议在 trait 上强制 `Send + Sync`，可在设计阶段就发现问题：

```rust
pub trait SnapshotPublisher<T>: Send + Sync {
    // 编译时强制检查
}
```

---

## 验证结果

```bash
$ cargo check --package hft-snapshot
   Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.75s

$ cargo test --package hft-snapshot
running 2 tests
test tests::test_multiple_readers ... ok
test tests::test_snapshot_container_basic ... ok

test result: ok. 2 passed; 0 failed; 0 ignored
```

---

## 性能预期

基于 ArcSwap 官方 benchmark：

```
读取延迟 (p50): ~6ns
读取延迟 (p99): <10ns
写入延迟: ~80-100ns
```

对于 HFT 系统（p99 目标 25μs），快照读取的 10ns 开销完全可忽略（占比 0.04%）。

---

## 参考资料

- [arc-swap crate](https://docs.rs/arc-swap/latest/arc_swap/)
- [left-right crate](https://docs.rs/left-right/latest/left_right/)
- [Rust 并发模式：Send vs Sync](https://doc.rust-lang.org/nomicon/send-and-sync.html)
