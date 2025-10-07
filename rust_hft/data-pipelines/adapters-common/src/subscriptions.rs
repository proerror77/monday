use hft-core::Symbol;

#[derive(Debug, Clone)]
pub enum SubscriptionKind {
    OrderBook,
    Trades,
    Ticker,
    // Extend as needed per exchange
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub kind: SubscriptionKind,
    pub symbol: Symbol,
}

#[derive(Default)]
pub struct SubscriptionBuilder {
    items: Vec<Subscription>,
}

impl SubscriptionBuilder {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn orderbook(mut self, symbol: Symbol) -> Self {
        self.items.push(Subscription { kind: SubscriptionKind::OrderBook, symbol });
        self
    }

    pub fn trades(mut self, symbol: Symbol) -> Self {
        self.items.push(Subscription { kind: SubscriptionKind::Trades, symbol });
        self
    }

    pub fn ticker(mut self, symbol: Symbol) -> Self {
        self.items.push(Subscription { kind: SubscriptionKind::Ticker, symbol });
        self
    }

    pub fn build(self) -> Vec<Subscription> { self.items }
}

