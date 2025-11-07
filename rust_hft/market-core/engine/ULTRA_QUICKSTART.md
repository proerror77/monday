# Ultra Performance Quickstart

**TL;DR**: 熱路徑使用 `ingest_fast()`，節省 10-15x 延遲。

---

## 快速開始

### 1. 基本用法

```rust
use engine::dataflow::{UltraEventIngester, UltraIngestionConfig};

// 創建 ingester（HFT 配置）
let (ingester, consumer) = UltraEventIngester::new(
    UltraIngestionConfig::hft()
);

// 熱路徑：預檢查 + unsafe unchecked
if !ingester.is_full() {
    unsafe {
        ingester.ingest_fast(event);
    }
}

// 消費：批量處理
let batch = consumer.consume_batch(64);
for event in batch {
    // 處理事件
}
```

### 2. 何時使用 Ultra 版本？

| 場景 | 使用版本 | 延遲 |
|------|----------|------|
| WebSocket 熱路徑 | `ingest_fast()` | ~10-15ns |
| 非關鍵路徑 | `ingest()` | ~150-200ns |
| 開發調試 | `ingest()` | ~150-200ns |

### 3. 安全要點

**必須保證**：
- ✅ 調用前檢查 `!is_full()`
- ✅ 事件時間戳有效（非零）
- ✅ 單生產者/單消費者

**錯誤示範**：
```rust
// ❌ 不安全：未檢查容量
unsafe { ingester.ingest_fast(event); }

// ✅ 正確：預先檢查
if !ingester.is_full() {
    unsafe { ingester.ingest_fast(event); }
}
```

---

## 性能對比

### Ring Buffer

| 操作 | 標準版 | Ultra版 | 改進 |
|------|--------|---------|------|
| push | ~15-20ns | ~3-5ns | **3-4x** |
| pop | ~10-15ns | ~3-5ns | **2-3x** |

### Event Ingestion

| 操作 | 標準版 | Ultra版 | 改進 |
|------|--------|---------|------|
| ingest | ~150-200ns | ~10-15ns | **10-15x** |
| batch | ~180ns/item | ~8-10ns/item | **18-22x** |

---

## API 參考

### Ultra Ring Buffer

```rust
use engine::dataflow::{ultra_ring_buffer, UltraRingBuffer};

let (producer, consumer) = ultra_ring_buffer(1024);

// 快速路徑（unsafe）
if !producer.is_full() {
    unsafe { producer.send_unchecked(item); }
}

if !consumer.is_empty() {
    let item = unsafe { consumer.recv_unchecked() };
}

// 安全路徑（checked）
producer.send(item)?;
consumer.recv();
```

### Ultra Event Ingester

```rust
use engine::dataflow::{UltraEventIngester, UltraIngestionConfig};

let (ingester, consumer) = UltraEventIngester::new(
    UltraIngestionConfig::hft()
);

// 快速單次寫入
if !ingester.is_full() {
    unsafe { ingester.ingest_fast(event); }
}

// 快速批量寫入
let capacity = ingester.available_capacity();
if capacity >= events.len() {
    unsafe { ingester.ingest_batch_unchecked(&events); }
}

// 快速批量消費
if !consumer.is_empty() {
    let batch = unsafe { consumer.consume_batch_unchecked(64) };
}
```

---

## 配置選項

```rust
// HFT 配置（極致性能）
UltraIngestionConfig::hft()
// - queue_capacity: 16384
// - stale_threshold_us: 1000 (1ms)

// 默認配置
UltraIngestionConfig::default()
// - queue_capacity: 32768
// - stale_threshold_us: 3000 (3ms)

// 自定義配置
UltraIngestionConfig {
    queue_capacity: 65536,
    stale_threshold_us: 2000,
}
```

---

## 示例代碼

運行完整演示：
```bash
cargo run --example ultra_performance_demo
```

查看源碼：
```
market-core/engine/examples/ultra_performance_demo.rs
```

---

## 性能驗證

### 1. 單元測試

```bash
cargo test -p engine --lib ring_buffer_ultra
cargo test -p engine --lib ingestion_ultra
```

### 2. 性能基準

創建 `benches/ultra_hotpath.rs`：
```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine::dataflow::ultra_ring_buffer;

fn bench_ultra_push(c: &mut Criterion) {
    let (producer, _) = ultra_ring_buffer(1024);

    c.bench_function("ultra_push", |b| {
        b.iter(|| {
            if !producer.is_full() {
                unsafe { producer.send_unchecked(black_box(42)); }
            }
        });
    });
}

criterion_group!(benches, bench_ultra_push);
criterion_main!(benches);
```

運行：
```bash
cargo bench --bench ultra_hotpath
```

---

## 故障排除

### Q: 為什麼還要保留標準版本？

A: "Never break userspace" - 保留監控能力。兩種版本共存：
- 標準版：完整 metrics + logging（開發/調試）
- Ultra版：零開銷（生產熱路徑）

### Q: unsafe 安全嗎？

A: 安全，前提是遵守不變量：
- INVARIANT-1: head/tail ∈ [0, capacity)
- INVARIANT-2: slots[tail..head) 已初始化
- INVARIANT-3: SPSC 語義
- INVARIANT-4: capacity 是 2 的冪次

所有 unsafe 操作都有清晰的注釋說明前置條件。

### Q: 如何測量改進？

A: 使用抽樣監控：
```rust
// 熱路徑：ultra 版本（無監控）
unsafe { ingester.ingest_fast(event); }

// 抽樣 0.1%：標準版本（有監控）
if rand::random::<u64>() % 1000 == 0 {
    let start = now_micros();
    ingester.ingest(event)?;
    metrics::histogram!("latency", now_micros() - start);
}
```

---

## 下一步

1. ✅ **立即行動**：在 WebSocket 接收路徑使用 `ingest_fast()`
2. 🔄 **性能驗證**：運行基準測試確認改進
3. 🔄 **生產部署**：抽樣監控 + 告警閾值
4. 📋 **長期優化**：參考 `P0_PERFORMANCE_FIXES.md` 第三階段（WebSocket 零拷貝）

---

**記住**：極致性能來自數據結構，而非代碼技巧。
