use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine::{aggregation::MarketView, aggregation::TopNSnapshot, Engine, EngineConfig};
use ports::{BookLevel, MarketSnapshot};

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
    };

    c.bench_function("TopN::update_from_snapshot+mid", |b| {
        b.iter(|| {
            let mut topn = TopNSnapshot::new(symbol.clone(), 10);
            topn.update_from_snapshot(black_box(&snap));
            black_box(topn.get_mid_price_fast());
        })
    });
}

// 將新的基準加入 group（保持現有次序在前）
criterion_group!(topn_benches, bench_topn_aggregation);
