//! Ultra Performance Components Benchmark
//!
//! 專門測試 ultra 版本組件的性能：
//! - ring_buffer_ultra vs ring_buffer
//! - ingestion_ultra vs ingestion
//!
//! Run with: cargo bench --bench ultra_components

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use engine::dataflow::ring_buffer::SpscRingBuffer;
use engine::dataflow::UltraRingBuffer;
use ports::MarketEvent;

/// 創建測試用的 MarketEvent
fn create_test_event(id: u64) -> MarketEvent {
    use hft_core::{Price, Quantity, Symbol};
    use ports::{BookLevel, MarketSnapshot};

    MarketEvent::Snapshot(MarketSnapshot {
        symbol: Symbol::from("BINANCE:BTCUSDT"),
        timestamp: id,
        bids: vec![BookLevel {
            price: Price::from_f64(50000.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
        }],
        asks: vec![BookLevel {
            price: Price::from_f64(50001.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
        }],
        sequence: id,
        source_venue: Some(hft_core::VenueId::BINANCE),
    })
}

/// Benchmark: Ring Buffer Push Performance
fn bench_ring_buffer_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer_push");
    group.throughput(Throughput::Elements(1));

    // 標準版本 - Option<T>
    group.bench_function("standard", |b| {
        b.iter_batched(
            || (SpscRingBuffer::new(1024), create_test_event(0)),
            |(buffer, event)| black_box(buffer.try_push(event)),
            BatchSize::SmallInput,
        );
    });

    // Ultra 版本 - MaybeUninit<T>
    group.bench_function("ultra", |b| {
        b.iter_batched(
            || (UltraRingBuffer::new(1024), create_test_event(0)),
            |(buffer, event)| unsafe { black_box(buffer.push_unchecked(event)) },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark: Ring Buffer Pop Performance
fn bench_ring_buffer_pop(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer_pop");
    group.throughput(Throughput::Elements(1));

    // 標準版本 - 使用 roundtrip 模式確保 buffer 始終有數據
    group.bench_function("standard", |b| {
        b.iter_batched(
            || {
                let buffer = SpscRingBuffer::new(1024);
                buffer.try_push(create_test_event(0)).ok();
                buffer
            },
            |buffer| black_box(buffer.try_pop()),
            BatchSize::SmallInput,
        );
    });

    // Ultra 版本 - 使用 roundtrip 模式確保 buffer 始終有數據
    group.bench_function("ultra", |b| {
        b.iter_batched(
            || {
                let buffer = UltraRingBuffer::new(1024);
                unsafe {
                    buffer.push_unchecked(create_test_event(0));
                }
                buffer
            },
            |buffer| unsafe { black_box(buffer.pop_unchecked()) },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark: Ring Buffer Round Trip (Push + Pop)
fn bench_ring_buffer_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer_roundtrip");
    group.throughput(Throughput::Elements(1));

    // 標準版本
    group.bench_function("standard", |b| {
        let buffer = SpscRingBuffer::new(1024);
        let mut counter = 0u64;

        b.iter(|| {
            let event = create_test_event(counter);
            counter += 1;
            buffer.try_push(event).ok();
            black_box(buffer.try_pop())
        });
    });

    // Ultra 版本
    group.bench_function("ultra", |b| {
        let buffer = UltraRingBuffer::new(1024);
        let mut counter = 0u64;

        b.iter(|| {
            let event = create_test_event(counter);
            counter += 1;
            unsafe {
                buffer.push_unchecked(event);
                black_box(buffer.pop_unchecked())
            }
        });
    });

    group.finish();
}

/// Benchmark: Batch Operations
fn bench_ring_buffer_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer_batch");

    for batch_size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));

        // 標準版本
        group.bench_with_input(
            BenchmarkId::new("standard", batch_size),
            batch_size,
            |b, &size| {
                let buffer = SpscRingBuffer::new(2048);

                b.iter(|| {
                    for i in 0..size {
                        buffer.try_push(create_test_event(i)).ok();
                    }
                    for _ in 0..size {
                        black_box(buffer.try_pop());
                    }
                });
            },
        );

        // Ultra 版本
        group.bench_with_input(
            BenchmarkId::new("ultra", batch_size),
            batch_size,
            |b, &size| {
                let buffer = UltraRingBuffer::new(2048);

                b.iter(|| unsafe {
                    for i in 0..size {
                        buffer.push_unchecked(create_test_event(i));
                    }
                    for _ in 0..size {
                        black_box(buffer.pop_unchecked());
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Memory Overhead
fn bench_ring_buffer_memory(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer_memory");

    // 測量不同容量下的內存佔用
    for capacity in [256, 1024, 4096].iter() {
        group.bench_with_input(
            BenchmarkId::new("standard_alloc", capacity),
            capacity,
            |b, &cap| {
                b.iter(|| {
                    let buffer = SpscRingBuffer::<MarketEvent>::new(cap);
                    black_box(buffer)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("ultra_alloc", capacity),
            capacity,
            |b, &cap| {
                b.iter(|| {
                    let buffer = UltraRingBuffer::<MarketEvent>::new(cap);
                    black_box(buffer)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_ring_buffer_push,
    bench_ring_buffer_pop,
    bench_ring_buffer_roundtrip,
    bench_ring_buffer_batch,
    bench_ring_buffer_memory,
);

criterion_main!(benches);
