use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine::{aggregation::MarketView, Engine, EngineConfig};

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
criterion_main!(benches);

