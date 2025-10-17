use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce};
use ports::{AccountView, ExecutionEvent, MarketEvent, OrderIntent, Strategy};

#[derive(Default)]
struct MockStrategy {
    id: String,
}

impl MockStrategy {
    fn new(id: usize) -> Self {
        Self {
            id: format!("bench_strategy_{id}"),
        }
    }
}

impl Strategy for MockStrategy {
    fn on_market_event(
        &mut self,
        _event: &MarketEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        vec![OrderIntent {
            symbol: Symbol::from("BENCH:BTCUSDT"),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001).expect("valid quantity"),
            order_type: OrderType::Market,
            price: Some(Price::from_f64(30_000.0).expect("valid price")),
            time_in_force: TimeInForce::IOC,
            strategy_id: self.id.clone(),
            target_venue: None,
        }]
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn name(&self) -> &str {
        &self.id
    }
}

fn bench_strategy_dispatch_box_dyn(c: &mut Criterion) {
    let mut strategies: Vec<Box<dyn Strategy>> = (0..32)
        .map(|idx| Box::new(MockStrategy::new(idx)) as Box<dyn Strategy>)
        .collect();

    let market_event = MarketEvent::Disconnect {
        reason: "benchmark".to_string(),
    };
    let account = AccountView::default();

    c.bench_function("StrategyDispatch/BoxDyn32", |b| {
        b.iter(|| {
            for strategy in strategies.iter_mut() {
                black_box(strategy.on_market_event(&market_event, &account));
            }
        });
    });
}

criterion_group!(benches, bench_strategy_dispatch_box_dyn);
criterion_main!(benches);
