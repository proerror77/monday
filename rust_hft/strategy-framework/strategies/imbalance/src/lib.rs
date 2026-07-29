use hft_core::{FixedQuantity, OrderType, Quantity, Side, Symbol, TimeInForce, VenueId};
use ports::{AccountView, MarketEvent, OrderIntent, Strategy, StrategyContext, VenueScope};
use tracing::debug;

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
    signal_state: SignalState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalState {
    Neutral,
    Buy,
    Sell,
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
            signal_state: SignalState::Neutral,
        }
    }

    #[inline]
    fn calc_obi_from_sums(bid_sum: f64, ask_sum: f64) -> f64 {
        let total = bid_sum + ask_sum;
        if total > 0.0 {
            (bid_sum - ask_sum) / total
        } else {
            0.0
        }
    }

    #[inline]
    fn calc_fixed_obi(&self, bids: &[FixedQuantity], asks: &[FixedQuantity]) -> f64 {
        let levels = self.params.top_levels;
        let bid_sum: i128 = bids
            .iter()
            .take(levels)
            .map(|quantity| i128::from(quantity.raw()))
            .sum();
        let ask_sum: i128 = asks
            .iter()
            .take(levels)
            .map(|quantity| i128::from(quantity.raw()))
            .sum();
        Self::calc_obi_from_sums(bid_sum as f64, ask_sum as f64)
    }

    fn intent_for_obi(&mut self, obi: f64, venue: Option<VenueId>) -> Vec<OrderIntent> {
        let next_state = if obi > self.params.obi_threshold {
            SignalState::Buy
        } else if obi < -self.params.obi_threshold {
            SignalState::Sell
        } else {
            self.signal_state = SignalState::Neutral;
            return Vec::new();
        };
        if next_state == self.signal_state {
            return Vec::new();
        }
        let side = match next_state {
            SignalState::Buy => Side::Buy,
            SignalState::Sell => Side::Sell,
            SignalState::Neutral => unreachable!(),
        };
        let Ok(quantity) = Quantity::from_f64(self.params.lot) else {
            return Vec::new();
        };

        debug!(
            strategy = %self.name,
            symbol = %self.symbol.as_str(),
            obi,
            threshold = self.params.obi_threshold,
            ?side,
            ?venue,
            "LOB imbalance signal"
        );
        self.signal_state = next_state;
        vec![OrderIntent {
            symbol: self.symbol.clone(),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: hft_core::ProductType::Spot,
            compliance_context: hft_core::ComplianceContext::default(),
            side,
            quantity,
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: self.name.clone(),
            target_venue: venue,
        }]
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
        match event {
            MarketEvent::Snapshot(snapshot) if snapshot.symbol == self.symbol => {
                let bid_sum = snapshot
                    .bids
                    .iter()
                    .take(self.params.top_levels)
                    .filter_map(|level| level.quantity.to_f64())
                    .sum();
                let ask_sum = snapshot
                    .asks
                    .iter()
                    .take(self.params.top_levels)
                    .filter_map(|level| level.quantity.to_f64())
                    .sum();
                self.intent_for_obi(
                    Self::calc_obi_from_sums(bid_sum, ask_sum),
                    snapshot.source_venue,
                )
            }
            _ => Vec::new(),
        }
    }

    fn on_market_event_with_context(
        &mut self,
        event: &MarketEvent,
        context: &StrategyContext<'_>,
    ) -> Vec<OrderIntent> {
        let is_matching_book_event = match event {
            MarketEvent::Snapshot(snapshot) => snapshot.symbol == self.symbol,
            MarketEvent::Update(update) => update.symbol == self.symbol,
            MarketEvent::Quote(quote) => quote.symbol == self.symbol,
            _ => false,
        };
        if !is_matching_book_event {
            return Vec::new();
        }
        let Some(book) = context.book.filter(|book| book.symbol == &self.symbol) else {
            return Vec::new();
        };

        self.intent_for_obi(
            self.calc_fixed_obi(book.bid_quantities, book.ask_quantities),
            Some(book.venue),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{Price, VenueId};
    use ports::{BookLevel, MarketSnapshot};

    fn snapshot(bid_quantity: f64, ask_quantity: f64) -> MarketEvent {
        MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1,
            bids: vec![BookLevel {
                price: Price::from_f64(100.0).unwrap(),
                quantity: Quantity::from_f64(bid_quantity).unwrap(),
            }],
            asks: vec![BookLevel {
                price: Price::from_f64(101.0).unwrap(),
                quantity: Quantity::from_f64(ask_quantity).unwrap(),
            }],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        })
    }

    #[test]
    fn persistent_imbalance_emits_only_on_signal_transition() {
        let mut strategy = ImbalanceStrategy::new(
            Symbol::new("BTCUSDT"),
            Some(ImbalanceParams {
                obi_threshold: 0.2,
                lot: 0.01,
                top_levels: 1,
            }),
        );
        let account = AccountView::default();

        assert_eq!(
            strategy
                .on_market_event(&snapshot(3.0, 1.0), &account)
                .len(),
            1
        );
        assert!(strategy
            .on_market_event(&snapshot(4.0, 1.0), &account)
            .is_empty());
        assert!(strategy
            .on_market_event(&snapshot(1.0, 1.0), &account)
            .is_empty());
        assert_eq!(
            strategy
                .on_market_event(&snapshot(3.0, 1.0), &account)
                .len(),
            1
        );
        assert_eq!(
            strategy.on_market_event(&snapshot(1.0, 3.0), &account)[0].side,
            Side::Sell
        );
    }
}
