use criterion::{black_box, criterion_group, criterion_main, Criterion};
use data_adapter_bitget::{
    parse_bitget_orderbook_frame, parse_bitget_orderbook_snapshot, parse_bitget_trade_event,
    parse_bitget_trade_frame, parse_bitget_ws_envelope,
};

const ORDERBOOK_FRAME: &str = r#"{
    "action":"snapshot",
    "arg":{"instType":"SPOT","channel":"books15","instId":"BTCUSDT"},
    "data":[{
        "asks":[["67189.5","0.25","1"],["67190.0","0.30","1"],["67190.5","0.35","1"]],
        "bids":[["67188.1","0.12","1"],["67187.5","0.15","1"],["67187.0","0.18","1"]],
        "ts":"1777353243014"
    }]
}"#;

const TRADE_FRAME: &str = r#"{
    "arg":{"instType":"SPOT","channel":"trade","instId":"BTCUSDT"},
    "data":[{
        "px":"67188.1",
        "sz":"0.12",
        "side":"buy",
        "ts":"1777353243014",
        "tradeId":"1777353243014-1"
    }]
}"#;

fn bench_envelope(c: &mut Criterion) {
    c.bench_function("bitget_md/parse_ws_envelope", |b| {
        b.iter(|| black_box(parse_bitget_ws_envelope(black_box(ORDERBOOK_FRAME))))
    });
}

fn bench_orderbook(c: &mut Criterion) {
    c.bench_function("bitget_md/parse_orderbook_frame_borrowed", |b| {
        b.iter(|| black_box(parse_bitget_orderbook_frame(black_box(ORDERBOOK_FRAME))))
    });
}

fn bench_trade(c: &mut Criterion) {
    c.bench_function("bitget_md/parse_trade_frame_borrowed", |b| {
        b.iter(|| black_box(parse_bitget_trade_frame(black_box(TRADE_FRAME))))
    });
}

fn bench_orderbook_snapshot_event(c: &mut Criterion) {
    c.bench_function("bitget_md/full_orderbook_snapshot_event", |b| {
        b.iter(|| black_box(parse_bitget_orderbook_snapshot(black_box(ORDERBOOK_FRAME))))
    });
}

fn bench_trade_event(c: &mut Criterion) {
    c.bench_function("bitget_md/full_trade_event", |b| {
        b.iter(|| black_box(parse_bitget_trade_event(black_box(TRADE_FRAME))))
    });
}

criterion_group!(
    benches,
    bench_envelope,
    bench_orderbook,
    bench_trade,
    bench_orderbook_snapshot_event,
    bench_trade_event
);
criterion_main!(benches);
