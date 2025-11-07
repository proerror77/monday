//! JSON 解析性能基準測試
//!
//! 比較 serde_json 和 simd-json 的解析性能
//!
//! 運行方式：
//! ```bash
//! # 測試默認 (serde_json)
//! cargo bench --bench json_parsing
//!
//! # 測試 SIMD (simd-json)
//! cargo bench --bench json_parsing --features json-simd
//! ```

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use data_adapter_binance::MessageConverter;

/// 真實的 Binance 深度更新消息樣本
const DEPTH_UPDATE_JSON: &str = r#"{
    "e": "depthUpdate",
    "E": 1721810005123,
    "s": "BTCUSDT",
    "U": 1000000,
    "u": 1000001,
    "b": [
        ["67188.10", "0.12345"],
        ["67187.50", "0.23456"],
        ["67187.00", "0.34567"],
        ["67186.50", "0.45678"],
        ["67186.00", "0.56789"]
    ],
    "a": [
        ["67189.50", "0.25000"],
        ["67190.00", "0.35000"],
        ["67190.50", "0.45000"],
        ["67191.00", "0.55000"],
        ["67191.50", "0.65000"]
    ]
}"#;

/// 真實的 Binance 交易事件樣本
const TRADE_EVENT_JSON: &str = r#"{
    "e": "trade",
    "E": 1721810005321,
    "s": "BTCUSDT",
    "t": 12345678,
    "p": "67188.40",
    "q": "0.02134",
    "b": 1111111,
    "a": 2222222,
    "T": 1721810005345,
    "m": false,
    "M": false
}"#;

/// 真實的 Binance K線事件樣本
const KLINE_EVENT_JSON: &str = r#"{
    "e": "kline",
    "E": 1721810005567,
    "s": "ETHUSDT",
    "k": {
        "t": 1721810000000,
        "T": 1721810059999,
        "s": "ETHUSDT",
        "i": "1m",
        "f": 100000,
        "L": 100099,
        "o": "3456.78",
        "c": "3457.89",
        "h": "3458.90",
        "l": "3455.67",
        "v": "123.456",
        "n": 100,
        "x": false,
        "q": "427890.12",
        "V": "61.728",
        "Q": "213445.06",
        "B": "0"
    }
}"#;

/// 基準測試：解析深度更新消息
fn bench_parse_depth_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_depth_update");
    group.throughput(Throughput::Bytes(DEPTH_UPDATE_JSON.len() as u64));

    group.bench_function("parse", |b| {
        b.iter(|| {
            let result = MessageConverter::parse_stream_message(black_box(DEPTH_UPDATE_JSON));
            black_box(result)
        })
    });

    group.finish();
}

/// 基準測試：解析交易事件消息
fn bench_parse_trade_event(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_trade_event");
    group.throughput(Throughput::Bytes(TRADE_EVENT_JSON.len() as u64));

    group.bench_function("parse", |b| {
        b.iter(|| {
            let result = MessageConverter::parse_stream_message(black_box(TRADE_EVENT_JSON));
            black_box(result)
        })
    });

    group.finish();
}

/// 基準測試：解析 K線事件消息
fn bench_parse_kline_event(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_kline_event");
    group.throughput(Throughput::Bytes(KLINE_EVENT_JSON.len() as u64));

    group.bench_function("parse", |b| {
        b.iter(|| {
            let result = MessageConverter::parse_stream_message(black_box(KLINE_EVENT_JSON));
            black_box(result)
        })
    });

    group.finish();
}

/// 基準測試：批量解析混合消息
fn bench_parse_mixed_batch(c: &mut Criterion) {
    let messages = vec![DEPTH_UPDATE_JSON, TRADE_EVENT_JSON, KLINE_EVENT_JSON];
    let total_bytes: usize = messages.iter().map(|m| m.len()).sum();

    let mut group = c.benchmark_group("parse_mixed_batch");
    group.throughput(Throughput::Bytes(total_bytes as u64));

    group.bench_function("parse_all", |b| {
        b.iter(|| {
            for msg in &messages {
                let result = MessageConverter::parse_stream_message(black_box(msg));
                black_box(result);
            }
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_parse_depth_update,
    bench_parse_trade_event,
    bench_parse_kline_event,
    bench_parse_mixed_batch
);
criterion_main!(benches);
