# HFT 系统测试策略与实施指南

**版本**: 1.0  
**状态**: 🚀 **Ready for Implementation**

---

## 快速导航

1. [TDD 开发流程](#tdd-开发流程)
2. [核心组件测试](#核心组件测试规范)
3. [网络层测试](#网络层测试)
4. [性能测试](#性能测试)
5. [质量门控](#质量门控检查清单)

---

## TDD 开发流程

### Red-Green-Refactor

```rust
// 1. RED - 写失败测试
#[test]
fn test_snapshot_concurrent_updates() {
    let container = SnapshotContainer::new(Market::default());
    // 并发测试逻辑
    assert!(final_snapshot.price < 1000); // 失败
}

// 2. GREEN - 最小实现
impl<T> SnapshotContainer<T> {
    pub fn store(&self, snapshot: Arc<T>) {
        self.publisher.store(snapshot);
    }
}

// 3. REFACTOR - 优化
```

---

## 核心组件测试规范

### Ring Buffer

```rust
mod loom_tests {
    #[cfg(loom)]
    #[test] fn loom_spsc_invariant() { ... }
    #[test] fn loom_release_acquire() { ... }
}

mod property_tests {
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn preserves_fifo_order(items: Vec<u64>) { ... }
    }
}
```

---

## 网络层测试

```rust
#[tokio::test]
async fn test_ws_reconnect() {
    let mock_server = MockServer::start().await;
    // 模拟断线重连
}
```

---

## 性能测试

```rust
fn bench_hotpath(c: &mut Criterion) {
    c.bench_function("ingestion_p99", |b| {
        b.iter(|| {
            // 热路径性能测试
        });
    });
}
```

---

## 质量门控检查清单

### PR 合并前
- [ ] 单元测试通过
- [ ] Loom 测试通过
- [ ] 覆盖率 ≥70%
- [ ] 无热路径 unwrap
- [ ] 性能无回归

---

**详见完整报告**: `TEST_EVALUATION_REPORT.md`
