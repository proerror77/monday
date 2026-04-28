use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use engine::binance_md::{
    parse_depth_update, LatencyTrace, MarketDataLane, ParsedDepthUpdate, SignalRules,
};

const BTCUSDT_ID: u32 = 1;
const RAW_DEPTH: &[u8] = br#"{
    "e":"depthUpdate",
    "E":1777353243014379,
    "s":"BTCUSDT",
    "U":100,
    "u":100,
    "b":[["76942.790000","1.609380"],["76942.780000","0.019580"]],
    "a":[["76942.800000","0.671330"],["76942.810000","0.000630"]]
}"#;

fn prepared_update() -> ParsedDepthUpdate {
    parse_depth_update(RAW_DEPTH, BTCUSDT_ID, 1_000).expect("valid depth fixture")
}

fn prepared_lane() -> MarketDataLane<50> {
    let mut lane = MarketDataLane::<50>::new(BTCUSDT_ID, SignalRules::default());
    lane.load_snapshot_for_replay(
        99,
        &[
            (76_942_780_000, 19_580),
            (76_942_770_000, 10_000),
            (76_942_760_000, 10_000),
        ],
        &[
            (76_942_800_000, 671_330),
            (76_942_810_000, 630),
            (76_942_820_000, 10_000),
        ],
        1_000,
    );
    lane
}

fn bench_parse_depth(c: &mut Criterion) {
    c.bench_function("binance_md/parse_depth_update", |b| {
        b.iter(|| {
            black_box(parse_depth_update(
                black_box(RAW_DEPTH),
                black_box(BTCUSDT_ID),
                black_box(1_000),
            ))
        })
    });
}

fn bench_book_feature_signal_fast(c: &mut Criterion) {
    c.bench_function("binance_md/book_feature_signal_fast", |b| {
        b.iter_batched(
            || (prepared_lane(), prepared_update()),
            |(mut lane, update)| {
                let mut tick = 1_000_i64;
                black_box(lane.process_depth_update_fast_with_clock(
                    black_box(&update),
                    LatencyTrace {
                        recv_ns: 0,
                        parse_done_ns: tick,
                        ..LatencyTrace::default()
                    },
                    &mut || {
                        tick += 1_000;
                        tick
                    },
                ))
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_full_no_replay(c: &mut Criterion) {
    c.bench_function("binance_md/full_parse_book_feature_signal_no_replay", |b| {
        b.iter_batched(
            prepared_lane,
            |mut lane| {
                let update =
                    parse_depth_update(black_box(RAW_DEPTH), BTCUSDT_ID, 1_000).expect("valid");
                let mut tick = 1_000_i64;
                black_box(lane.process_depth_update_fast_with_clock(
                    black_box(&update),
                    LatencyTrace {
                        recv_ns: 0,
                        parse_done_ns: tick,
                        ..LatencyTrace::default()
                    },
                    &mut || {
                        tick += 1_000;
                        tick
                    },
                ))
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    benches,
    bench_parse_depth,
    bench_book_feature_signal_fast,
    bench_full_no_replay
);
criterion_main!(benches);
