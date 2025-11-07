use hft_core::{OrderType, Quantity, Side, Symbol, TimeInForce};
use ports::{AccountView, MarketEvent, OrderIntent, Strategy, VenueScope};
use tracing::info;

#[derive(Debug, Clone)]
pub struct ImbalanceParams {
    pub obi_threshold: f64, // e.g., 0.2 means 20% imbalance
    pub lot: f64,           // order size
    pub top_levels: usize,  // how many levels to aggregate for OBI
}

impl Default for ImbalanceParams {
    fn default() -> Self {
        Self {
            obi_threshold: 0.2,
            lot: 0.01,
            top_levels: 5,
        }
    }
}

pub struct ImbalanceStrategy {
    symbol: Symbol,
    params: ImbalanceParams,
    name: String,
}

impl ImbalanceStrategy {
    /// 創建新的失衡策略（舊版，保持向後兼容性）
    pub fn new(symbol: Symbol, params: Option<ImbalanceParams>) -> Self {
        let strategy_name = format!("imbalance_{}", symbol.as_str());
        Self::with_name(symbol, params, strategy_name)
    }

    /// 🔥 Phase 1.4: 創建帶穩定名稱的失衡策略
    pub fn with_name(
        symbol: Symbol,
        params: Option<ImbalanceParams>,
        strategy_name: String,
    ) -> Self {
        Self {
            name: strategy_name,
            symbol,
            params: params.unwrap_or_default(),
        }
    }

    fn calc_obi(&self, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> f64 {
        let n = self.params.top_levels;
        let bid_sum: f64 = bids.iter().take(n).map(|(_, q)| *q).sum();
        let ask_sum: f64 = asks.iter().take(n).map(|(_, q)| *q).sum();
        let total = bid_sum + ask_sum;
        if total > 0.0 {
            (bid_sum - ask_sum) / total
        } else {
            0.0
        }
    }
}

impl Strategy for ImbalanceStrategy {
    fn id(&self) -> &str {
        &self.name
    }
    fn venue_scope(&self) -> VenueScope {
        VenueScope::Single
    }
    fn on_market_event(&mut self, event: &MarketEvent, _account: &AccountView) -> Vec<OrderIntent> {
        let mut intents = Vec::new();
        let sym = &self.symbol;
        match event {
            MarketEvent::Snapshot(s) if &s.symbol == sym => {
                // Convert BookLevels -> (price, qty)
                let bids: Vec<(f64, f64)> = s
                    .bids
                    .iter()
                    .map(|l| {
                        (
                            l.price.to_f64().unwrap_or(0.0),
                            l.quantity.to_f64().unwrap_or(0.0),
                        )
                    })
                    .collect();
                let asks: Vec<(f64, f64)> = s
                    .asks
                    .iter()
                    .map(|l| {
                        (
                            l.price.to_f64().unwrap_or(0.0),
                            l.quantity.to_f64().unwrap_or(0.0),
                        )
                    })
                    .collect();
                let obi = self.calc_obi(&bids, &asks);
                let qty = Quantity::from_f64(self.params.lot)
                    .unwrap_or_else(|_| Quantity::from_f64(0.01).unwrap());
                if obi > self.params.obi_threshold {
                    // Buy when bid side is heavy
                    intents.push(OrderIntent {
                        symbol: sym.clone(),
                        side: Side::Buy,
                        quantity: qty,
                        order_type: OrderType::Market,
                        price: None,
                        time_in_force: TimeInForce::IOC,
                        strategy_id: self.name.clone(),
                        target_venue: s.source_venue, // 單場策略：綁定來源場館
                    });
                    info!(
                        "OBI {:.3} > {:.3}, BUY {}",
                        obi,
                        self.params.obi_threshold,
                        sym.as_str()
                    );
                } else if obi < -self.params.obi_threshold {
                    intents.push(OrderIntent {
                        symbol: sym.clone(),
                        side: Side::Sell,
                        quantity: qty,
                        order_type: OrderType::Market,
                        price: None,
                        time_in_force: TimeInForce::IOC,
                        strategy_id: self.name.clone(),
                        target_venue: s.source_venue,
                    });
                    info!(
                        "OBI {:.3} < -{:.3}, SELL {}",
                        obi,
                        self.params.obi_threshold,
                        sym.as_str()
                    );
                }
            }
            _ => {}
        }
        intents
    }

    fn on_execution_event(
        &mut self,
        _event: &ports::ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn name(&self) -> &str {
        &self.name
    }
}

pub fn create_imbalance_strategy(
    symbol: Symbol,
    params: Option<ImbalanceParams>,
) -> ImbalanceStrategy {
    ImbalanceStrategy::new(symbol, params)
}
