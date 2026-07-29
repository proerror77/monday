use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine::{
    aggregation::{AggregationEngine, MarketView, TopNSnapshot},
    Engine, EngineConfig,
};
use hft_core::{Quantity, Symbol, VenueId};
use ports::{BookLevel, BookUpdate, MarketEvent, MarketSnapshot, TopOfBook};

fn bench_engine_tick(c: &mut Criterion) {
    let mut engine = Engine::new(EngineConfig::default());
    c.bench_function("Engine::tick_empty", |b| {
        b.iter(|| {
            // 空負載 tick（無事件）
            let _ = black_box(engine.tick());
        })
    });
}

fn bench_market_view_read(c: &mut Criterion) {
    let engine = Engine::new(EngineConfig::default());
    c.bench_function("MarketView::clone_read", |b| {
        b.iter(|| {
            let mv: std::sync::Arc<MarketView> = engine.get_market_view();
            black_box(mv);
        })
    });
}

criterion_group!(benches, bench_engine_tick, bench_market_view_read);
criterion_main!(benches, topn_benches);

fn bench_topn_aggregation(c: &mut Criterion) {
    // 構造一個含 10 檔位的快照
    let symbol = hft_core::Symbol::new("BTC_USDT");
    let bids: Vec<BookLevel> = (0..10)
        .map(|i| BookLevel::new_unchecked(50_000.0 - i as f64, 1.0 + i as f64))
        .collect();
    let asks: Vec<BookLevel> = (0..10)
        .map(|i| BookLevel::new_unchecked(50_001.0 + i as f64, 1.0 + i as f64))
        .collect();
    let snap = MarketSnapshot {
        symbol: symbol.clone(),
        timestamp: 1_700_000_000,
        bids,
        asks,
        sequence: 42,
        source_venue: None,
        timestamps: Default::default(),
    };

    c.bench_function("TopN::update_from_snapshot+mid", |b| {
        b.iter(|| {
            let mut topn = TopNSnapshot::new(symbol.clone(), 10);
            topn.update_from_snapshot(black_box(&snap));
            black_box(topn.get_mid_price_fast());
        })
    });
}

fn benchmark_snapshot(symbol: &Symbol) -> MarketSnapshot {
    MarketSnapshot {
        symbol: symbol.clone(),
        timestamp: 1,
        bids: (0..20)
            .map(|level| {
                BookLevel::new_unchecked(50_000.0 - f64::from(level), 1.0 + f64::from(level))
            })
            .collect(),
        asks: (0..20)
            .map(|level| {
                BookLevel::new_unchecked(50_001.0 + f64::from(level), 1.0 + f64::from(level))
            })
            .collect(),
        sequence: 1,
        source_venue: Some(VenueId::BINANCE),
        timestamps: Default::default(),
    }
}

fn bench_canonical_market_events(c: &mut Criterion) {
    let symbol = Symbol::new("BTCUSDT");

    let mut depth_engine = AggregationEngine::with_config(20, u64::MAX);
    let mut depth_output = Vec::with_capacity(2);
    depth_engine
        .handle_event_into(
            MarketEvent::Snapshot(benchmark_snapshot(&symbol)),
            &mut depth_output,
        )
        .expect("benchmark snapshot");
    let mut depth_sequence = 2_u64;
    c.bench_function("Aggregation::canonical_delta_to_top20", |b| {
        b.iter(|| {
            depth_output.clear();
            let sequence = depth_sequence;
            depth_sequence += 1;
            depth_engine
                .handle_event_into(
                    MarketEvent::Update(BookUpdate {
                        symbol: symbol.clone(),
                        timestamp: sequence,
                        bids: vec![BookLevel::new_unchecked(50_000.0, 2.0)],
                        asks: vec![BookLevel::new_unchecked(50_001.0, 2.0)],
                        first_sequence: Some(sequence),
                        sequence,
                        is_snapshot: false,
                        source_venue: Some(VenueId::BINANCE),
                        timestamps: Default::default(),
                    }),
                    &mut depth_output,
                )
                .expect("benchmark depth update");
            black_box(&depth_output);
        });
    });

    let mut quote_engine = AggregationEngine::with_config(20, u64::MAX);
    let mut quote_output = Vec::with_capacity(2);
    quote_engine
        .handle_event_into(
            MarketEvent::Snapshot(benchmark_snapshot(&symbol)),
            &mut quote_output,
        )
        .expect("benchmark snapshot");
    let mut quote_sequence = 2_u64;
    c.bench_function("Aggregation::realtime_bbo_overlay_to_top20", |b| {
        b.iter(|| {
            quote_output.clear();
            let sequence = quote_sequence;
            quote_sequence += 1;
            quote_engine
                .handle_event_into(
                    MarketEvent::Quote(TopOfBook {
                        symbol: symbol.clone(),
                        timestamp: sequence,
                        sequence,
                        bid: BookLevel {
                            price: hft_core::Price::from_f64(50_000.0).expect("valid price"),
                            quantity: Quantity::from_f64(2.0).expect("valid quantity"),
                        },
                        ask: BookLevel {
                            price: hft_core::Price::from_f64(50_001.0).expect("valid price"),
                            quantity: Quantity::from_f64(2.0).expect("valid quantity"),
                        },
                        source_venue: Some(VenueId::BINANCE),
                        timestamps: Default::default(),
                    }),
                    &mut quote_output,
                )
                .expect("benchmark quote");
            black_box(&quote_output);
        });
    });
}

// 將新的基準加入 group（保持現有次序在前）
criterion_group!(
    topn_benches,
    bench_topn_aggregation,
    bench_canonical_market_events
);
